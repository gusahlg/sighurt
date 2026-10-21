//! Mapping between terminal cells and the page, and the address editor.
//!
//! Owns: the arithmetic that turns a mouse cell into a page pixel, which view a drag that left
//! the page still belongs to, and the question of whether a key belongs to the address editor. Must not own: the
//! terminal, the keymap, or what the editor does with the key.

use sighurt_api::geometry::SurfacePoint;
use sighurt_api::ids::ViewId;
use sighurt_api::input::{ButtonState, KeyEvent, PointerButton};

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

/// The pixel at the top-left of the cell (`column`, `row`), relative to the page's top-left, for
/// a cell anywhere on the screen: negative above and left of the page, past its size below and
/// right. A drag that left the page still has a position on it. `None` when a cell has no pixel
/// size.
pub fn cell_to_page_offset(
    column: u16,
    row: u16,
    page: PageCells,
    cell: CellPixels,
) -> Option<(i64, i64)> {
    if cell.width == 0 || cell.height == 0 {
        return None;
    }
    Some((
        (i64::from(column) - i64::from(page.column)) * i64::from(cell.width),
        (i64::from(row) - i64::from(page.row)) * i64::from(cell.height),
    ))
}

/// A captured button let go of by the frontend rather than by the user.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Release {
    /// The view that held it.
    pub view: ViewId,
    /// The button.
    pub button: PointerButton,
    /// Where the pointer last was, relative to the page.
    pub position: SurfacePoint,
}

/// The pointer capture of buttons pressed over a view: until a button is released, its release
/// and the pointer's motion belong to the view, wherever on the screen they happen.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PointerCapture {
    held: Vec<(ViewId, PointerButton)>,
    last: SurfacePoint,
}

impl PointerCapture {
    /// `button` went down over `view`.
    pub fn press(&mut self, view: ViewId, button: PointerButton) {
        self.held.retain(|&(_, held)| held != button);
        self.held.push((view, button));
    }

    /// The pointer is at `position`, relative to the page.
    pub fn moved(&mut self, position: SurfacePoint) {
        self.last = position;
    }

    /// Nothing is captured on `view` any more because it is gone.
    pub fn forget(&mut self, view: ViewId) {
        self.held.retain(|&(held, _)| held != view);
    }

    /// The view captured by the oldest held button, if any.
    pub fn held(&self) -> Option<ViewId> {
        self.held.first().map(|&(view, _)| view)
    }

    /// Ends every capture, or those on `view`: the releases to send, since the user's will not
    /// arrive (the mode changed, the view hid, the mouse was turned off, focus left).
    pub fn cancel(&mut self, view: Option<ViewId>) -> Vec<Release> {
        let last = self.last;
        let mut out = Vec::new();
        self.held.retain(|&(held, button)| {
            let cancelled = view.is_none_or(|v| v == held);
            if cancelled {
                out.push(Release {
                    view: held,
                    button,
                    position: last,
                });
            }
            !cancelled
        });
        out
    }

    /// The view a motion, or a change of a captured button, belongs to. A release of the button
    /// ends its capture. Wheel reports and other buttons are not captured.
    pub fn route(
        &mut self,
        button: Option<PointerButton>,
        motion: bool,
        state: ButtonState,
    ) -> Option<(ViewId, PointerButton)> {
        if motion {
            return self.held.first().copied();
        }
        let at = self
            .held
            .iter()
            .position(|&(_, held)| Some(held) == button)?;
        match state {
            ButtonState::Released => Some(self.held.remove(at)),
            ButtonState::Pressed => Some(self.held[at]),
        }
    }
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
    fn a_cell_off_the_page_has_an_offset_from_it() {
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
        assert_eq!(cell_to_page_offset(2, 3, page, cell), Some((8, 32)));
        assert_eq!(cell_to_page_offset(0, 0, page, cell), Some((-8, -16)));
        assert_eq!(cell_to_page_offset(20, 9, page, cell), Some((152, 128)));
        let none = CellPixels {
            width: 0,
            height: 16,
        };
        assert_eq!(cell_to_page_offset(2, 3, page, none), None);
    }

    #[test]
    fn a_pressed_button_owns_the_pointer_until_it_is_released() {
        let (view, other) = (ViewId::next(), ViewId::next());
        let mut capture = PointerCapture::default();
        use PointerButton::{Left, Right};
        assert_eq!(
            capture.route(Some(Left), false, ButtonState::Released),
            None
        );
        capture.press(view, Left);
        // Motion off the page, and another button, first.
        assert_eq!(
            capture.route(Some(Left), true, ButtonState::Pressed),
            Some((view, Left))
        );
        assert_eq!(
            capture.route(Some(Right), false, ButtonState::Pressed),
            None
        );
        assert_eq!(
            capture.route(Some(Right), false, ButtonState::Released),
            None
        );
        // The release arrives wherever the pointer is, once.
        assert_eq!(
            capture.route(Some(Left), false, ButtonState::Released),
            Some((view, Left))
        );
        assert_eq!(
            capture.route(Some(Left), false, ButtonState::Released),
            None
        );
        // A closed view's capture is dropped.
        capture.press(view, Left);
        capture.forget(other);
        assert!(capture.held().is_some());
        capture.forget(view);
        assert_eq!(capture.held(), None);
    }

    #[test]
    fn two_buttons_are_held_apart_and_cancelling_releases_each() {
        let (a, b) = (ViewId::next(), ViewId::next());
        let mut capture = PointerCapture::default();
        use PointerButton::{Left, Right};
        capture.press(a, Left);
        capture.press(b, Right);
        // The second press did not overwrite the first.
        assert_eq!(
            capture.route(Some(Left), false, ButtonState::Released),
            Some((a, Left))
        );
        capture.press(a, Left);
        capture.moved(SurfacePoint { x: 3.0, y: -4.0 });
        let position = SurfacePoint { x: 3.0, y: -4.0 };
        assert_eq!(
            capture.cancel(Some(b)),
            [Release {
                view: b,
                button: Right,
                position
            }]
        );
        assert_eq!(capture.held(), Some(a));
        assert_eq!(capture.cancel(None).len(), 1);
        assert_eq!(capture.held(), None);
        assert!(capture.cancel(None).is_empty());
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
