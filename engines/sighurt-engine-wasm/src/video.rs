//! FFmpeg-backed video for guests (`api_video_*`, `api_subtitle_*`): decode, playback clock,
//! HLS variant metadata, and subtitle cues.

use std::io::Write;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use wasmtime::{Caller, Linker};

use ffmpeg::format::{self, Pixel};
use ffmpeg::media::Type;
use ffmpeg::software::scaling::{context::Context as ScalerContext, flag::Flags as ScaleFlags};
use ffmpeg::util::frame::video::Video;
use ffmpeg::util::mathematics::{rescale, Rescale};
use ffmpeg_next as ffmpeg;
use tempfile::NamedTempFile;
use url::Url;

use crate::capabilities::{
    guest_bytes, guest_str, write_prefix, ConsoleLevel, DecodedImage, DrawCommand, HostState,
};
use crate::subtitle::{self, SubtitleCue};
use crate::video_format;

static FFMPEG_INIT: OnceLock<Result<(), ffmpeg::Error>> = OnceLock::new();

fn ensure_ffmpeg() -> Result<(), ffmpeg::Error> {
    *FFMPEG_INIT.get_or_init(|| {
        let r = ffmpeg::init();
        if r.is_ok() {
            // Avoid spamming stderr with EOF/packet noise during normal decode.
            ffmpeg::util::log::set_level(ffmpeg::util::log::Level::Quiet);
        }
        r
    })
}

fn frame_pts_ms_from_tb(time_base: ffmpeg::Rational, frame: &Video) -> Option<u64> {
    let pts = frame.timestamp().or_else(|| frame.pts())?;
    if pts < 0 {
        return None;
    }
    let ms = pts.rescale(time_base, (1, 1000));
    if ms < 0 {
        None
    } else {
        Some(ms as u64)
    }
}

fn scale_frame_to_rgba(
    scaler: &mut ScalerContext,
    decoded: &Video,
) -> Result<DecodedImage, String> {
    let mut rgb = Video::empty();
    scaler.run(decoded, &mut rgb).map_err(|e| e.to_string())?;
    let (width, height) = (rgb.width(), rgb.height());
    let (stride, row) = (rgb.stride(0), width as usize * 4);
    let mut pixels = Vec::with_capacity(row * height as usize);
    for line in rgb.data(0).chunks(stride).take(height as usize) {
        pixels.extend_from_slice(&line[..row]);
    }
    Ok(DecodedImage {
        width,
        height,
        pixels,
    })
}

/// A decoded frame and its presentation time in milliseconds.
type Frame = (Arc<DecodedImage>, u64);

/// Receives the frames the decoder has ready, keeping the last one before `target_ms` in
/// `before` and the first at or after it in `after`. Returns whether that one was found.
fn receive_frames(
    decoder: &mut ffmpeg::decoder::Video,
    scaler: &mut ScalerContext,
    time_base: ffmpeg::Rational,
    target_ms: u64,
    before: &mut Option<Frame>,
    after: &mut Option<Frame>,
) -> Result<bool, String> {
    let mut decoded = Video::empty();
    while decoder.receive_frame(&mut decoded).is_ok() {
        let pts_ms = frame_pts_ms_from_tb(time_base, &decoded).unwrap_or(0);
        let frame = (Arc::new(scale_frame_to_rgba(scaler, &decoded)?), pts_ms);
        if pts_ms >= target_ms {
            *after = Some(frame);
            return Ok(true);
        }
        *before = Some(frame);
    }
    Ok(false)
}

/// Decodes one video stream to RGBA via libswscale.
pub struct VideoPlayer {
    input: format::context::Input,
    video_stream_index: usize,
    decoder: ffmpeg::decoder::Video,
    scaler: ScalerContext,
    time_base: ffmpeg::Rational,
    pub duration_ms: u64,
    /// The frame last returned and its PTS (ms), for incremental decode.
    last: Option<Frame>,
    /// Whether [`ffmpeg::decoder::Opened::send_eof`] was already sent (must not repeat).
    decoder_eof_sent: bool,
}

