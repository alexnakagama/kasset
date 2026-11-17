//! The single table of keys and what they do. `kasset --help`, the in-app
//! help overlay and the bottom key bar are all built from [`GROUPS`], so the
//! three can never disagree.
//!
//! Invariants (checked by the tests below):
//! - Every entry's `keys` column is ASCII, so the `--help` text lines up in
//!   any terminal; bar labels may use arrows (`↑/↓`).
//! - A context's bar lists its labels in table order. Groups are ordered so
//!   that each view's own keys (library, sidebar, playlist) come before the
//!   shared transport keys, because the bar is cut at the terminal's width.
//! - Every [`BarContext`] has a non-empty bar, and no bar label is wider
//!   than 24 columns.
//!
//! All text here is static and trusted; nothing from tags, file names or
//! playlists ever passes through this module.

use std::fmt::Write as _;

/// The hint drawn at the right end of the bar in Normal mode.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "drawn by the key bar (task 5.2)")
)]
pub const HELP_HINT: &str = "? help";

/// Widest bar label allowed, in terminal columns.
#[cfg(test)]
const MAX_BAR_LABEL_WIDTH: usize = 24;

/// Where a key bar is shown: a Normal-mode view, a prompt, or the overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarContext {
    /// Normal mode, main pane showing the library.
    Library,
    /// Normal mode, main pane showing a playlist.
    Playlist,
    /// Normal mode, sidebar focused.
    Sidebar,
    /// Typing a search query.
    Search,
    /// Choosing the playlist to add tracks to.
    Picker,
    /// Typing the name of a new playlist.
    Name,
    /// Typing a playlist's new name.
    Rename,
    /// Asking whether to delete a playlist.
    ConfirmDelete,
    /// The help overlay.
    Help,
}

impl BarContext {
    /// Every context, for exhaustive checks.
    #[cfg(test)]
    const ALL: [Self; 9] = [
        Self::Library,
        Self::Playlist,
        Self::Sidebar,
        Self::Search,
        Self::Picker,
        Self::Name,
        Self::Rename,
        Self::ConfirmDelete,
        Self::Help,
    ];
}

use BarContext::{ConfirmDelete, Help, Library, Name, Picker, Playlist, Rename, Search, Sidebar};

/// The three Normal-mode contexts.
const NORMAL: &[BarContext] = &[Library, Playlist, Sidebar];

/// A short label for the key bar and the contexts whose bar shows it.
#[derive(Debug)]
pub struct Bar {
    pub label: &'static str,
    pub contexts: &'static [BarContext],
}

/// One line of the help: the key(s), what they do, and the bar labels
/// that stand for this entry in particular contexts.
#[derive(Debug)]
pub struct Entry {
    pub keys: &'static str,
    pub action: &'static str,
    pub bar: &'static [Bar],
}

/// A titled group of entries in the help.
#[derive(Debug)]
pub struct Group {
    pub title: &'static str,
    pub entries: &'static [Entry],
}

/// Shorthand for the table below.
const fn shown(label: &'static str, contexts: &'static [BarContext]) -> Bar {
    Bar { label, contexts }
}

