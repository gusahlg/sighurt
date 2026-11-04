//! Seeded random-input tests ("fuzz-lite") that run in an ordinary `cargo test`.
//!
//! This file is included, not depended on, by the crates' fuzz tests (`#[path =
//! ".../tools/fuzz/mod.rs"] mod fuzz;`), so it adds no crate and no dependency. It owns a
//! xorshift64 generator, the iteration loop (seed per case, replay, panic reporting) and a
//! watchdog that ends the test binary when one case runs longer than its limit. It does not
//! know any parser.
//!
//! Environment: `SIG_FUZZ_ITERATIONS` (default 2000, a multiplier of `scaled`), `SIG_FUZZ_SEED`
//! (the base seed; the failure message prints it) and `SIG_FUZZ_CASE` (run only that case).
#![allow(dead_code)]

use std::io::Write;
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A xorshift64* generator: small, deterministic, good enough to explore inputs.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        // splitmix64 the seed so that nearby seeds give unrelated streams, and never zero.
        let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        Self(if z == 0 { 0x1234_5678_9ABC_DEF1 } else { z })
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A number in `0..n` (`0` when `n` is `0`).
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }

    /// A number in `lo..=hi`.
    pub fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + self.below(hi - lo + 1)
    }

    /// True once in `n`.
    pub fn one_in(&mut self, n: usize) -> bool {
        self.below(n) == 0
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }

    pub fn byte(&mut self) -> u8 {
        self.next_u64() as u8
    }

    pub fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.byte()).collect()
    }

    /// A char from a mix of ASCII, controls, Latin, CJK, emoji, bidi and format characters,
    /// the surrogate-adjacent and the last valid code points.
    pub fn char(&mut self) -> char {
        match self.below(12) {
            0..=3 => (b' ' + (self.byte() % 95)) as char,
            4 => char::from(self.byte() % 32),
            5 => *self.pick(&[
                '\u{7f}',
                '\u{85}',
                '\u{a0}',
                '\u{ad}',
                '\u{200b}',
                '\u{200d}',
                '\u{200e}',
                '\u{202e}',
                '\u{2028}',
                '\u{2029}',
                '\u{2066}',
                '\u{2069}',
                '\u{feff}',
                '\u{fffd}',
                '\u{ffff}',
                '\u{10ffff}',
                '\u{e000}',
                '\u{d7ff}',
                '\u{301}',
                '\u{0}',
            ]),
            6 => char::from_u32(0xC0 + self.below(0x300) as u32).unwrap_or('?'),
            7 => char::from_u32(0x4E00 + self.below(0x5000) as u32).unwrap_or('?'),
            8 => char::from_u32(0x1F300 + self.below(0x400) as u32).unwrap_or('?'),
            9 => char::from_u32(0x0600 + self.below(0x200) as u32).unwrap_or('?'),
            _ => loop {
                if let Some(c) = char::from_u32(self.below(0x11_0000) as u32) {
                    break c;
                }
            },
        }
    }

    pub fn string(&mut self, max_len: usize) -> String {
        let len = self.below(max_len + 1);
        (0..len).map(|_| self.char()).collect()
    }

    /// A string of `len` characters out of `alphabet`.
    pub fn string_from(&mut self, alphabet: &[&str], max_len: usize) -> String {
        let len = self.below(max_len + 1);
        (0..len).map(|_| *self.pick(alphabet)).collect()
    }

    /// `text` mutated a few ways: bytes flipped, a piece cut, duplicated, or random text and
    /// "escape" characters inserted. The result is valid UTF-8 (cut at char boundaries).
    pub fn mutate(&mut self, text: &str) -> String {
        let mut chars: Vec<char> = text.chars().collect();
        for _ in 0..self.range(1, 4) {
            let at = self.below(chars.len() + 1);
            match self.below(8) {
                0 if !chars.is_empty() => {
                    let at = at.min(chars.len() - 1);
                    chars[at] = self.char();
                }
                1 if !chars.is_empty() => {
                    chars.remove(at.min(chars.len() - 1));
                }
                2 => chars.truncate(at),
                3 => {
                    let end = (at + self.below(40)).min(chars.len());
                    let piece: Vec<char> = chars[at.min(end)..end].to_vec();
                    for _ in 0..self.range(1, 3) {
                        let pos = self.below(chars.len() + 1);
                        chars.splice(pos..pos, piece.iter().copied());
                    }
                }
                4 => {
                    let c = *self.pick(&[
                        '\\', '"', '\'', '\n', '\r', '\t', '\u{1b}', '[', ']', '{', '}', '=', '#',
                        '%', '\0', '.', '-', '\u{7f}', '\u{202e}',
                    ]);
                    chars.insert(at, c);
                }
                5 => {
                    let inserted = self.string(8);
                    chars.splice(at..at, inserted.chars());
                }
                6 if !chars.is_empty() => {
                    chars.drain(..at.min(chars.len()));
                }
                _ => {
                    let end = (at + self.below(8)).min(chars.len());
                    chars.drain(at.min(end)..end);
                }
            }
        }
        chars.into_iter().collect()
    }

    /// `bytes` mutated: bytes flipped, a piece cut or repeated, random bytes inserted.
    pub fn mutate_bytes(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut out = bytes.to_vec();
        for _ in 0..self.range(1, 4) {
            let at = self.below(out.len() + 1);
            match self.below(5) {
                0 if !out.is_empty() => {
                    let at = at.min(out.len() - 1);
                    out[at] ^= 1 << self.below(8);
                }
                1 => out.truncate(at),
                2 => {
                    let end = (at + self.below(64)).min(out.len());
                    let piece = out[at.min(end)..end].to_vec();
                    let pos = self.below(out.len() + 1);
                    out.splice(pos..pos, piece);
                }
                3 => {
                    let n = self.below(8);
                    let inserted = self.bytes(n);
                    out.splice(at..at, inserted);
                }
                _ if !out.is_empty() => {
                    out.remove(at.min(out.len() - 1));
                }
                _ => {}
            }
        }
        out
    }
}

