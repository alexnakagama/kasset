//! The single table of keys and what they do. `kasset --help`, the in-app
//! help overlay and the bottom key bar are all built from [`GROUPS`], so the
//! three can never disagree.
//!
//! Invariants (checked by the tests below):
//! - Every entry's `keys` column is ASCII, so the `--help` text lines up in
//!   any terminal; bar labels may use arrows (`↑/↓`).
//! - A context's bar lists its labels in table order. Groups are ordered so
//!   that each view's own keys (library, sidebar, playlist, browse levels)
//!   come before the shared transport keys, because the bar is cut at the
//!   terminal's width.
//! - Every [`BarContext`] has a non-empty bar, and no bar label is wider
//!   than 24 columns.
//!
//! All text here is static and trusted; nothing from tags, file names or
//! playlists ever passes through this module.

use std::fmt::Write as _;

/// The hint drawn at the right end of the bar in Normal mode.
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
    /// Normal mode, main pane showing the artist or the album list (the
    /// first browse level, where `Esc` goes to the sidebar).
    BrowseTop,
    /// Normal mode, main pane showing an artist's albums.
    BrowseGroups,
    /// Normal mode, main pane showing a browsed list of tracks (an album, or
    /// an artist's tracks).
    BrowseTracks,
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
    const ALL: [Self; 12] = [
        Self::Library,
        Self::Playlist,
        Self::Sidebar,
        Self::BrowseTop,
        Self::BrowseGroups,
        Self::BrowseTracks,
        Self::Search,
        Self::Picker,
        Self::Name,
        Self::Rename,
        Self::ConfirmDelete,
        Self::Help,
    ];

    /// Whether this is a Normal-mode context (a view, not a prompt or the
    /// overlay): its bar ends with [`HELP_HINT`].
    #[must_use]
    pub fn is_normal(self) -> bool {
        NORMAL.contains(&self)
    }
}

use BarContext::{
    BrowseGroups, BrowseTop, BrowseTracks, ConfirmDelete, Help, Library, Name, Picker, Playlist,
    Rename, Search, Sidebar,
};

