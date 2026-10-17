//! A PNG writer with no dependencies: RGBA8, filter None, zlib stored blocks.
//!
//! Stored deflate is enough for the two frames this experiment keeps. The writer reads the file
//! back, checks the chunk CRCs and inflates those stored blocks, so a bad encoder fails the run
//! instead of leaving an unreadable file.

use std::fs::File;
use std::io::Write;
use std::path::Path;

/// Writes `rgba` (tightly packed, 4 bytes per pixel) and checks the file inflates to the same
/// pixels.
pub fn write_rgba(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
    let row = width as usize * 4;
    let rows = height as usize;
    if row == 0 || rgba.len() != row * rows {
        return Err(format!("png: {width}x{height} does not match {} bytes", rgba.len()));
    }
    let mut raw = Vec::with_capacity(rows * (row + 1));
    for y in 0..rows {
        raw.push(0);
        raw.extend_from_slice(&rgba[y * row..(y + 1) * row]);
    }
    let zlib = zlib_store(&raw);
    let mut png = Vec::with_capacity(64 + zlib.len());
    png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut png, *b"IHDR", &ihdr);
    chunk(&mut png, *b"IDAT", &zlib);
    chunk(&mut png, *b"IEND", &[]);
    let mut file = File::create(path).map_err(|e| format!("png {}: {e}", path.display()))?;
    file.write_all(&png)
        .map_err(|e| format!("png {}: {e}", path.display()))?;
    verify(&png, width, height, rgba).map_err(|e| format!("png {}: {e}", path.display()))
}

fn chunk(out: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(&kind);
    out.extend_from_slice(data);
    let mut crc_bytes = Vec::with_capacity(4 + data.len());
    crc_bytes.extend_from_slice(&kind);
    crc_bytes.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_bytes).to_be_bytes());
}

fn zlib_store(data: &[u8]) -> Vec<u8> {
    // CMF/FLG 0x78 0x01: deflate, window 32K, no preset dictionary, fastest. 0x7801 % 31 == 0.
    let mut out = vec![0x78, 0x01];
    let mut offset = 0;
    loop {
        let n = (data.len() - offset).min(65535);
        let last = offset + n == data.len();
        // Byte-aligned stored block: BFINAL in bit 0, BTYPE 00 in bits 1–2, then LEN and NLEN.
        out.push(u8::from(last));
        let n16 = n as u16;
        out.extend_from_slice(&n16.to_le_bytes());
        out.extend_from_slice(&(!n16).to_le_bytes());
        out.extend_from_slice(&data[offset..offset + n]);
        offset += n;
        if last {
            break;
        }
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn verify(png: &[u8], width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
    if png.len() < 8 || &png[..8] != b"\x89PNG\r\n\x1a\n" {
        return Err("bad signature".into());
    }
    let mut i = 8;
    let mut idat = Vec::new();
    let mut saw_ihdr = false;
    let mut saw_iend = false;
    while i + 12 <= png.len() {
        let len = u32::from_be_bytes(png[i..i + 4].try_into().unwrap()) as usize;
        if i + 12 + len > png.len() {
            return Err("truncated chunk".into());
        }
        let kind = &png[i + 4..i + 8];
        let data = &png[i + 8..i + 8 + len];
        let crc = u32::from_be_bytes(png[i + 8 + len..i + 12 + len].try_into().unwrap());
        let mut crc_bytes = Vec::with_capacity(4 + data.len());
        crc_bytes.extend_from_slice(kind);
        crc_bytes.extend_from_slice(data);
        if crc32(&crc_bytes) != crc {
            return Err(format!("bad crc {}", String::from_utf8_lossy(kind)));
        }
        match kind {
            b"IHDR" => {
                if data.len() < 13 {
                    return Err("short IHDR".into());
                }
                let w = u32::from_be_bytes(data[0..4].try_into().unwrap());
                let h = u32::from_be_bytes(data[4..8].try_into().unwrap());
                if w != width || h != height || data[8] != 8 || data[9] != 6 {
                    return Err(format!("IHDR {w}x{h} depth {} color {}", data[8], data[9]));
                }
                saw_ihdr = true;
            }
            b"IDAT" => idat.extend_from_slice(data),
            b"IEND" => saw_iend = true,
            _ => {}
        }
        i += 12 + len;
        if saw_iend {
            break;
        }
    }
    if !saw_ihdr || !saw_iend {
        return Err("missing IHDR or IEND".into());
    }
    let raw = inflate_store(&idat)?;
    let row = width as usize * 4;
    if raw.len() != height as usize * (row + 1) {
        return Err(format!("inflated {} bytes", raw.len()));
    }
    for y in 0..height as usize {
        let start = y * (row + 1);
        if raw[start] != 0 {
            return Err(format!("row {y} filter {}", raw[start]));
        }
        if raw[start + 1..start + 1 + row] != rgba[y * row..(y + 1) * row] {
            return Err(format!("row {y} pixels differ"));
        }
    }
    Ok(())
}

fn inflate_store(zlib: &[u8]) -> Result<Vec<u8>, String> {
    if zlib.len() < 6 || zlib[0] != 0x78 || zlib[1] != 0x01 {
        return Err("unexpected zlib header".into());
    }
    let mut i = 2;
    let mut out = Vec::new();
    loop {
        if i >= zlib.len() {
            return Err("truncated deflate".into());
        }
        let header = zlib[i];
        i += 1;
        if (header >> 1) & 0b11 != 0 {
            return Err("deflate block is not stored".into());
        }
        if i + 4 > zlib.len() {
            return Err("truncated stored header".into());
        }
        let n = u16::from_le_bytes([zlib[i], zlib[i + 1]]);
        let nlen = u16::from_le_bytes([zlib[i + 2], zlib[i + 3]]);
        i += 4;
        if nlen != !n {
            return Err("stored block length check".into());
        }
        let n = n as usize;
        if i + n > zlib.len() {
            return Err("truncated stored block".into());
        }
        out.extend_from_slice(&zlib[i..i + n]);
        i += n;
        if header & 1 == 1 {
            break;
        }
    }
    if i + 4 != zlib.len() {
        return Err("zlib trailer size".into());
    }
    let got = u32::from_be_bytes(zlib[i..i + 4].try_into().unwrap());
    if got != adler32(&out) {
        return Err("adler32".into());
    }
    Ok(out)
}

fn adler32(data: &[u8]) -> u32 {
    let mut a = 1u32;
    let mut b = 0u32;
    for &byte in data {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}