impl VideoPlayer {
    fn open_input(input: format::context::Input) -> Result<Self, String> {
        ensure_ffmpeg().map_err(|e| e.to_string())?;

        let stream = input
            .streams()
            .best(Type::Video)
            .ok_or_else(|| "no video stream".to_string())?;
        let video_stream_index = stream.index();
        let time_base = stream.time_base();

        let context = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
            .map_err(|e| e.to_string())?;
        let decoder = context.decoder().video().map_err(|e| e.to_string())?;

        let duration_ms = Self::probe_duration_ms(&input, &stream);

        let (width, height) = (decoder.width(), decoder.height());
        let scaler = ScalerContext::get(
            decoder.format(),
            width,
            height,
            Pixel::RGBA,
            width,
            height,
            ScaleFlags::BILINEAR,
        )
        .map_err(|e| e.to_string())?;

        Ok(Self {
            input,
            video_stream_index,
            decoder,
            scaler,
            time_base,
            duration_ms,
            last: None,
            decoder_eof_sent: false,
        })
    }

    pub fn open_path(path: &Path) -> Result<Self, String> {
        let input = format::input(path).map_err(|e| e.to_string())?;
        Self::open_input(input)
    }

    pub fn open_url(url: &str) -> Result<Self, String> {
        let input = format::input(url).map_err(|e| e.to_string())?;
        Self::open_input(input)
    }

    fn probe_duration_ms(
        input: &format::context::Input,
        stream: &ffmpeg::format::stream::Stream,
    ) -> u64 {
        let d = input.duration();
        if d > 0 {
            let ms = d.rescale(rescale::TIME_BASE, (1, 1000));
            if ms > 0 {
                return ms as u64;
            }
        }
        let sd = stream.duration();
        if sd > 0 {
            let ms = sd.rescale(stream.time_base(), (1, 1000));
            return ms.max(0) as u64;
        }
        0
    }

    fn seek_to_ms(&mut self, target_ms: u64) -> Result<(), String> {
        let ts = (target_ms as i64).rescale((1, 1000), rescale::TIME_BASE);
        self.input.seek(ts, ts..ts).map_err(|e| e.to_string())?;
        self.decoder.flush();
        self.last = None;
        self.decoder_eof_sent = false;
        Ok(())
    }

    /// Decode a frame appropriate for `target_ms` (last frame with PTS ≤ target, or first at/after seek).
    pub fn decode_frame_at(&mut self, target_ms: u64) -> Result<Arc<DecodedImage>, String> {
        let target_ms = target_ms.min(self.duration_ms.saturating_add(500));
        let need_seek = match &self.last {
            None => true,
            Some((_, last)) => target_ms < *last || target_ms.saturating_sub(*last) > 2_500,
        };
        if need_seek {
            self.seek_to_ms(target_ms)?;
        }

        let VideoPlayer {
            input,
            video_stream_index,
            decoder,
            scaler,
            time_base,
            last,
            decoder_eof_sent,
            ..
        } = self;
        let (mut before, mut after) = (None, None);
        for (stream, packet) in input.packets() {
            if stream.index() != *video_stream_index {
                continue;
            }
            decoder.send_packet(&packet).map_err(|e| e.to_string())?;
            if receive_frames(
                decoder,
                scaler,
                *time_base,
                target_ms,
                &mut before,
                &mut after,
            )? {
                break;
            }
        }
        // Frames already buffered in the decoder (no new packets yet).
        if after.is_none() {
            receive_frames(
                decoder,
                scaler,
                *time_base,
                target_ms,
                &mut before,
                &mut after,
            )?;
        }
        // Demuxer exhausted: flush the decoder exactly once, then drain remaining frames.
        if after.is_none() && before.is_none() && !*decoder_eof_sent {
            let _ = decoder.send_eof();
            *decoder_eof_sent = true;
            receive_frames(
                decoder,
                scaler,
                *time_base,
                target_ms,
                &mut before,
                &mut after,
            )?;
        }
        if let Some(frame) = after.or(before) {
            *last = Some(frame);
        }
        match last {
            Some((image, _)) => Ok(image.clone()),
            None => Err("no video frame decoded".into()),
        }
    }
}

/// FFmpeg decoder state is synchronized through [`std::sync::Mutex`] on the host; not shared across threads concurrently.
unsafe impl Send for VideoPlayer {}

/// Global playback + optional [`VideoPlayer`] and subtitle list.
pub struct VideoPlaybackState {
    pub player: Option<VideoPlayer>,
    pub playing: bool,
    play_start: Option<Instant>,
    pub base_position_ms: u64,
    pub volume: f32,
    pub looping: bool,
    pub subtitles: Vec<SubtitleCue>,
    pub last_url_content_type: String,
    pub hls_variants: Vec<String>,
    pub hls_base_url: String,
    temp_file: Option<NamedTempFile>,
}

