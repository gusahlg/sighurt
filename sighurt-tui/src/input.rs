//! Mapping between terminal cells and the page, and the address editor.
//!
//! Owns: the arithmetic that turns a mouse cell into a page pixel, and the
//! question of whether a key belongs to the address editor. Must not own: the
//! terminal, the keymap, or what the editor does with the key.

use sighurt_api::input::KeyEvent;

/// The page's rectangle, in terminal cells. `column` and `row` are the
/// top-left cell, 0-based.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageCells {
    /// Left column of the page.
    pub column: u16,
    /// Top row of the page.
    pub row: u16,
    /// Width in cells.
    pub columns: u16,
    /// Height in cells.
    pub rows: u16,
}

/// Pixel size of one terminal cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellPixels {
    /// Pixels across one cell.
    pub width: u16,
    /// Pixels down one cell.
    pub height: u16,
}

/// The page pixel at the top-left of the cell (`column`, `row`).
///
/// `None` when the cell is outside `page`, or when a cell has no pixel size.
/// The pixel is relative to the page's top-left, not the terminal's.
pub fn cell_to_page_pixel(
    column: u16,
    row: u16,
    page: PageCells,
    cell: CellPixels,
) -> Option<(u32, u32)> {
    if cell.width == 0 || cell.height == 0 || page.columns == 0 || page.rows == 0 {
        return None;
    }
    if column < page.column || row < page.row {
        return None;
    }
    let x = column - page.column;
    let y = row - page.row;
    if x >= page.columns || y >= page.rows {
        return None;
    }
    Some((
        u32::from(x) * u32::from(cell.width),
        u32::from(y) * u32::from(cell.height),
    ))
}

/// Whether `event` is for the address editor.
///
/// True while the address line is being edited, except a Logo chord, which is
/// left for the keymap. Presses and releases both stay with the editor, so a
/// key is not also delivered to the page.
pub fn key_goes_to_editor(address_editing: bool, event: &KeyEvent) -> bool {
    address_editing && !event.modifiers.logo
}

#[cfg(test)]
mod tests {
    use super::*;
    use sighurt_api::input::{ButtonState, Key, KeyCode, Modifiers};

    fn key(state: ButtonState, logo: bool) -> KeyEvent {
        KeyEvent {
            code: KeyCode(30),
            key: Key::Character("a".into()),
            state,
            repeat: false,
            modifiers: Modifiers {
                logo,
                ..Modifiers::default()
            },
            consumed: Modifiers::default(),
            text: None,
        }
    }

    #[test]
    fn mouse_cell_maps_to_the_page_pixel() {
        let page = PageCells {
            column: 1,
            row: 1,
            columns: 10,
            rows: 5,
        };
        let cell = CellPixels {
            width: 8,
            height: 16,
        };
        assert_eq!(cell_to_page_pixel(2, 3, page, cell), Some((8, 32)));
        assert_eq!(cell_to_page_pixel(0, 3, page, cell), None);
        assert_eq!(cell_to_page_pixel(11, 1, page, cell), None);
        assert_eq!(
            cell_to_page_pixel(
                2,
                3,
                page,
                CellPixels {
                    width: 0,
                    height: 16
                }
            ),
            None
        );
    }

    #[test]
    fn editor_keys_stay_with_the_editor() {
        let press = key(ButtonState::Pressed, false);
        let release = key(ButtonState::Released, false);
        let logo = key(ButtonState::Pressed, true);
        assert!(key_goes_to_editor(true, &press));
        assert!(key_goes_to_editor(true, &release));
        assert!(!key_goes_to_editor(true, &logo));
        assert!(!key_goes_to_editor(false, &press));
    }
}
