//! The video decoder of `video` ([`VideoPlayer`]): FFmpeg, through `ffmpeg-next`, decoding one
//! stream to RGBA with libswscale. What FFmpeg may open is checked before this is reached
//! (`video`'s documentation): local files with the `file` protocol and the containers of
//! [`LOCAL_FORMATS`] only, URLs with [`NETWORK_PROTOCOLS`] only.
//!
//! Owns: FFmpeg's contexts for one stream. Must not own: what may be opened, the playback clock or
//! drawing (`video`'s).

use std::path::Path;
use std::sync::{Arc, OnceLock};

use ffmpeg::format::{self, Pixel};
use ffmpeg::media::Type;
use ffmpeg::software::scaling::{context::Context as ScalerContext, flag::Flags as ScaleFlags};
use ffmpeg::util::frame::video::Video;
use ffmpeg::util::mathematics::{rescale, Rescale};
use ffmpeg_next as ffmpeg;

use super::{LOCAL_FORMATS, MAX_VIDEO_PIXELS, MAX_VIDEO_SIDE, NETWORK_PROTOCOLS};
use crate::capabilities::DecodedImage;

/// Longest FFmpeg waits for the network on one read, in microseconds (its `rw_timeout`).
const READ_TIMEOUT_US: &str = "30000000";

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
        let side = 1..=MAX_VIDEO_SIDE;
        if !side.contains(&width)
            || !side.contains(&height)
            || width.saturating_mul(height) > MAX_VIDEO_PIXELS
        {
            return Err(format!(
                "{width}x{height} frames are over the limit of {MAX_VIDEO_PIXELS} pixels"
            ));
        }
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

    /// Opens a local file of guest bytes: the `file` protocol and [`LOCAL_FORMATS`] only.
    pub fn open_path(path: &Path) -> Result<Self, String> {
        ensure_ffmpeg().map_err(|e| e.to_string())?;
        let mut options = ffmpeg::Dictionary::new();
        options.set("protocol_whitelist", "file");
        options.set("format_whitelist", LOCAL_FORMATS);
        let input = format::input_with_dictionary(path, options).map_err(|e| e.to_string())?;
        Self::open_input(input)
    }

    /// Opens `url`, which [`checked_url`](super::checked_url) allowed: [`NETWORK_PROTOCOLS`] only, with a read
    /// timeout.
    pub(super) fn open_url(url: &str) -> Result<Self, String> {
        ensure_ffmpeg().map_err(|e| e.to_string())?;
        let mut options = ffmpeg::Dictionary::new();
        options.set("protocol_whitelist", NETWORK_PROTOCOLS);
        options.set("rw_timeout", READ_TIMEOUT_US);
        let input = format::input_with_dictionary(url, options).map_err(|e| e.to_string())?;
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
