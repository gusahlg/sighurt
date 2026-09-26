//! Audio playback for guests (`api_audio_*`), on [rodio](https://crates.io/crates/rodio).
//!
//! Each logical channel has its own [`rodio::Player`] so guests can play overlapping sounds
//! (for example music on one channel and effects on another). The functions without a channel
//! argument use channel 0.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;
use rodio::Source;
use wasmtime::{Caller, Linker};

use crate::audio_format::{self, AUDIO_FORMAT_UNKNOWN};
use crate::capabilities::{
    guest_bytes, guest_str, http_get, with, with_started, write_prefix, ConsoleLevel, HostState,
};

/// Per-channel audio state: a rodio Player plus metadata.
struct AudioChannel {
    player: rodio::Player,
    duration_ms: u64,
    looping: bool,
}

/// The audio output and its channels, opened on first use.
pub struct AudioEngine {
    device_sink: rodio::stream::MixerDeviceSink,
    channels: HashMap<u32, AudioChannel>,
}

impl AudioEngine {
    fn try_new() -> Option<Self> {
        let mut device_sink = rodio::DeviceSinkBuilder::open_default_sink().ok()?;
        device_sink.log_on_drop(false);
        Some(Self {
            device_sink,
            channels: HashMap::new(),
        })
    }

    fn channel(&mut self, id: u32) -> &mut AudioChannel {
        let mixer = self.device_sink.mixer();
        self.channels.entry(id).or_insert_with(|| AudioChannel {
            player: rodio::Player::connect_new(mixer),
            duration_ms: 0,
            looping: false,
        })
    }

    /// Replaces what `channel` plays with the sound file in `data`. False if it can't be decoded.
    fn play(&mut self, channel: u32, data: Vec<u8>) -> bool {
        let Ok(source) = rodio::Decoder::try_from(std::io::Cursor::new(data)) else {
            return false;
        };
        let duration = source.total_duration().unwrap_or_default();
        let ch = self.channel(channel);
        ch.player.clear();
        ch.duration_ms = duration.as_millis() as u64;
        if ch.looping {
            ch.player.append(source.repeat_infinite());
        } else {
            ch.player.append(source);
        }
        ch.player.play();
        true
    }
}

/// Plays `data` on `channel`, warning when it isn't the format the guest said (`hint`).
/// Returns the guest's result code.
fn play(state: &HostState, channel: u32, data: Vec<u8>, hint: u32) -> i32 {
    if data.is_empty() {
        return -1;
    }
    let sniffed = audio_format::sniff_audio_format(&data);
    if hint != AUDIO_FORMAT_UNKNOWN && sniffed != AUDIO_FORMAT_UNKNOWN && sniffed != hint {
        state.log(
            ConsoleLevel::Warn,
            format!("[AUDIO] Format hint {hint} does not match sniffed container {sniffed}"),
        );
    }
    match with_started(&state.audio, AudioEngine::try_new, |a| {
        a.play(channel, data)
    }) {
        Some(true) => {
            state.log(
                ConsoleLevel::Log,
                format!("[AUDIO] Playing on channel {channel}"),
            );
            0
        }
        Some(false) => {
            state.log(
                ConsoleLevel::Error,
                format!("[AUDIO] Failed to decode audio for channel {channel}"),
            );
            -2
        }
        None => {
            state.log(ConsoleLevel::Error, "[AUDIO] No audio device available");
            -3
        }
    }
}

/// Fetches the sound at `url` and plays it on channel 0. Returns the guest's result code.
fn play_url(state: &HostState, url: &str) -> i32 {
    state.log(ConsoleLevel::Log, format!("[AUDIO] Fetching {url}"));
    let (data, content_type) = match http_get(url, audio_format::AUDIO_HTTP_ACCEPT, u64::MAX) {
        Ok(response) => response,
        Err(e) => {
            state.log(ConsoleLevel::Error, format!("[AUDIO] Fetch error: {e}"));
            return -1;
        }
    };
    let content_type = content_type.unwrap_or_default();
    state
        .last_audio_url_content_type
        .lock()
        .unwrap()
        .clone_from(&content_type);
    let sniffed = audio_format::sniff_audio_format(&data);
    if !content_type.is_empty() {
        if audio_format::is_likely_non_audio_document(&content_type)
            && sniffed == AUDIO_FORMAT_UNKNOWN
        {
            state.log(
                ConsoleLevel::Error,
                "[AUDIO] Response is not a supported audio resource (document MIME, no audio signature)",
            );
            return -4;
        }
        let mime_format = audio_format::mime_to_audio_format(&content_type);
        if mime_format != AUDIO_FORMAT_UNKNOWN
            && sniffed != AUDIO_FORMAT_UNKNOWN
            && mime_format != sniffed
        {
            state.log(
                ConsoleLevel::Warn,
                format!(
                    "[AUDIO] Content-Type disagrees with sniffed container (MIME -> {mime_format}, sniff -> {sniffed})"
                ),
            );
        }
    }
    play(state, 0, data, AUDIO_FORMAT_UNKNOWN)
}

