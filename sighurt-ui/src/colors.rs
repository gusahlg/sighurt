//! The chrome's colors: Sighurt's look by default, each overridable from `[ui.colors]`.

use gpui::{rgb, rgba, Rgba};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Colors {
    /// Window and page background.
    pub bg: Rgba,
    /// URL bar and the home page's card.
    pub surface: Rgba,
    /// Buttons under the pointer.
    pub hover: Rgba,
    /// The selected tab.
    pub tab: Rgba,
    /// Lines between areas.
    pub border: Rgba,
    /// Border of the focused URL bar.
    pub focus: Rgba,
    pub text: Rgba,
    /// Secondary text and icons.
    pub text_muted: Rgba,
    /// Hints and disabled buttons.
    pub text_dim: Rgba,
    /// Selected text in the URL bar.
    pub selection: Rgba,
    /// Error messages.
    pub error: Rgba,
}

impl Default for Colors {
    fn default() -> Self {
        Self {
            bg: rgb(0x0a0a0b),
            surface: rgb(0x18181b),
            hover: rgb(0x27272a),
            tab: rgb(0x27272a),
            border: rgb(0x27272a),
            focus: rgb(0xd4d4d8),
            text: rgb(0xfafafa),
            text_muted: rgb(0xa1a1aa),
            text_dim: rgb(0x71717a),
            selection: rgba(0x60a5fa55),
            error: rgb(0xdc2626),
        }
    }
}

impl Colors {
    /// The defaults with `ui`'s (the config's `[ui]` section) `colors` table applied. Unknown
    /// settings and malformed colors are reported and skipped.
    pub fn new(ui: &toml::Table) -> Self {
        let mut colors = Self::default();
        for (key, value) in ui {
            let Some(table) = value.as_table().filter(|_| key == "colors") else {
                tracing::warn!("[ui]: unknown setting {key:?}");
                continue;
            };
            for (name, value) in table {
                match (colors.field(name), value.as_str().and_then(hex)) {
                    (Some(field), Some(color)) => *field = color,
                    (None, _) => tracing::warn!("[ui.colors]: unknown color {name:?}"),
                    (_, None) => tracing::warn!(
                        "[ui.colors]: {name} should be \"#rrggbb\" or \"#rrggbbaa\", not {value}"
                    ),
                }
            }
        }
        colors
    }

    fn field(&mut self, name: &str) -> Option<&mut Rgba> {
        Some(match name {
            "bg" => &mut self.bg,
            "surface" => &mut self.surface,
            "hover" => &mut self.hover,
            "tab" => &mut self.tab,
            "border" => &mut self.border,
            "focus" => &mut self.focus,
            "text" => &mut self.text,
            "text_muted" => &mut self.text_muted,
            "text_dim" => &mut self.text_dim,
            "selection" => &mut self.selection,
            "error" => &mut self.error,
            _ => return None,
        })
    }
}

/// Parses "#rrggbb" or "#rrggbbaa".
fn hex(text: &str) -> Option<Rgba> {
    let digits = text.strip_prefix('#')?;
    if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let value = u32::from_str_radix(digits, 16).ok()?;
    match digits.len() {
        6 => Some(rgb(value)),
        8 => Some(rgba(value)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_overrides_defaults() {
        let ui: toml::Table = toml::from_str(
            r##"
            colors.bg = "#000000"
            colors.selection = "#ff000080"
            colors.text = "white"
            colors.nope = "#123456"
            theme = "light"
            "##,
        )
        .unwrap();
        let colors = Colors::new(&ui);
        assert_eq!(colors.bg, rgb(0x000000));
        assert_eq!(colors.selection, rgba(0xff000080));
        // Malformed or unknown: skipped.
        assert_eq!(colors.text, Colors::default().text);
        assert_eq!(colors.border, Colors::default().border);
        assert_eq!(Colors::new(&toml::Table::new()), Colors::default());
    }

    #[test]
    fn parses_hex_colors() {
        assert_eq!(hex("#0a0a0b"), Some(rgb(0x0a0a0b)));
        assert_eq!(hex("#60a5fa55"), Some(rgba(0x60a5fa55)));
        for bad in ["0a0a0b", "#fff", "#+a0a0b", "#0a0a0g", "#0a0a0b5"] {
            assert_eq!(hex(bad), None, "{bad}");
        }
    }
}