impl Default for VideoPlaybackState {
    fn default() -> Self {
        Self {
            player: None,
            playing: false,
            play_start: None,
            base_position_ms: 0,
            volume: 1.0,
            looping: false,
            subtitles: Vec::new(),
            last_url_content_type: String::new(),
            hls_variants: Vec::new(),
            hls_base_url: String::new(),
            temp_file: None,
        }
    }
}

impl VideoPlaybackState {
    pub fn duration_ms(&self) -> u64 {
        self.player.as_ref().map(|p| p.duration_ms).unwrap_or(0)
    }

    pub fn current_position_ms(&self) -> u64 {
        let dur = self.duration_ms();
        let pos = if self.playing {
            let start = self.play_start.expect("play_start when playing");
            self.base_position_ms + start.elapsed().as_millis() as u64
        } else {
            self.base_position_ms
        };
        if self.looping && dur > 0 {
            pos % dur
        } else if dur > 0 {
            pos.min(dur)
        } else {
            pos
        }
    }

    pub fn play(&mut self) {
        self.play_start = Some(Instant::now());
        self.playing = true;
    }

    pub fn pause(&mut self) {
        if self.playing {
            self.base_position_ms = self.current_position_ms();
            self.playing = false;
            self.play_start = None;
        }
    }

    pub fn stop(&mut self) {
        self.playing = false;
        self.play_start = None;
        self.base_position_ms = 0;
        self.player = None;
        self.temp_file = None;
        self.hls_variants.clear();
        self.hls_base_url.clear();
    }

    /// Reset clock only (after swapping the underlying stream, e.g. HLS variant).
    pub fn reset_playback_clock(&mut self) {
        self.base_position_ms = 0;
        self.playing = false;
        self.play_start = None;
    }

    pub fn seek(&mut self, position_ms: u64) {
        let dur = self.duration_ms();
        let pos = if dur > 0 {
            position_ms.min(dur)
        } else {
            position_ms
        };
        self.base_position_ms = pos;
        if self.playing {
            self.play_start = Some(Instant::now());
        }
    }

    pub fn open_bytes(&mut self, data: &[u8], format_hint: u32) -> Result<(), String> {
        self.stop();
        let ext = video_format::suffix_for_format(format_hint);
        let mut tmp = tempfile::Builder::new()
            .suffix(ext)
            .tempfile()
            .map_err(|e| e.to_string())?;
        tmp.write_all(data).map_err(|e| e.to_string())?;
        tmp.flush().map_err(|e| e.to_string())?;
        self.player = Some(VideoPlayer::open_path(tmp.path())?);
        self.temp_file = Some(tmp);
        Ok(())
    }
}

/// Parse `#EXT-X-STREAM-INF` master playlist variant URIs (best-effort).
pub fn parse_hls_master_variants(body: &str) -> Vec<String> {
    let lines: Vec<&str> = body.lines().collect();
    let mut out = Vec::new();
    for i in 0..lines.len() {
        if lines[i].starts_with("#EXT-X-STREAM-INF") && i + 1 < lines.len() {
            let u = lines[i + 1].trim();
            if !u.starts_with('#') && !u.is_empty() {
                out.push(u.to_string());
            }
        }
    }
    out
}

pub fn resolve_against_base(base: &str, relative: &str) -> Option<String> {
    let b = Url::parse(base).ok()?;
    b.join(relative).ok().map(|u| u.to_string())
}

impl VideoPlaybackState {
    /// The absolute URL of HLS variant `index`.
    fn variant_url(&self, index: u32) -> Option<String> {
        let relative = self.hls_variants.get(index as usize)?;
        Some(resolve_against_base(&self.hls_base_url, relative).unwrap_or_else(|| relative.clone()))
    }
}

