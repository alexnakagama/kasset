//! The colour scheme: every style the screen uses, by role. `ui.rs` draws
//! with these roles only and holds no colour or emphasis of its own, so
//! each colour keeps one meaning across the whole screen.
//!
//! Roles: [`PLAYING`] (green), the selections ([`SELECTED_FOCUSED`],
//! [`SELECTED_UNFOCUSED`]), the pane borders and titles ([`BORDER_FOCUSED`],
//! [`TITLE_FOCUSED`], [`BORDER_UNFOCUSED`]), [`DIM`] text, [`ERROR`] (red),
//! [`PAUSED`] (yellow), [`STOPPED`], [`PROGRESS`] (cyan, like the focused
//! border: both mark what is active), [`SPECTRUM`] (magenta), and the
//! colourless [`STRONG`], [`HEADER`], [`SECTION`] and [`MESSAGE`].
//!
//! Invariants:
//! - Only the terminal's named ANSI colours are used, so the scheme follows
//!   the user's terminal theme.
//! - Colour is never the only cue. The playing row also has a marker, and
//!   the two selection styles differ in emphasis (reversed vs. bold and
//!   underlined) as well as strength.
//! - The unfocused border uses the default colour, not `DarkGray`, which is
//!   invisible on some dark themes (e.g. Solarized).

use ratatui::style::{Color, Modifier, Style};

/// The track (or sidebar entry) that is playing.
pub const PLAYING: Style = Style::new().fg(Color::Green).add_modifier(Modifier::BOLD);

/// The selected row of the focused pane.
pub const SELECTED_FOCUSED: Style = Style::new().add_modifier(Modifier::REVERSED);

/// The selected row of the pane without focus: visible, but clearly
/// secondary, and not told apart from the playing row by colour alone.
pub const SELECTED_UNFOCUSED: Style =
    Style::new().add_modifier(Modifier::BOLD.union(Modifier::UNDERLINED));

/// The border of the focused pane.
pub const BORDER_FOCUSED: Style = Style::new().fg(Color::Cyan);

/// The title of the focused pane (drawn on its border).
pub const TITLE_FOCUSED: Style = BORDER_FOCUSED.add_modifier(Modifier::BOLD);

/// The border and title of a pane without focus.
pub const BORDER_UNFOCUSED: Style = Style::new();

/// Secondary text: hints, read-only names, the key bar.
pub const DIM: Style = Style::new().fg(Color::DarkGray);

/// Error messages.
pub const ERROR: Style = Style::new().fg(Color::Red).add_modifier(Modifier::BOLD);

/// The "paused" state.
pub const PAUSED: Style = Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD);

/// The "stopped" state.
pub const STOPPED: Style = DIM.add_modifier(Modifier::BOLD);

/// The filled part of the progress bar.
pub const PROGRESS: Style = Style::new().fg(Color::Cyan);

/// The spectrum strip's bars. Their height carries the information, so the
/// colour is decoration only; magenta is used by nothing else.
pub const SPECTRUM: Style = Style::new().fg(Color::Magenta);

/// Text that stands out without a colour: popup titles, the playing title,
/// the search prompt.
pub const STRONG: Style = Style::new().add_modifier(Modifier::BOLD);

/// Column headers of a track table. Bold only: never underlined, since it
/// sits right above the first row and must not look like
/// [`SELECTED_UNFOCUSED`]; and colourless, since a structural label must
/// not depend on `DarkGray`, which some themes draw as the background.
pub const HEADER: Style = Style::new().add_modifier(Modifier::BOLD);
/// Group titles in the help overlay, which has no selection to confuse
/// them with.
pub const SECTION: Style = Style::new().add_modifier(Modifier::BOLD.union(Modifier::UNDERLINED));

/// A message drawn in place of an empty list, and placeholder text.
pub const MESSAGE: Style = Style::new().add_modifier(Modifier::ITALIC);

/// The selected row's style in a pane with or without focus.
#[must_use]
pub const fn selected(focused: bool) -> Style {
    if focused {
        SELECTED_FOCUSED
    } else {
        SELECTED_UNFOCUSED
    }
}

/// A pane's border and title styles, with or without focus.
#[must_use]
pub const fn border(focused: bool) -> (Style, Style) {
    if focused {
        (BORDER_FOCUSED, TITLE_FOCUSED)
    } else {
        (BORDER_UNFOCUSED, BORDER_UNFOCUSED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selections_differ_by_more_than_colour() {
        assert!(SELECTED_FOCUSED.add_modifier.contains(Modifier::REVERSED));
        assert!(!SELECTED_UNFOCUSED.add_modifier.contains(Modifier::REVERSED));
        assert!(
            SELECTED_UNFOCUSED
                .add_modifier
                .contains(Modifier::UNDERLINED)
        );
        assert_eq!(SELECTED_UNFOCUSED.fg, None);
        assert_ne!(PLAYING.fg, None, "playing is a colour, selection is not");
    }

    #[test]
    fn unfocused_border_is_not_dark_gray() {
        assert_ne!(BORDER_UNFOCUSED.fg, Some(Color::DarkGray));
        assert_ne!(border(true), border(false));
    }

    #[test]
    fn spectrum_colour_has_no_other_meaning() {
        assert_eq!(SPECTRUM.fg, Some(Color::Magenta));
        for other in [
            PLAYING,
            SELECTED_FOCUSED,
            SELECTED_UNFOCUSED,
            BORDER_FOCUSED,
            TITLE_FOCUSED,
            BORDER_UNFOCUSED,
            DIM,
            ERROR,
            PAUSED,
            STOPPED,
            PROGRESS,
            STRONG,
            HEADER,
            SECTION,
            MESSAGE,
        ] {
            assert_ne!(other.fg, SPECTRUM.fg, "{other:?}");
        }
    }

    #[test]
    fn header_does_not_look_like_a_selection() {
        for selection in [SELECTED_FOCUSED, SELECTED_UNFOCUSED] {
            assert_ne!(HEADER, selection);
        }
        assert!(!HEADER.add_modifier.contains(Modifier::UNDERLINED));
        assert!(!HEADER.add_modifier.contains(Modifier::REVERSED));
        assert_eq!(HEADER.fg, None, "headers must not depend on a colour");
    }
}
