//! Audio demo: tones and sound effects synthesised in the guest as WAV bytes,
//! played on the default channel and on a separate SFX channel.

use core::f32::consts::TAU;

use sighurt_sdk::*;

const BG_COLOR: (u8, u8, u8) = (25, 25, 40);
const ACCENT: (u8, u8, u8) = (100, 80, 200);
const TEXT_DIM: (u8, u8, u8) = (140, 130, 160);
const TEXT_BRIGHT: (u8, u8, u8) = (230, 220, 255);
const GREEN: (u8, u8, u8) = (80, 220, 120);
const ORANGE: (u8, u8, u8) = (240, 180, 60);
const RED: (u8, u8, u8) = (220, 80, 80);

/// Tone pads: button label, frequency in Hz, status text.
const PADS: [(&str, f32, &str); 4] = [
    ("A4  440Hz", 440.0, "A4 (440 Hz)"),
    ("C5  523Hz", 523.25, "C5 (523 Hz)"),
    ("E5  659Hz", 659.25, "E5 (659 Hz)"),
    ("G5  784Hz", 783.99, "G5 (784 Hz)"),
];

const SAMPLE_RATE: u32 = 44100;
const SFX_CHANNEL: u32 = 1;

static mut LAST_NOTE: &str = "";
static mut VOLUME: f32 = 1.0;
static mut LOOPING: bool = false;

#[no_mangle]
pub extern "C" fn start_app() {
    log("Sighurt Audio Player loaded!");
    audio_channel_set_volume(SFX_CHANNEL, 0.8);
}

#[no_mangle]
pub extern "C" fn on_frame(_dt_ms: u32) {
    let (width, _height) = canvas_dimensions();
    let w = width as f32;

    canvas_clear(BG_COLOR.0, BG_COLOR.1, BG_COLOR.2, 255);

    // ── Header ──────────────────────────────────────────────────────
    canvas_rect(0.0, 0.0, w, 52.0, ACCENT.0, ACCENT.1, ACCENT.2, 255);
    text(20.0, 14.0, 22.0, (255, 255, 255), "Sighurt Audio Player");

    // ── Tone Pads ───────────────────────────────────────────────────
    heading(72.0, "TONE PADS");
    for (i, &(label, freq, note)) in PADS.iter().enumerate() {
        if button(20.0 + i as f32 * 100.0, 95.0, 90.0, 36.0, label) {
            audio_play(&tone(freq, 1.5));
            unsafe { LAST_NOTE = note };
        }
    }

    // ── Playback Controls ───────────────────────────────────────────
    canvas_line(20.0, 150.0, w - 20.0, 150.0, 50, 45, 70, 255, 1.0);
    heading(165.0, "CONTROLS (default channel)");

    if button(20.0, 188.0, 80.0, 30.0, "Pause") {
        audio_pause();
    }
    if button(110.0, 188.0, 80.0, 30.0, "Resume") {
        audio_resume();
    }
    if button(200.0, 188.0, 80.0, 30.0, "Stop") {
        audio_stop();
    }
    let looping = unsafe { LOOPING };
    let loop_label = if looping { "Loop: on" } else { "Loop: off" };
    if button(290.0, 188.0, 100.0, 30.0, loop_label) {
        unsafe { LOOPING = !looping };
        audio_set_loop(!looping);
    }

    text(20.0, 238.0, 13.0, TEXT_BRIGHT, "Volume");
    let mut vol = unsafe { VOLUME };
    if button(80.0, 232.0, 30.0, 26.0, "-") {
        vol = (vol - 0.1).max(0.0);
        audio_set_volume(vol);
    }
    if button(116.0, 232.0, 30.0, 26.0, "+") {
        vol = (vol + 0.1).min(1.5);
        audio_set_volume(vol);
    }
    unsafe { VOLUME = vol };
    text(160.0, 238.0, 13.0, GREEN, &format!("{:.0}%", vol * 100.0));

    // ── SFX Channel ─────────────────────────────────────────────────
    canvas_line(20.0, 275.0, w - 20.0, 275.0, 50, 45, 70, 255, 1.0);
    heading(290.0, "SFX CHANNEL (plays over main audio)");

    if button(20.0, 313.0, 80.0, 28.0, "Blip") {
        audio_channel_play(SFX_CHANNEL, &tone(1200.0, 0.08));
    }
    if button(110.0, 313.0, 80.0, 28.0, "Beep") {
        audio_channel_play(SFX_CHANNEL, &tone(880.0, 0.25));
    }
    if button(200.0, 313.0, 80.0, 28.0, "Chirp") {
        audio_channel_play(SFX_CHANNEL, &chirp());
    }

    // ── Status ──────────────────────────────────────────────────────
    canvas_line(20.0, 360.0, w - 20.0, 360.0, 50, 45, 70, 255, 1.0);

    let playing = audio_is_playing();
    let pos_ms = audio_position();
    let dur_ms = audio_duration();
    let pos_secs = pos_ms as f32 / 1000.0;
    let dur_secs = dur_ms as f32 / 1000.0;

    let (status_text, color) = if playing {
        ("Playing", GREEN)
    } else if pos_ms > 0 {
        ("Paused", ORANGE)
    } else {
        ("Stopped", RED)
    };

    text(20.0, 373.0, 14.0, color, status_text);

    let note = unsafe { LAST_NOTE };
    if !note.is_empty() {
        text(100.0, 373.0, 14.0, TEXT_DIM, &format!("  {note}"));
    }

    let time_info = if dur_ms > 0 {
        format!("Position: {pos_secs:.1}s / {dur_secs:.1}s")
    } else {
        format!("Position: {pos_secs:.1}s")
    };
    text(20.0, 395.0, 13.0, TEXT_DIM, &time_info);

    // ── Visualiser bar ──────────────────────────────────────────────
    if playing {
        // Epoch time needs f64: as f32 it only changes every ~2 minutes.
        let t = time_now_ms() as f64 / 200.0;
        let bar_y = 415.0;
        let bar_count = 24;
        let bar_w = (w - 40.0) / bar_count as f32;
        for i in 0..bar_count {
            let phase = t + i as f64 * 0.3;
            let h = (phase.sin() * 0.5 + 0.5) as f32 * 25.0 + 3.0;
            let hue_shift = (i as f32 / bar_count as f32 * 255.0) as u8;
            canvas_rect(
                20.0 + i as f32 * bar_w,
                bar_y + 25.0 - h,
                bar_w - 2.0,
                h,
                100 + hue_shift / 3,
                80,
                200,
                200,
            );
        }
    }
}

