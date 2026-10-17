//! The per-process name of the slot where a page world stores node ids.
//!
//! Owns: that unguessable name, and turning a Rust string into a JavaScript string literal.
//! Must not own: the walk, the actions or the snapshot.

use std::hash::BuildHasher;
use std::io::Read;
use std::sync::OnceLock;

/// The property name, stable for this process and not one a page can predict.
pub(crate) fn state_key() -> &'static str {
    static KEY: OnceLock<String> = OnceLock::new();
    KEY.get_or_init(generate_key)
}

/// [`state_key`] as a JavaScript string literal, quotes included.
pub(crate) fn state_key_literal() -> String {
    js_string_literal(state_key())
}

/// A JavaScript string literal for `value`.
///
/// JSON string syntax is a JavaScript string, except U+2028 and U+2029, which JSON leaves raw
/// and JavaScript treats as line breaks. Those two are written as `\u` escapes. Nothing of
/// `value` is concatenated into the script as raw source.
pub(crate) fn js_string_literal(value: &str) -> String {
    let json = serde_json::to_string(value).expect("a Rust string serializes to JSON");
    json.replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

fn generate_key() -> String {
    let mut bytes = [0u8; 16];
    let filled = match std::fs::File::open("/dev/urandom") {
        Ok(mut file) => file.read_exact(&mut bytes).is_ok(),
        Err(_) => false,
    };
    if !filled {
        fill_from_hasher(&mut bytes);
    }
    // Mix the process hasher seed in either case, so the name does not depend on urandom alone.
    let mixed = std::collections::hash_map::RandomState::new().hash_one(u128::from_le_bytes(bytes));
    let mut out = String::with_capacity(1 + 32 + 16);
    out.push('s');
    for byte in bytes {
        push_hex(&mut out, byte);
    }
    for shift in (0..64).step_by(8).rev() {
        push_hex(&mut out, (mixed >> shift) as u8);
    }
    out
}

fn fill_from_hasher(bytes: &mut [u8; 16]) {
    let hasher = std::collections::hash_map::RandomState::new();
    let hi = hasher.hash_one(1u64).to_le_bytes();
    let lo = hasher.hash_one(2u64).to_le_bytes();
    bytes[..8].copy_from_slice(&hi);
    bytes[8..].copy_from_slice(&lo);
}

fn push_hex(out: &mut String, byte: u8) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push(HEX[(byte >> 4) as usize] as char);
    out.push(HEX[(byte & 0x0f) as usize] as char);
}
