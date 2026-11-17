//! FFmpeg-backed video for guests (`api_video_*`, `api_subtitle_*`): decode, playback clock,
//! HLS variant metadata, and subtitle cues.
//!
//! FFmpeg opens what it is given with any protocol it has (`file:`, `pipe:`, `concat:`,
//! `tcp://`...), so nothing a guest names reaches it unchecked. A URL must be an absolute
//! `http`/`https` URL the document's [`NetPolicy`](crate::net::NetPolicy) allows, its host's
//! every address included; FFmpeg then opens it with only the protocols HTTP(S) needs
//! ([`NETWORK_PROTOCOLS`], which also binds the segments of an HLS playlist) and a read timeout.
//! FFmpeg resolves the name again and follows HTTP redirects itself, which the policy doesn't
//! see. Bytes a guest passes are opened as a local file with only the `file` protocol and the
//! container demuxers of [`LOCAL_FORMATS`], so they can't name other files or URLs (as an HLS
//! playlist or a concat list would). Streams whose frames are over [`MAX_VIDEO_SIDE`] or
//! [`MAX_VIDEO_PIXELS`] are refused before anything is allocated for them.
//!
//! Opening a URL waits for the network as a wait of the guest's call
//! ([`Budget::wait`](crate::engine::Budget::wait)), without holding the video state's lock.
//!
//! The decoder is `video_ffmpeg.rs`. A Windows build has no FFmpeg (`video_none.rs`): every
//! video fails to open, saying so, and the rest (the clock, subtitles, HLS playlists) is the
//! same.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use anyhow::Result;
use wasmtime::{Caller, Linker};

use tempfile::NamedTempFile;
use url::Url;

use crate::capabilities::{
    guest_bytes, guest_str, write_prefix, ConsoleLevel, DrawCommand, HostState, MAX_IMAGE_SIDE,
    MAX_URL,
};
use crate::engine::Wait;
use crate::net::{NetPolicy, HTTP};
use crate::subtitle::{self, SubtitleCue};
use crate::video_format;

/// The protocols FFmpeg may use for a video URL, and for what an HLS playlist names.
pub const NETWORK_PROTOCOLS: &str = "http,https,tcp,tls";
/// The demuxers FFmpeg may use for bytes a guest passes: containers that name no other file.
pub const LOCAL_FORMATS: &str = "mov,mp4,m4a,3gp,3g2,mj2,matroska,webm,avi,flv,mpegts,ogg,mpeg";
/// Longest side of a video frame, and most pixels in one: decoded frames are images.
pub const MAX_VIDEO_SIDE: u32 = MAX_IMAGE_SIDE;
pub const MAX_VIDEO_PIXELS: u32 = 4096 * 4096;
/// Largest HLS master playlist read, and most variants kept from it.
const MAX_PLAYLIST_BYTES: u64 = 1024 * 1024;
const MAX_HLS_VARIANTS: usize = 256;
/// How long the `HEAD` and playlist requests of `api_video_load_url` may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[cfg(not(windows))]
#[path = "video_ffmpeg.rs"]
mod player;
#[cfg(windows)]
#[path = "video_none.rs"]
mod player;
pub use player::VideoPlayer;

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

/// Parse `#EXT-X-STREAM-INF` master playlist variant URIs (best-effort), at most
/// [`MAX_HLS_VARIANTS`] of at most [`MAX_URL`] bytes.
pub fn parse_hls_master_variants(body: &str) -> Vec<String> {
    let lines: Vec<&str> = body.lines().collect();
    let mut out = Vec::new();
    for i in 0..lines.len() {
        if lines[i].starts_with("#EXT-X-STREAM-INF") && i + 1 < lines.len() {
            let u = lines[i + 1].trim();
            if !u.starts_with('#') && !u.is_empty() && u.len() <= MAX_URL {
                out.push(u.to_string());
            }
        }
        if out.len() == MAX_HLS_VARIANTS {
            break;
        }
    }
    out
}

/// `url` if FFmpeg may open it for `policy`: an absolute `http`/`https` URL whose host and its
/// every address the policy allows. Blocks while it resolves the host.
pub fn checked_url(url: &str, policy: &NetPolicy) -> Result<String, String> {
    let parsed = policy.check(url, HTTP)?;
    policy.check_resolved(&parsed)?;
    Ok(parsed.into())
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

/// What `api_video_load_url` found at a URL.
struct Opened {
    content_type: String,
    variants: Vec<String>,
    player: Result<VideoPlayer, String>,
}

/// Opens `url`, which [`checked_url`] allowed for `policy`, reading the variants of an HLS
/// master playlist. Waits for the network.
fn open_url(url: &str, policy: &NetPolicy) -> Result<Opened, String> {
    let client = crate::net::blocking_client(policy)?;
    let request = |method| {
        let request = client.request(method, url).timeout(REQUEST_TIMEOUT);
        match policy.origin() {
            Some(origin) => request.header("Origin", origin),
            None => request,
        }
    };
    let content_type = request(reqwest::Method::HEAD)
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
            let response = request(reqwest::Method::GET)
                .header("Accept", video_format::VIDEO_HTTP_ACCEPT)
                .send()
                .ok()
                .filter(|r| r.status().is_success())?;
            let mut body = Vec::new();
            response
                .take(MAX_PLAYLIST_BYTES)
                .read_to_end(&mut body)
                .ok()?;
            String::from_utf8(body).ok()
        })
        .flatten();
    Ok(Opened {
        content_type,
        variants: playlist
            .map(|body| parse_hls_master_variants(&body))
            .unwrap_or_default(),
        player: VideoPlayer::open_url(url),
    })
}

