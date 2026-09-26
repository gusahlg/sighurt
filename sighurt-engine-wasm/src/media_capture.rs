//! Host-side media capture: camera, microphone, and screen (with permission prompts).
//!
//! Guests call [`register_media_capture_functions`] imports from the `oxide` module. Access is
//! gated per origin through [`crate::permissions`]: the first call returns
//! [`PERMISSION_PENDING`] while the in-browser prompt is showing, and the guest retries on a
//! later frame. Native OS prompts (camera / microphone / screen recording) may appear in
//! addition once the in-browser grant is given.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use nokhwa::pixel_format::RgbFormat;
use nokhwa::utils::{CameraIndex, RequestedFormat, RequestedFormatType};
use nokhwa::Camera;
use wasmtime::{Caller, Linker};

use crate::capabilities::{
    console_log, pack, permission_gate, write_guest, write_prefix, ConsoleEntry, ConsoleLevel,
    HostState,
};
use crate::permissions::PermissionKind;

const MIC_RING_CAP: usize = 96_000;

/// Shared capture state for a tab (camera stream, mic ring buffer, counters for pipeline stats).
#[derive(Default)]
pub struct MediaCaptureState {
    camera: Option<Camera>,
    last_frame_w: u32,
    last_frame_h: u32,
    camera_frames: u64,
    microphone: Option<MicrophoneInput>,
    screen_w: u32,
    screen_h: u32,
}

impl MediaCaptureState {
    /// Stops any live camera/microphone streams and resets all counters.
    ///
    /// Called when the tab navigates to a different origin so a new app can never
    /// read frames or samples from devices the previous origin opened.
    pub fn reset(&mut self) {
        if let Some(mut cam) = self.camera.take() {
            let _ = cam.stop_stream();
        }
        *self = Self::default();
    }
}

struct MicrophoneInput {
    /// Kept alive: dropping it stops recording.
    _stream: cpal::Stream,
    buffer: Arc<Mutex<VecDeque<f32>>>,
    sample_rate: u32,
}

/// Mixes interleaved samples of `channels` channels down to mono and appends them to the ring,
/// dropping the oldest samples beyond its capacity.
fn push_mono<T: Copy>(
    data: &[T],
    channels: usize,
    ring: &Mutex<VecDeque<f32>>,
    to_f32: impl Fn(T) -> f32,
) {
    let channels = channels.max(1);
    let mut ring = ring.lock().unwrap();
    for frame in data.chunks_exact(channels) {
        let mono = frame.iter().map(|&s| to_f32(s)).sum::<f32>() / channels as f32;
        if ring.len() >= MIC_RING_CAP {
            ring.pop_front();
        }
        ring.push_back(mono);
    }
}

fn open_microphone(console: &Arc<Mutex<Vec<ConsoleEntry>>>) -> Result<MicrophoneInput, i32> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| log_err(console, -2, "[MIC] No input device".to_string()))?;
    let supported = match device.default_input_config() {
        Ok(c) => c,
        Err(e) => {
            return Err(log_err(console, -3, format!("[MIC] Config: {e}")));
        }
    };
    let sample_format = supported.sample_format();
    let config: cpal::StreamConfig = supported.clone().into();
    let channels = config.channels as usize;
    let ring = Arc::new(Mutex::new(VecDeque::with_capacity(MIC_RING_CAP)));
    let ring2 = ring.clone();
    let console_err = console.clone();
    let err_fn = move |e| {
        console_log(
            &console_err,
            ConsoleLevel::Warn,
            format!("[MIC] Stream error: {e}"),
        );
    };

    let stream = match sample_format {
        SampleFormat::F32 => device.build_input_stream(
            &config,
            move |data: &[f32], _| push_mono(data, channels, &ring2, |s| s),
            err_fn,
            None,
        ),
        SampleFormat::I16 => device.build_input_stream(
            &config,
            move |data: &[i16], _| push_mono(data, channels, &ring2, |s| f32::from(s) / 32768.0),
            err_fn,
            None,
        ),
        SampleFormat::U16 => device.build_input_stream(
            &config,
            move |data: &[u16], _| {
                push_mono(data, channels, &ring2, |s| {
                    (f32::from(s) - 32768.0) / 32768.0
                })
            },
            err_fn,
            None,
        ),
        other => {
            return Err(log_err(
                console,
                -3,
                format!("[MIC] Unsupported sample format {other:?}"),
            ));
        }
    };
    let stream = match stream {
        Ok(s) => s,
        Err(e) => {
            return Err(log_err(console, -3, format!("[MIC] Build stream: {e}")));
        }
    };
    if let Err(e) = stream.play() {
        return Err(log_err(console, -3, format!("[MIC] Play: {e}")));
    }
    let sample_rate = supported.sample_rate();
    Ok(MicrophoneInput {
        _stream: stream,
        buffer: ring,
        sample_rate,
    })
}