/// Every key, grouped for the help and ordered for the bars (see the
/// module doc).
pub const GROUPS: &[Group] = &[
    Group {
        title: "Library",
        entries: &[
            Entry {
                keys: "Enter",
                action: "play the selected track (in the library or a playlist)",
                bar: &[shown("Enter play", &[Library, Playlist])],
            },
            Entry {
                keys: "a / A",
                action: "add the selected / every listed track to a playlist",
                bar: &[
                    shown("a/A add", &[Library]),
                    shown("↑/↓ choose", &[Picker]),
                    shown("type a name", &[Name]),
                    shown("Enter add", &[Picker]),
                    shown("Enter create", &[Name]),
                    shown("Esc cancel", &[Picker, Name]),
                ],
            },
        ],
    },
    Group {
        title: "Sidebar & playlists",
        entries: &[
            Entry {
                keys: "Enter",
                action: "in the sidebar: show the selected entry in the main pane and focus it",
                bar: &[shown("Enter open", &[Sidebar])],
            },
            Entry {
                keys: "d",
                action: "in a playlist: remove the selected entry",
                bar: &[shown("d remove", &[Playlist])],
            },
            Entry {
                keys: "J / K",
                action: "in a playlist: move the selected entry down / up",
                bar: &[shown("J/K move", &[Playlist])],
            },
            Entry {
                keys: "R",
                action: "in the sidebar: rename the selected playlist",
                bar: &[
                    shown("R rename", &[Sidebar]),
                    shown("type a name", &[Rename]),
                    shown("Enter rename", &[Rename]),
                    shown("Esc cancel", &[Rename]),
                ],
            },
            Entry {
                keys: "D",
                action: "in the sidebar: delete the selected playlist (asks y/n; songs stay)",
                bar: &[
                    shown("D delete", &[Sidebar]),
                    shown("y delete", &[ConfirmDelete]),
                    shown("any other key cancel", &[ConfirmDelete]),
                ],
            },
            Entry {
                keys: "Esc",
                action: "in a playlist: clear the filter, then focus the sidebar",
                bar: &[shown("Esc sidebar", &[Playlist])],
            },
            Entry {
                keys: "Tab, Shift+Tab",
                action: "move focus between the sidebar and the main pane",
                bar: &[shown("Tab focus", NORMAL)],
            },
        ],
    },
    Group {
        title: "Playback",
        entries: &[
            Entry {
                keys: "Space",
                action: "pause / resume",
                bar: &[shown("Space pause", NORMAL)],
            },
            Entry {
                keys: "s",
                action: "stop",
                bar: &[shown("s stop", NORMAL)],
            },
            Entry {
                keys: "n / p",
                action: "next / previous track (p restarts the track after 3 s)",
                bar: &[shown("n/p next/prev", NORMAL)],
            },
            Entry {
                keys: "Left / Right",
                action: "seek -5 s / +5 s",
                bar: &[shown("←/→ seek", NORMAL)],
            },
            Entry {
                keys: "+ (=) / -",
                action: "volume up / down in 5 % steps",
                bar: &[shown("+/- volume", NORMAL)],
            },
            Entry {
                keys: "z",
                action: "toggle shuffle",
                bar: &[shown("z shuffle", NORMAL)],
            },
            Entry {
                keys: "r",
                action: "cycle repeat (off / all / one)",
                bar: &[shown("r repeat", NORMAL)],
            },
        ],
    },
    Group {
        title: "Navigation & search",
        entries: &[
            Entry {
                keys: "/",
                action: "search the focused list (Enter keeps the filter, Esc clears it)",
                bar: &[
                    shown("/ search", NORMAL),
                    shown("type to filter", &[Search]),
                    shown("Enter keep", &[Search]),
                    shown("Esc clear", &[Search]),
                ],
            },
            Entry {
                keys: "Esc",
                action: "clear an active filter",
                bar: &[],
            },
            Entry {
                keys: "Up/Down, k/j",
                action: "move the selection",
                bar: &[shown("↑/↓ move", &[Search])],
            },
            Entry {
                keys: "PgUp / PgDn",
                action: "move the selection by a page",
                bar: &[],
            },
            Entry {
                keys: "g/Home, G/End",
                action: "first / last row",
                bar: &[],
            },
        ],
    },
    Group {
        title: "Help & quit",
        entries: &[
            Entry {
                keys: "?",
                action: "show every key (Up/Down, k/j, PgUp/PgDn, g/G scroll; other keys close)",
                bar: &[
                    shown("↑/↓ scroll", &[Help]),
                    shown("any other key close", &[Help]),
                ],
            },
            Entry {
                keys: "q, Ctrl+C",
                action: "quit",
                bar: &[shown("q quit", NORMAL), shown("Ctrl+C quit", &[Search])],
            },
        ],
    },
];