/// The Normal-mode contexts.
const NORMAL: &[BarContext] = &[
    Library,
    Playlist,
    Sidebar,
    BrowseTop,
    BrowseGroups,
    BrowseTracks,
];

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
                action: "play the selected track (in the library, a playlist, an album or an \
                         artist's tracks)",
                bar: &[shown("Enter play", &[Library, Playlist, BrowseTracks])],
            },
            Entry {
                keys: "a / A",
                action: "add the selected / every listed track to a playlist",
                bar: &[
                    shown("a/A add", &[Library, BrowseTracks]),
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
                action: "open the selected sidebar entry, artist, album or All tracks in the \
                         main pane (and focus it)",
                bar: &[shown("Enter open", &[Sidebar, BrowseTop, BrowseGroups])],
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
                action: "in a playlist or the artist / album list: clear the filter, then \
                         focus the sidebar",
                bar: &[shown("Esc sidebar", &[Playlist, BrowseTop])],
            },
            Entry {
                keys: "Tab, Shift+Tab",
                action: "move focus between the sidebar and the main pane",
                bar: &[shown("Tab focus", NORMAL)],
            },
        ],
    },
    Group {
        title: "Browse",
        entries: &[
            Entry {
                keys: "Esc / Backspace",
                action: "back one level, to the entry opened (Esc clears a filter first)",
                bar: &[shown("Esc back", &[BrowseGroups, BrowseTracks])],
            },
            Entry {
                keys: "a",
                action: "on an artist, album or All tracks: add all its tracks to a playlist",
                bar: &[shown("a add", &[BrowseTop, BrowseGroups])],
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
                action: "clear an active filter (before going back or to the sidebar)",
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
        title: "Display",
        entries: &[
            Entry {
                keys: "v",
                action: "show / hide the visualizer",
                bar: &[],
            },
            Entry {
                keys: "l",
                action: "show / hide the lyrics",
                bar: &[],
            },
        ],
    },
    Group {
        title: "Help & quit",
        entries: &[
            Entry {
                keys: "?",
                action: "show every key (Up/Down, k/j, PgUp/PgDn, g/Home, G/End scroll; other keys close)",
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
pub fn help_line_count() -> usize {
    help_lines().count()
}

/// Width of the widest `keys` column (ASCII, so bytes are columns).
#[must_use]
pub fn keys_width() -> usize {
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
#[must_use]
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
        assert_eq!(bar(BrowseTop)[..2], ["Enter open", "Esc sidebar"]);
        assert_eq!(bar(BrowseGroups)[..1], ["Enter open"]);
        assert_eq!(bar(BrowseTracks)[..2], ["Enter play", "a/A add"]);
    }

    /// Every view's own keys come before the shared transport keys, which
    /// start with `Space pause`.
    #[test]
    fn own_keys_come_before_the_transport_keys() {
        let own: [(BarContext, &[&str]); 6] = [
            (Library, &["Enter play", "a/A add"]),
            (
                Playlist,
                &["Enter play", "d remove", "J/K move", "Esc sidebar"],
            ),
            (Sidebar, &["Enter open", "R rename", "D delete"]),
            (BrowseTop, &["Enter open", "Esc sidebar", "a add"]),
            (BrowseGroups, &["Enter open", "Esc back", "a add"]),
            (BrowseTracks, &["Enter play", "a/A add", "Esc back"]),
        ];
        for (context, labels) in own {
            let bar = bar(context);
            let Some(transport) = bar.iter().position(|&l| l == "Space pause") else {
                panic!("{context:?} has no transport keys");
            };
            for label in labels {
                let at = bar.iter().position(|l| l == label);
                assert!(
                    at.is_some_and(|at| at < transport),
                    "{context:?}: {label:?} in {bar:?}"
                );
            }
            assert!(context.is_normal(), "{context:?}");
        }
    }

    /// Spec *Artist list bar*: `Esc` goes to the sidebar there, and back a
    /// level deeper down.
    #[test]
    fn browse_bars_name_where_esc_goes() {
        assert!(!bar(BrowseTop).contains(&"Esc back"));
        assert!(!bar(BrowseGroups).contains(&"Esc sidebar"));
        assert!(!bar(BrowseTracks).contains(&"Esc sidebar"));
        assert!(!bar(BrowseTracks).contains(&"a add"), "a/A add instead");
        assert!(!bar(Library).contains(&"Esc back"));
    }

    /// Spec *Help lists back*.
    #[test]
    fn help_lists_going_back() {
        let text = help_text();
        let Some(line) = text
            .lines()
            .find(|l| l.trim_start().starts_with("Esc / Backspace"))
        else {
            panic!("--help does not list Esc / Backspace: {text}");
        };
        assert!(line.contains("back one level"), "{line}");
        assert!(text.contains("\nBrowse:\n"), "{text}");
        let titles: Vec<&str> = GROUPS.iter().map(|group| group.title).collect();
        let sidebar = titles.iter().position(|&t| t == "Sidebar & playlists");
        let browse = titles.iter().position(|&t| t == "Browse");
        assert_eq!(browse, sidebar.map(|s| s + 1), "{titles:?}");
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
    fn the_visualizer_key_is_in_the_help_but_not_the_bars() {
        let action = "show / hide the visualizer";
        let text = help_text();
        let Some(text_line) = text.lines().find(|l| l.ends_with(action)) else {
            panic!("--help does not list the visualizer key");
        };
        assert_eq!(text_line.split_whitespace().next(), Some("v"));
        assert!(help_lines().any(|line| matches!(
            line,
            HelpLine::Key(Entry { keys: "v", action: a, .. }) if *a == action
        )));
        let titles: Vec<&str> = GROUPS.iter().map(|group| group.title).collect();
        assert_eq!(titles[titles.len() - 2..], ["Display", "Help & quit"]);
        for context in BarContext::ALL {
            assert!(!bar(context).iter().any(|label| label.starts_with("v ")));
        }
    }

    #[test]
    fn the_lyrics_key_is_in_the_help_after_v_but_not_the_bars() {
        let action = "show / hide the lyrics";
        let text = help_text();
        let Some(text_line) = text.lines().find(|l| l.ends_with(action)) else {
            panic!("--help does not list the lyrics key");
        };
        assert_eq!(text_line.split_whitespace().next(), Some("l"));
        let keys: Vec<&str> = help_lines()
            .filter_map(|line| match line {
                HelpLine::Key(entry) => Some(entry.keys),
                HelpLine::Title(_) | HelpLine::Blank => None,
            })
            .collect();
        let Some(v) = keys.iter().position(|&k| k == "v") else {
            panic!("no `v` entry");
        };
        assert_eq!(keys.get(v + 1), Some(&"l"), "right after `v`");
        assert!(help_lines().any(|line| matches!(
            line,
            HelpLine::Key(Entry { keys: "l", action: a, .. }) if *a == action
        )));
        let display = GROUPS.iter().find(|group| group.title == "Display");
        assert_eq!(
            display.map(|group| group.entries.iter().map(|e| e.keys).collect::<Vec<_>>()),
            Some(vec!["v", "l"])
        );
        for context in BarContext::ALL {
            assert!(!bar(context).iter().any(|label| label.starts_with("l ")));
        }
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