fn heading(y: f32, label: &str) {
    text(20.0, y, 14.0, TEXT_DIM, label);
}

fn text(x: f32, y: f32, size: f32, (r, g, b): (u8, u8, u8), s: &str) {
    canvas_text(x, y, size, r, g, b, 255, s);
}

/// A sine tone with a 10 ms attack and 50 ms release, as WAV bytes.
fn tone(frequency: f32, duration_secs: f32) -> Vec<u8> {
    let n = (SAMPLE_RATE as f32 * duration_secs) as usize;
    let attack = SAMPLE_RATE as usize / 100;
    let release = SAMPLE_RATE as usize / 20;
    wav((0..n).map(|i| {
        let t = i as f32 / SAMPLE_RATE as f32;
        let envelope = if i < attack {
            i as f32 / attack as f32
        } else if i + release >= n {
            (n - i) as f32 / release as f32
        } else {
            1.0
        };
        (t * frequency * TAU).sin() * envelope * 0.7
    }))
}

/// A fading 400 → 2000 Hz sweep, as WAV bytes.
fn chirp() -> Vec<u8> {
    let n = (SAMPLE_RATE as f32 * 0.15) as usize;
    wav((0..n).map(|i| {
        let t = i as f32 / SAMPLE_RATE as f32;
        let progress = i as f32 / n as f32;
        (t * (400.0 + progress * 1600.0) * TAU).sin() * (1.0 - progress) * 0.7
    }))
}

/// Encode mono samples in `-1.0..=1.0` as a 16-bit PCM WAV file.
fn wav(samples: impl ExactSizeIterator<Item = f32>) -> Vec<u8> {
    let data_size = samples.len() as u32 * 2;
    let mut wav = Vec::with_capacity(44 + data_size as usize);

    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_size).to_le_bytes());
    wav.extend_from_slice(b"WAVE");

    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());

    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size.to_le_bytes());
    for sample in samples {
        wav.extend_from_slice(&((sample * 32767.0) as i16).to_le_bytes());
    }
    wav
}

/// A hand-drawn button: returns `true` when it was clicked this frame.
fn button(x: f32, y: f32, w: f32, h: f32, label: &str) -> bool {
    let (mx, my) = mouse_position();
    let hover = mx >= x && mx < x + w && my >= y && my < y + h;
    let shade = if hover { 80 } else { 60 };
    canvas_rounded_rect(x, y, w, h, 6.0, shade, shade, shade + 30, 255);
    // The 14 px label's line box is 1.2 × its size tall, starting at `y`.
    canvas_text_ex(
        x + w / 2.0,
        y + (h - 14.0 * 1.2) / 2.0,
        14.0,
        235,
        235,
        245,
        255,
        "",
        0,
        FONT_STYLE_NORMAL,
        TEXT_ALIGN_CENTER,
        label,
    );
    hover && mouse_button_clicked(0)
}