/// One line of the help as drawn, top to bottom.
#[derive(Debug, Clone, Copy)]
pub enum HelpLine {
    /// A group title.
    Title(&'static str),
    /// One key entry.
    Key(&'static Entry),
    /// The empty line between two groups.
    Blank,
}

/// The lines of the help, in order: each group's title and entries, with
/// one blank line between groups. The overlay draws exactly these lines;
/// `--help` prints them one to one between a two-line header and a
/// two-line footer (see [`help_text`]).
pub fn help_lines() -> impl Iterator<Item = HelpLine> {
    GROUPS.iter().enumerate().flat_map(|(i, group)| {
        let blank = (i > 0).then_some(HelpLine::Blank);
        blank
            .into_iter()
            .chain(std::iter::once(HelpLine::Title(group.title)))
            .chain(group.entries.iter().map(HelpLine::Key))
    })
}

/// How many lines the help has.
#[must_use]
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used by the help overlay (task 3.1)")
)]
pub fn help_line_count() -> usize {
    help_lines().count()
}

/// Width of the widest `keys` column (ASCII, so bytes are columns).
fn keys_width() -> usize {
    help_lines()
        .filter_map(|line| match line {
            HelpLine::Key(entry) => Some(entry.keys.len()),
            HelpLine::Title(_) | HelpLine::Blank => None,
        })
        .max()
        .unwrap_or(0)
}

/// First lines of the `--help` key summary, before [`help_lines`].
const HELP_HEADER: &str = "Keys (press ? in kasset to see them there too):\n\n";

/// Last lines of the `--help` key summary, after [`help_lines`].
const HELP_FOOTER: &str = "\nPlaylists are stored as .m3u8 files in <DIR>/Playlists.";

/// The key summary printed after `kasset --help`: [`HELP_HEADER`], one line
/// per [`help_lines`] entry, then [`HELP_FOOTER`].
#[must_use]
pub fn help_text() -> String {
    let width = keys_width();
    let mut text = String::from(HELP_HEADER);
    for line in help_lines() {
        // Writing to a `String` cannot fail.
        let _ = match line {
            HelpLine::Title(title) => writeln!(text, "{title}:"),
            HelpLine::Key(entry) => writeln!(text, "  {:width$}  {}", entry.keys, entry.action),
            HelpLine::Blank => writeln!(text),
        };
    }
    text.push_str(HELP_FOOTER);
    text
}