/// Decodes the frame at the current playback position and draws it into a rectangle of the
/// canvas, with the subtitle cue showing at that time.
fn render_at(state: &HostState, x: f32, y: f32, w: f32, h: f32) -> Result<(), String> {
    let mut video = state.video.lock().unwrap();
    let t = video.current_position_ms();
    let player = video.player.as_mut().ok_or("no video loaded")?;
    let frame = player.decode_frame_at(t)?;
    let subtitle = subtitle::cue_text_at(&video.subtitles, t).map(str::to_string);
    drop(video);

    let mut canvas = state.canvas.lock().unwrap();
    canvas.push_image(frame, x, y, w, h);
    if let Some(text) = subtitle {
        canvas.push(DrawCommand::Text {
            x: x + 8.0,
            y: (y + h - 24.0).max(y + 12.0),
            size: 16.0,
            color: [255; 4],
            text,
        });
    }
    Ok(())
}

/// `api_video_load_url`: opens `url` for playback, reading the variants of an HLS master
/// playlist. Returns the guest's result code.
fn load_url(state: &HostState, url: &str) -> i32 {
    state.log(ConsoleLevel::Log, format!("[VIDEO] Opening {url}"));
    let Ok(client) = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(90))
        .build()
    else {
        return -3;
    };
    let content_type = client
        .head(url)
        .send()
        .ok()
        .and_then(|r| {
            let ct = r.headers().get(reqwest::header::CONTENT_TYPE)?;
            ct.to_str().ok().map(str::to_string)
        })
        .unwrap_or_default();
    let is_playlist = url.to_ascii_lowercase().contains("m3u8")
        || content_type.to_ascii_lowercase().contains("mpegurl")
        || content_type.to_ascii_lowercase().contains("m3u8");
    let playlist = is_playlist
        .then(|| {
            client
                .get(url)
                .header("Accept", video_format::VIDEO_HTTP_ACCEPT)
                .timeout(Duration::from_secs(60))
                .send()
                .ok()
                .filter(|r| r.status().is_success())?
                .text()
                .ok()
        })
        .flatten();

    let mut video = state.video.lock().unwrap();
    video.stop();
    video.last_url_content_type.clone_from(&content_type);
    video.hls_base_url = url.to_string();
    video.hls_variants = playlist
        .map(|body| parse_hls_master_variants(&body))
        .unwrap_or_default();
    match VideoPlayer::open_url(url) {
        Ok(player) => {
            video.player = Some(player);
            let message = format!("[VIDEO] Opened URL (Content-Type: {content_type})");
            state.log(ConsoleLevel::Log, message);
            0
        }
        Err(e) => {
            state.log(ConsoleLevel::Error, format!("[VIDEO] Open failed: {e}"));
            -2
        }
    }
}