fn log_err(console: &Mutex<Vec<ConsoleEntry>>, code: i32, msg: String) -> i32 {
    console_log(console, ConsoleLevel::Warn, msg);
    code
}

/// Opens the first camera, at up to 1280 × 720.
fn open_camera(console: &Mutex<Vec<ConsoleEntry>>) -> Result<Camera, i32> {
    match nokhwa::query(nokhwa::utils::ApiBackend::Auto) {
        Err(e) => return Err(log_err(console, -2, format!("[CAMERA] No cameras: {e}"))),
        Ok(cameras) if cameras.is_empty() => {
            return Err(log_err(console, -2, "[CAMERA] No cameras found".into()))
        }
        Ok(_) => {}
    }
    let format = RequestedFormat::new::<RgbFormat>(RequestedFormatType::HighestResolution(
        nokhwa::utils::Resolution::new(1280, 720),
    ));
    let mut camera = Camera::new(CameraIndex::Index(0), format)
        .map_err(|e| log_err(console, -3, format!("[CAMERA] Open failed: {e}")))?;
    camera
        .open_stream()
        .map_err(|e| log_err(console, -3, format!("[CAMERA] Stream: {e}")))?;
    Ok(camera)
}

/// The next camera frame as RGBA, or `None` (logged) when there is none.
fn camera_frame(state: &HostState) -> Option<Vec<u8>> {
    let mut capture = state.media_capture.lock().unwrap();
    let frame = capture.camera.as_mut()?.frame();
    let image = frame
        .and_then(|buffer| buffer.decode_image::<RgbFormat>())
        .map_err(|e| state.log(ConsoleLevel::Warn, format!("[CAMERA] Frame: {e}")))
        .ok()?;
    capture.last_frame_w = image.width();
    capture.last_frame_h = image.height();
    capture.camera_frames = capture.camera_frames.saturating_add(1);
    Some(
        image
            .pixels()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
    )
}

/// A screenshot of the first display as RGBA, or the guest's error code (logged).
fn screen_capture(state: &HostState) -> Result<Vec<u8>, i32> {
    let screens = screenshots::Screen::all()
        .map_err(|e| log_err(&state.console, -2, format!("[SCREEN] Enumerate: {e}")))?;
    let screen = screens
        .first()
        .ok_or_else(|| log_err(&state.console, -2, "[SCREEN] No displays".into()))?;
    let image = screen
        .capture()
        .map_err(|e| log_err(&state.console, -3, format!("[SCREEN] Capture: {e}")))?;
    let mut capture = state.media_capture.lock().unwrap();
    capture.screen_w = image.width();
    capture.screen_h = image.height();
    Ok(image.into_raw())
}