/// The labels of `context`'s key bar, in the order they are drawn.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "drawn by the key bar (task 5.2)")
)]
pub fn bar(context: BarContext) -> Vec<&'static str> {
    GROUPS
        .iter()
        .flat_map(|group| group.entries)
        .flat_map(|entry| entry.bar)
        .filter(|bar| bar.contexts.contains(&context))
        .map(|bar| bar.label)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::text::Span;

    #[test]
    fn every_context_has_a_bar() {
        for context in BarContext::ALL {
            assert!(!bar(context).is_empty(), "{context:?} has no bar");
        }
    }

    #[test]
    fn bar_labels_are_short() {
        for entry in GROUPS.iter().flat_map(|group| group.entries) {
            for bar in entry.bar {
                let width = Span::raw(bar.label).width();
                assert!(
                    width <= MAX_BAR_LABEL_WIDTH,
                    "{:?} is {width} columns",
                    bar.label
                );
                assert!(!bar.contexts.is_empty(), "{:?} is never shown", bar.label);
            }
        }
    }

    #[test]
    fn no_context_repeats_a_label() {
        for context in BarContext::ALL {
            let labels = bar(context);
            for (i, label) in labels.iter().enumerate() {
                assert!(
                    !labels[..i].contains(label),
                    "{context:?} shows {label:?} twice"
                );
            }
        }
    }

    #[test]
    fn view_bars_put_their_own_keys_first() {
        assert_eq!(bar(Library)[..2], ["Enter play", "a/A add"]);
        assert_eq!(bar(Sidebar)[..3], ["Enter open", "R rename", "D delete"]);
        assert_eq!(bar(Playlist)[..3], ["Enter play", "d remove", "J/K move"]);
    }

    #[test]
    fn prompt_bars() {
        assert_eq!(bar(Picker), ["↑/↓ choose", "Enter add", "Esc cancel"]);
        assert_eq!(bar(Name), ["type a name", "Enter create", "Esc cancel"]);
        assert_eq!(bar(Rename), ["type a name", "Enter rename", "Esc cancel"]);
        assert_eq!(bar(ConfirmDelete), ["y delete", "any other key cancel"]);
        assert_eq!(bar(Help), ["↑/↓ scroll", "any other key close"]);
        assert_eq!(
            bar(Search),
            [
                "type to filter",
                "Enter keep",
                "Esc clear",
                "↑/↓ move",
                "Ctrl+C quit"
            ]
        );
    }

    #[test]
    fn no_prompt_bar_offers_normal_mode_keys() {
        for context in [Search, Picker, Name, Rename, ConfirmDelete, Help] {
            for label in bar(context) {
                for &normal in NORMAL {
                    assert!(
                        !bar(normal).contains(&label),
                        "{context:?} shows the {normal:?} label {label:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn help_hint_is_not_a_bar_label() {
        for context in BarContext::ALL {
            assert!(!bar(context).contains(&HELP_HINT));
        }
    }

    #[test]
    fn keys_columns_are_ascii_and_filled() {
        for group in GROUPS {
            assert_ne!(group.title, "");
            assert!(!group.entries.is_empty(), "{} is empty", group.title);
            for entry in group.entries {
                assert!(entry.keys.is_ascii(), "{:?} is not ASCII", entry.keys);
                assert_ne!(entry.keys, "");
                assert_ne!(entry.action, "");
            }
        }
    }

    #[test]
    fn help_line_count_matches_the_lines_drawn() {
        let entries: usize = GROUPS.iter().map(|group| group.entries.len()).sum();
        // A title per group, its entries, and a blank line between groups.
        let expected = GROUPS.len() + entries + GROUPS.len() - 1;
        assert_eq!(help_line_count(), expected);
        assert!(matches!(
            help_lines().next(),
            Some(HelpLine::Title("Library"))
        ));
        assert!(matches!(help_lines().last(), Some(HelpLine::Key(_))));
    }

    #[test]
    fn help_text_lists_every_group_and_entry_aligned() {
        let text = help_text();
        for group in GROUPS {
            assert!(
                text.contains(&format!("\n{}:\n", group.title)),
                "{}",
                group.title
            );
        }
        // Header, then exactly one line per help line, then the footer.
        let body = text
            .strip_prefix(HELP_HEADER)
            .and_then(|rest| rest.strip_suffix(HELP_FOOTER));
        let Some(body) = body else {
            panic!("--help keys are not framed by the header and footer");
        };
        assert_eq!(body.lines().count(), help_line_count());

        // Every action starts in the same column, after a gutter of at
        // least two spaces.
        let entries: Vec<&Entry> = GROUPS.iter().flat_map(|group| group.entries).collect();
        let key_lines: Vec<&str> = body.lines().filter(|l| l.starts_with("  ")).collect();
        assert_eq!(key_lines.len(), entries.len());
        let column = 2 + keys_width() + 2;
        for (line, entry) in key_lines.iter().zip(&entries) {
            assert_eq!(
                line.get(2..2 + entry.keys.len()),
                Some(entry.keys),
                "{line:?}"
            );
            assert_eq!(line.get(column..), Some(entry.action), "{line:?}");
            assert!(line[2 + entry.keys.len()..column].chars().all(|c| c == ' '));
        }
    }
}