/// `api_video_load_url`: opens `url` for playback, reading the variants of an HLS master
/// playlist. Returns the guest's result code: -2 when the URL is refused or can't be opened,
/// -1 when the document was cancelled meanwhile.
fn load_url(state: &HostState, url: &str) -> i32 {
    state.log(ConsoleLevel::Log, format!("[VIDEO] Opening {url}"));
    let policy = state.net_policy();
    let opened = state.budget.wait(Wait::Host, || {
        checked_url(url, &policy).and_then(|url| open_url(&url, &policy))
    });
    let Some(opened) = opened else {
        return -1;
    };
    let Opened {
        content_type,
        variants,
        player,
    } = match opened {
        Ok(opened) => opened,
        Err(e) => {
            state.log(ConsoleLevel::Error, format!("[VIDEO] Refused: {e}"));
            return -2;
        }
    };

    let mut video = state.video.lock().unwrap();
    video.stop();
    video.last_url_content_type.clone_from(&content_type);
    video.hls_base_url = url.to_string();
    video.hls_variants = variants;
    match player {
        Ok(player) => {
            video.player = Some(player);
            drop(video);
            let message = format!("[VIDEO] Opened URL (Content-Type: {content_type})");
            state.log(ConsoleLevel::Log, message);
            0
        }
        Err(e) => {
            drop(video);
            state.log(ConsoleLevel::Error, format!("[VIDEO] Open failed: {e}"));
            -2
        }
    }
}

/// `api_video_hls_open_variant`: opens variant `index` of the playlist loaded last, under the
/// same checks as a URL the guest gives. `None` when there is no such variant.
fn open_variant(state: &HostState, index: u32) -> Option<Result<(), String>> {
    let url = state.video.lock().unwrap().variant_url(index)?;
    let policy = state.net_policy();
    let player = state.budget.wait(Wait::Host, || {
        checked_url(&url, &policy).and_then(|url| VideoPlayer::open_url(&url))
    });
    let Some(player) = player else {
        return Some(Err("cancelled".into()));
    };
    let mut video = state.video.lock().unwrap();
    video.hls_base_url.clone_from(&url);
    video.hls_variants.clear();
    Some(player.map(|player| {
        video.player = Some(player);
        video.reset_playback_clock();
    }))
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
            if url.is_empty() || url.len() > MAX_URL {
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
            match open_variant(caller.data(), index) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing a guest names reaches FFmpeg unless it is an HTTP(S) URL the policy allows.
    #[test]
    fn only_allowed_http_urls_reach_ffmpeg() {
        let policy = NetPolicy::for_page("https://a.test/app.wasm", Some([1, 1, 1, 1].into()));
        for refused in [
            "file:///etc/passwd",
            "/proc/self/fd/0",
            "pipe:0",
            "concat:https://a.test/1.ts|https://a.test/2.ts",
            "subfile,,start,0,end,0,,:https://a.test/x",
            "tcp://127.0.0.1:22",
            "http://127.0.0.1:8080/video.mp4",
            "https://localhost/video.mp4",
            "crypto+https://a.test/x",
        ] {
            assert!(checked_url(refused, &policy).is_err(), "{refused}");
        }
    }

    /// Bytes that are an HLS playlist naming a local file are not opened: only containers are.
    #[test]
    fn guest_bytes_cannot_name_other_files() {
        let mut state = VideoPlaybackState::default();
        let playlist =
            b"#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\nfile:///etc/hostname\n#EXT-X-ENDLIST\n";
        assert!(state.open_bytes(playlist, 0).is_err());
        let concat = b"ffconcat version 1.0\nfile /etc/hostname\n";
        assert!(state.open_bytes(concat, 0).is_err());
    }

    #[test]
    fn playlists_keep_a_bounded_number_of_variants() {
        let body = "#EXT-X-STREAM-INF:BANDWIDTH=1\nv.m3u8\n".repeat(MAX_HLS_VARIANTS * 2);
        assert_eq!(parse_hls_master_variants(&body).len(), MAX_HLS_VARIANTS);
    }
}