/// Register `api_camera_*`, `api_microphone_*`, `api_screen_capture`, and `api_media_pipeline_stats`.
pub fn register_media_capture_functions(linker: &mut Linker<HostState>) -> Result<()> {
    fn capture<R>(
        caller: &Caller<'_, HostState>,
        f: impl FnOnce(&mut MediaCaptureState) -> R,
    ) -> R {
        f(&mut caller.data().media_capture.lock().unwrap())
    }

    linker.func_wrap(
        "oxide",
        "api_camera_open",
        |caller: Caller<'_, HostState>| -> i32 {
            if let Some(code) = permission_gate(caller.data(), PermissionKind::Camera) {
                return code;
            }
            let mut capture = caller.data().media_capture.lock().unwrap();
            if let Some(mut camera) = capture.camera.take() {
                let _ = camera.stop_stream();
            }
            match open_camera(&caller.data().console) {
                Ok(camera) => {
                    capture.camera = Some(camera);
                    0
                }
                Err(code) => code,
            }
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_camera_close",
        |caller: Caller<'_, HostState>| {
            if let Some(mut camera) = capture(&caller, |c| c.camera.take()) {
                let _ = camera.stop_stream();
            }
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_camera_capture_frame",
        |mut caller: Caller<'_, HostState>, out_ptr: u32, out_cap: u32| -> u32 {
            match camera_frame(caller.data()) {
                Some(rgba) => write_prefix(&mut caller, out_ptr, out_cap, &rgba).unwrap_or(0),
                None => 0,
            }
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_camera_frame_dimensions",
        |caller: Caller<'_, HostState>| -> u64 {
            capture(&caller, |c| pack(c.last_frame_w, c.last_frame_h))
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_microphone_open",
        |caller: Caller<'_, HostState>| -> i32 {
            if let Some(code) = permission_gate(caller.data(), PermissionKind::Microphone) {
                return code;
            }
            let mut capture = caller.data().media_capture.lock().unwrap();
            capture.microphone = None;
            match open_microphone(&caller.data().console) {
                Ok(microphone) => {
                    capture.microphone = Some(microphone);
                    0
                }
                Err(code) => code,
            }
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_microphone_close",
        |caller: Caller<'_, HostState>| capture(&caller, |c| c.microphone = None),
    )?;

    linker.func_wrap(
        "oxide",
        "api_microphone_sample_rate",
        |caller: Caller<'_, HostState>| -> u32 {
            capture(&caller, |c| {
                c.microphone.as_ref().map_or(0, |m| m.sample_rate)
            })
        },
    )?;

    // Writes up to `max_samples` mono f32 samples; returns how many.
    linker.func_wrap(
        "oxide",
        "api_microphone_read_samples",
        |mut caller: Caller<'_, HostState>, out_ptr: u32, max_samples: u32| -> u32 {
            let samples: Vec<u8> = capture(&caller, |c| {
                let Some(microphone) = &c.microphone else {
                    return Vec::new();
                };
                let mut ring = microphone.buffer.lock().unwrap();
                let take = (max_samples as usize).min(ring.len());
                ring.drain(..take).flat_map(f32::to_le_bytes).collect()
            });
            if !write_guest(&mut caller, out_ptr, &samples) {
                return 0;
            }
            (samples.len() / 4) as u32
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_screen_capture",
        |mut caller: Caller<'_, HostState>, out_ptr: u32, out_cap: u32| -> i32 {
            // The OS may additionally ask for screen-recording permission once granted here.
            if let Some(code) = permission_gate(caller.data(), PermissionKind::ScreenCapture) {
                return code;
            }
            match screen_capture(caller.data()) {
                Ok(rgba) => {
                    write_prefix(&mut caller, out_ptr, out_cap, &rgba).map_or(-4, |n| n as i32)
                }
                Err(code) => code,
            }
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_screen_capture_dimensions",
        |caller: Caller<'_, HostState>| -> u64 {
            capture(&caller, |c| pack(c.screen_w, c.screen_h))
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_media_pipeline_stats",
        |caller: Caller<'_, HostState>| -> u64 {
            capture(&caller, |c| {
                let ring = c
                    .microphone
                    .as_ref()
                    .map_or(0, |m| m.buffer.lock().unwrap().len() as u64);
                (c.camera_frames << 32) | ring
            })
        },
    )?;

    Ok(())
}