/// Runs `f` on channel `id` if it exists.
fn on_channel<R>(
    caller: &Caller<'_, HostState>,
    id: u32,
    f: impl FnOnce(&AudioChannel) -> R,
) -> Option<R> {
    with(&caller.data().audio, |a| a.channels.get(&id).map(f)).flatten()
}

/// Register all `api_audio_*` host functions.
pub fn register_audio_functions(linker: &mut Linker<HostState>) -> Result<()> {
    linker.func_wrap(
        "oxide",
        "api_audio_play",
        |caller: Caller<'_, HostState>, data_ptr: u32, data_len: u32| -> i32 {
            let data = guest_bytes(&caller, data_ptr, data_len).unwrap_or_default();
            play(caller.data(), 0, data, AUDIO_FORMAT_UNKNOWN)
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_audio_play_with_format",
        |caller: Caller<'_, HostState>, data_ptr: u32, data_len: u32, hint: u32| -> i32 {
            let data = guest_bytes(&caller, data_ptr, data_len).unwrap_or_default();
            play(caller.data(), 0, data, hint)
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_audio_channel_play",
        |caller: Caller<'_, HostState>, channel: u32, data_ptr: u32, data_len: u32| -> i32 {
            let data = guest_bytes(&caller, data_ptr, data_len).unwrap_or_default();
            play(caller.data(), channel, data, AUDIO_FORMAT_UNKNOWN)
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_audio_channel_play_with_format",
        |caller: Caller<'_, HostState>,
         channel: u32,
         data_ptr: u32,
         data_len: u32,
         hint: u32|
         -> i32 {
            let data = guest_bytes(&caller, data_ptr, data_len).unwrap_or_default();
            play(caller.data(), channel, data, hint)
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_audio_play_url",
        |caller: Caller<'_, HostState>, url_ptr: u32, url_len: u32| -> i32 {
            let url = guest_str(&caller, url_ptr, url_len).unwrap_or_default();
            play_url(caller.data(), &url)
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_audio_detect_format",
        |caller: Caller<'_, HostState>, data_ptr: u32, data_len: u32| -> u32 {
            let data = guest_bytes(&caller, data_ptr, data_len).unwrap_or_default();
            audio_format::sniff_audio_format(&data)
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_audio_last_url_content_type",
        |mut caller: Caller<'_, HostState>, out_ptr: u32, out_cap: u32| -> u32 {
            let content_type = caller
                .data()
                .last_audio_url_content_type
                .lock()
                .unwrap()
                .clone();
            write_prefix(&mut caller, out_ptr, out_cap, content_type.as_bytes()).unwrap_or(0)
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_audio_pause",
        |caller: Caller<'_, HostState>| {
            on_channel(&caller, 0, |ch| ch.player.pause());
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_audio_resume",
        |caller: Caller<'_, HostState>| {
            on_channel(&caller, 0, |ch| ch.player.play());
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_audio_stop",
        |caller: Caller<'_, HostState>| {
            on_channel(&caller, 0, |ch| ch.player.stop());
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_audio_channel_stop",
        |caller: Caller<'_, HostState>, channel: u32| {
            on_channel(&caller, channel, |ch| ch.player.stop());
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_audio_set_volume",
        |caller: Caller<'_, HostState>, level: f32| {
            on_channel(&caller, 0, |ch| ch.player.set_volume(level.clamp(0.0, 2.0)));
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_audio_channel_set_volume",
        |caller: Caller<'_, HostState>, channel: u32, level: f32| {
            on_channel(&caller, channel, |ch| {
                ch.player.set_volume(level.clamp(0.0, 2.0))
            });
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_audio_get_volume",
        |caller: Caller<'_, HostState>| -> f32 {
            on_channel(&caller, 0, |ch| ch.player.volume()).unwrap_or(1.0)
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_audio_is_playing",
        |caller: Caller<'_, HostState>| -> u32 {
            on_channel(&caller, 0, |ch| {
                u32::from(!ch.player.is_paused() && !ch.player.empty())
            })
            .unwrap_or(0)
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_audio_position",
        |caller: Caller<'_, HostState>| -> u64 {
            on_channel(&caller, 0, |ch| ch.player.get_pos().as_millis() as u64).unwrap_or(0)
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_audio_seek",
        |caller: Caller<'_, HostState>, position_ms: u64| -> i32 {
            let result = on_channel(&caller, 0, |ch| {
                ch.player.try_seek(Duration::from_millis(position_ms))
            });
            match result {
                Some(Ok(())) => 0,
                Some(Err(e)) => {
                    let message = format!("[AUDIO] Seek failed: {e}");
                    caller.data().log(ConsoleLevel::Warn, message);
                    -1
                }
                None => -1,
            }
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_audio_duration",
        |caller: Caller<'_, HostState>| -> u64 {
            on_channel(&caller, 0, |ch| ch.duration_ms).unwrap_or(0)
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_audio_set_loop",
        |caller: Caller<'_, HostState>, enabled: u32| {
            with_started(&caller.data().audio, AudioEngine::try_new, |a| {
                a.channel(0).looping = enabled != 0;
            });
        },
    )?;

    Ok(())
}