/// Register all `api_video_*` and `api_subtitle_*` host functions.
pub fn register_video_functions(linker: &mut Linker<HostState>) -> Result<()> {
    fn video<R>(caller: &Caller<'_, HostState>, f: impl FnOnce(&mut VideoPlaybackState) -> R) -> R {
        f(&mut caller.data().video.lock().unwrap())
    }

    linker.func_wrap(
        "oxide",
        "api_video_detect_format",
        |caller: Caller<'_, HostState>, data_ptr: u32, data_len: u32| -> u32 {
            let data = guest_bytes(&caller, data_ptr, data_len).unwrap_or_default();
            video_format::sniff_video_format(&data)
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_video_load",
        |caller: Caller<'_, HostState>, data_ptr: u32, data_len: u32, format_hint: u32| -> i32 {
            let data = guest_bytes(&caller, data_ptr, data_len).unwrap_or_default();
            if data.is_empty() {
                return -1;
            }
            match video(&caller, |v| v.open_bytes(&data, format_hint)) {
                Ok(()) => {
                    caller
                        .data()
                        .log(ConsoleLevel::Log, "[VIDEO] Loaded from bytes");
                    0
                }
                Err(e) => {
                    let message = format!("[VIDEO] Load failed: {e}");
                    caller.data().log(ConsoleLevel::Error, message);
                    -2
                }
            }
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_video_load_url",
        |caller: Caller<'_, HostState>, url_ptr: u32, url_len: u32| -> i32 {
            let url = guest_str(&caller, url_ptr, url_len).unwrap_or_default();
            if url.is_empty() {
                return -1;
            }
            load_url(caller.data(), &url)
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_video_last_url_content_type",
        |mut caller: Caller<'_, HostState>, out_ptr: u32, out_cap: u32| -> u32 {
            let content_type = video(&caller, |v| v.last_url_content_type.clone());
            write_prefix(&mut caller, out_ptr, out_cap, content_type.as_bytes()).unwrap_or(0)
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_video_hls_variant_count",
        |caller: Caller<'_, HostState>| -> u32 { video(&caller, |v| v.hls_variants.len() as u32) },
    )?;

    linker.func_wrap(
        "oxide",
        "api_video_hls_variant_url",
        |mut caller: Caller<'_, HostState>, index: u32, out_ptr: u32, out_cap: u32| -> u32 {
            let url = video(&caller, |v| v.variant_url(index)).unwrap_or_default();
            write_prefix(&mut caller, out_ptr, out_cap, url.as_bytes()).unwrap_or(0)
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_video_hls_open_variant",
        |caller: Caller<'_, HostState>, index: u32| -> i32 {
            let result = video(&caller, |v| {
                let url = v.variant_url(index)?;
                v.hls_base_url.clone_from(&url);
                v.hls_variants.clear();
                Some(VideoPlayer::open_url(&url).map(|player| {
                    v.player = Some(player);
                    v.reset_playback_clock();
                }))
            });
            match result {
                None => -1,
                Some(Ok(())) => {
                    let message = format!("[VIDEO] Opened HLS variant {index}");
                    caller.data().log(ConsoleLevel::Log, message);
                    0
                }
                Some(Err(e)) => {
                    let message = format!("[VIDEO] Variant open failed: {e}");
                    caller.data().log(ConsoleLevel::Error, message);
                    -2
                }
            }
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_video_play",
        |caller: Caller<'_, HostState>| video(&caller, |v| v.play()),
    )?;
    linker.func_wrap(
        "oxide",
        "api_video_pause",
        |caller: Caller<'_, HostState>| video(&caller, |v| v.pause()),
    )?;
    linker.func_wrap(
        "oxide",
        "api_video_stop",
        |caller: Caller<'_, HostState>| video(&caller, |v| v.stop()),
    )?;
    linker.func_wrap(
        "oxide",
        "api_video_seek",
        |caller: Caller<'_, HostState>, position_ms: u64| -> i32 {
            video(&caller, |v| v.seek(position_ms));
            0
        },
    )?;
    linker.func_wrap(
        "oxide",
        "api_video_position",
        |caller: Caller<'_, HostState>| -> u64 { video(&caller, |v| v.current_position_ms()) },
    )?;
    linker.func_wrap(
        "oxide",
        "api_video_duration",
        |caller: Caller<'_, HostState>| -> u64 { video(&caller, |v| v.duration_ms()) },
    )?;

    linker.func_wrap(
        "oxide",
        "api_video_render",
        |caller: Caller<'_, HostState>, x: f32, y: f32, w: f32, h: f32| -> i32 {
            match render_at(caller.data(), x, y, w, h) {
                Ok(()) => 0,
                Err(e) => {
                    let message = format!("[VIDEO] Render: {e}");
                    caller.data().log(ConsoleLevel::Error, message);
                    -1
                }
            }
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_video_set_volume",
        |caller: Caller<'_, HostState>, level: f32| {
            video(&caller, |v| v.volume = level.clamp(0.0, 2.0))
        },
    )?;
    linker.func_wrap(
        "oxide",
        "api_video_get_volume",
        |caller: Caller<'_, HostState>| -> f32 { video(&caller, |v| v.volume) },
    )?;
    linker.func_wrap(
        "oxide",
        "api_video_set_loop",
        |caller: Caller<'_, HostState>, enabled: u32| video(&caller, |v| v.looping = enabled != 0),
    )?;
    // Picture-in-picture has no place outside the page here, so this does nothing. It stays in
    // the `oxide` ABI so apps built against it still instantiate.
    linker.func_wrap("oxide", "api_video_set_pip", |_: u32| {})?;

    for (name, parse) in [
        (
            "api_subtitle_load_srt",
            subtitle::parse_srt as fn(&str) -> Vec<SubtitleCue>,
        ),
        ("api_subtitle_load_vtt", subtitle::parse_vtt),
    ] {
        linker.func_wrap(
            "oxide",
            name,
            move |caller: Caller<'_, HostState>, ptr: u32, len: u32| -> i32 {
                let text = guest_str(&caller, ptr, len).unwrap_or_default();
                video(&caller, |v| v.subtitles = parse(&text));
                0
            },
        )?;
    }
    linker.func_wrap(
        "oxide",
        "api_subtitle_clear",
        |caller: Caller<'_, HostState>| video(&caller, |v| v.subtitles.clear()),
    )?;

    Ok(())
}
