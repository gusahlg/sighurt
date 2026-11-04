//! Colors: [`Rgba`], straight-alpha 8-bit colors as a chrome names them and the configuration's
//! `[ui.colors]` writes them.
//!
//! Must not own: blending or a pixel format (each backend's), or which color means what (the
//! chrome's).

/// A color, with straight (not premultiplied) alpha.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgba {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
    /// Opacity.
    pub a: u8,
}

impl Rgba {
    /// An opaque color from `0xrrggbb`.
    pub const fn rgb(hex: u32) -> Self {
        Self::rgb_alpha(hex << 8 | 0xff)
    }

    /// A color from `0xrrggbbaa`.
    pub const fn rgb_alpha(hex: u32) -> Self {
        let [r, g, b, a] = hex.to_be_bytes();
        Self { r, g, b, a }
    }

    /// Parses `"#rrggbb"` or `"#rrggbbaa"`.
    pub fn parse(text: &str) -> Option<Self> {
        let digits = text.strip_prefix('#')?;
        if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let value = u32::from_str_radix(digits, 16).ok()?;
        match digits.len() {
            6 => Some(Self::rgb(value)),
            8 => Some(Self::rgb_alpha(value)),
            _ => None,
        }
    }

    /// The same color, opaque.
    pub const fn opaque(self) -> Self {
        Self { a: 255, ..self }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_colors() {
        assert_eq!(Rgba::parse("#0a0a0b"), Some(Rgba::rgb(0x0a0a0b)));
        assert_eq!(Rgba::parse("#60a5fa55"), Some(Rgba::rgb_alpha(0x60a5fa55)));
        for bad in ["0a0a0b", "#fff", "#+a0a0b", "#0a0a0g", "#0a0a0b5"] {
            assert_eq!(Rgba::parse(bad), None, "{bad}");
        }
    }
}