/// The number of iterations: `SIG_FUZZ_ITERATIONS`, else 2000, and `percent` of it (a
/// heavier target runs fewer cases).
pub fn iterations(percent: usize) -> usize {
    let base = std::env::var("SIG_FUZZ_ITERATIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2000usize);
    (base * percent / 100).max(1)
}

/// One case: its generator, and the note the watchdog and the failure message print.
pub struct Case {
    pub rng: Rng,
    pub index: usize,
    note: Arc<Mutex<String>>,
}

impl Case {
    /// Records what is about to be tried (cut to 600 chars). Call it before the call that may
    /// fail, with a cheap `Debug` of the input.
    pub fn note(&self, what: impl FnOnce() -> String) {
        let mut text = what();
        if text.len() > 600 {
            let mut end = 600;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            text.push_str("...");
        }
        *self.note.lock().unwrap_or_else(|p| p.into_inner()) = text;
    }
}

/// Runs `cases` cases of `body`, each with its own seed derived from the base seed. A panic
/// prints the target, the base seed, the case index and the last note, then propagates. A case
/// running longer than `limit` ends the process with the same message (a hang is a failure).
pub fn run(name: &str, cases: usize, limit: Duration, mut body: impl FnMut(&mut Case)) {
    let base: u64 = std::env::var("SIG_FUZZ_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| {
            name.bytes().fold(0xCBF2_9CE4_8422_2325u64, |h, b| {
                (h ^ b as u64).wrapping_mul(0x100_0000_01B3)
            })
        });
    let only: Option<usize> = std::env::var("SIG_FUZZ_CASE")
        .ok()
        .and_then(|v| v.parse().ok());
    let note = Arc::new(Mutex::new(String::new()));
    let began = Instant::now();
    let started = Arc::new(AtomicU64::new(u64::MAX));
    let done = Arc::new(AtomicBool::new(false));
    let epoch = Instant::now();
    let current = Arc::new(AtomicU64::new(0));
    let watchdog = {
        let (note, started, done, current) =
            (note.clone(), started.clone(), done.clone(), current.clone());
        let name = name.to_owned();
        std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(25));
                let since = started.load(Ordering::Relaxed);
                if since != u64::MAX
                    && epoch.elapsed().as_millis() as u64 > since + limit.as_millis() as u64
                {
                    let note = note.lock().unwrap_or_else(|p| p.into_inner()).clone();
                    let message = format!(
                        "\nFUZZ HANG in {name}: a case ran over {limit:?}\n  replay: SIG_FUZZ_SEED={base} SIG_FUZZ_CASE={}\n  input: {note}\n",
                        current.load(Ordering::Relaxed)
                    );
                    let _ = std::io::stderr().write_all(message.as_bytes());
                    std::process::exit(101);
                }
            }
        })
    };
    for index in 0..cases {
        if only.is_some_and(|only| only != index) {
            continue;
        }
        let mut case = Case {
            rng: Rng::new(base ^ (index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)),
            index,
            note: note.clone(),
        };
        current.store(index as u64, Ordering::Relaxed);
        started.store(epoch.elapsed().as_millis() as u64, Ordering::Relaxed);
        let outcome = catch_unwind(AssertUnwindSafe(|| body(&mut case)));
        started.store(u64::MAX, Ordering::Relaxed);
        if let Err(panic) = outcome {
            let last = note.lock().unwrap_or_else(|p| p.into_inner()).clone();
            done.store(true, Ordering::Relaxed);
            let message = format!(
                "\nFUZZ FAILURE in {name}\n  replay: SIG_FUZZ_SEED={base} SIG_FUZZ_CASE={index}\n  input: {last}\n"
            );
            let _ = std::io::stderr().write_all(message.as_bytes());
            resume_unwind(panic);
        }
    }
    done.store(true, Ordering::Relaxed);
    let _ = watchdog.join();
    eprintln!("{name}: {cases} cases in {:?}", began.elapsed());
}
