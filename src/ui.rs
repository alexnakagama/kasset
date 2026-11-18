//! Drawing the screen from the [`App`] state. Rendering never changes the app;
//! the only state kept here is what the event loop feeds back to it and the
//! panes' scroll positions ([`View`]).
//!
//! Layout, top to bottom: the body, the now-playing panel, the status line
//! and the key bar. The body is two panes side by side, the sidebar
//! ("Library" and the playlists) and the main pane (the library or the open
//! playlist), when the terminal is at least [`TWO_PANE_MIN_WIDTH`] columns
//! wide; below that only the focused pane is drawn, at full width. The
//! focused pane has the focused border and the strong selection style
//! ([`theme`]); each pane keeps its own scroll offset, and the page size fed
//! back to the app is the focused pane's. Popups are centred over the body.
//!
//! Every style comes from [`theme`]; this module holds no colour of its own.
//!
//! All text drawn comes from sanitised sources (tags and file names through
//! `text::sanitize`/`display_path`, playlist names through `text::sanitize`
//! here, typed text through input filtering), so no terminal control
//! sequence can reach the screen. Text that can be long (names, the library
//! root, the search query) is elided to its area, never wrapped into others.

use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Cell, Clear, Gauge, List, ListItem, ListState, Padding, Paragraph, Row, Table,
    TableState, Wrap,
};

use crate::app::{
    App, Focus, ListView, MainView, Mode, NameInput, NamePurpose, Picker, View as Screen,
};
use crate::playback::{Level, PlayState, clock};
use crate::player::Audio;
use crate::text::sanitize;
use crate::theme;

/// Height of the now-playing panel, borders included.
const NOW_PLAYING_HEIGHT: u16 = 6;

/// Narrowest terminal, in columns, that shows the sidebar and the main pane
/// side by side.
pub const TWO_PANE_MIN_WIDTH: u16 = 70;

/// Narrowest and widest sidebar in the two-pane layout, borders included.
const SIDEBAR_WIDTH: (u16, u16) = (22, 32);

// Each bar puts its view's own keys first: the bar is cut at the terminal's width.
const KEYS_LIBRARY: &str = "Enter play · a/A add selected/all to playlist · Tab focus · / search · \
                            Space pause · s stop · n/p next/prev · ←/→ seek · \
                            +/- volume · z shuffle · r repeat · q quit";
const KEYS_PLAYLISTS: &str = "Enter open · R rename · D delete · / search · Tab focus · \
                              Space pause · s stop · n/p next/prev · ←/→ seek · +/- volume · \
                              z shuffle · r repeat · q quit";
// The edit keys come first: the bar is cut at the terminal's width.
const KEYS_PLAYLIST: &str = "Enter play · d remove · J/K move · Esc sidebar · / search · \
                             Tab focus · Space pause · s stop · n/p next/prev · ←/→ seek · \
                             +/- volume · z shuffle · r repeat · q quit";
const KEYS_SEARCH: &str = "type to filter · Enter keep · Esc clear · ↑/↓ move · Ctrl+C quit";
const KEYS_PICKER: &str = "↑/↓ choose · Enter add · Esc cancel";
const KEYS_NAME: &str = "type a name · Enter create · Esc cancel";
const KEYS_RENAME: &str = "type a name · Enter rename · Esc cancel";
const KEYS_CONFIRM: &str = "y delete · any other key cancel";
const KEYS_HELP: &str = "↑/↓ scroll · any other key close";

/// Start of the library pane's title, before the library root.
const LIBRARY_PREFIX: &str = " Library · ";

/// The library pane's title when there is no room for the root.
const LIBRARY_TITLE: &str = " Library ";

/// The sidebar's title.
const SIDEBAR_TITLE: &str = " kasset ";

/// Name of the sidebar's first row.
const LIBRARY_ROW: &str = "Library";

/// Added to a read-only playlist's name in the main pane's title.
const READ_ONLY_SUFFIX: &str = " · read-only";

/// Label of the picker's first row.
pub const NEW_PLAYLIST: &str = "New playlist…";

/// Widest popup, in columns.
const POPUP_MAX_WIDTH: u16 = 60;

/// Width of `text` in terminal columns.
fn width(text: &str) -> usize {
    Span::raw(text).width()
}

/// `text` cut from the start to fit `max` columns, with `…` marking the cut.
fn elide_start(text: &str, max: usize) -> String {
    if width(text) <= max {
        return text.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    let mut used = 1; // the ellipsis
    let mut start = text.len();
    for (i, c) in text.char_indices().rev() {
        let w = width(&text[i..i + c.len_utf8()]);
        if used + w > max {
            break;
        }
        used += w;
        start = i;
    }
    format!("…{}", &text[start..])
}

/// `text` cut at the end to fit `max` columns, with `…` marking the cut.
fn elide_end(text: &str, max: usize) -> String {
    if width(text) <= max {
        return text.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    let mut used = 1; // the ellipsis
    let mut end = 0;
    for (i, c) in text.char_indices() {
        let w = width(&text[i..i + c.len_utf8()]);
        if used + w > max {
            break;
        }
        used += w;
        end = i + c.len_utf8();
    }
    format!("{}…", &text[..end])
}

/// A playlist name ready to draw.
fn shown_name(name: &str) -> String {
    sanitize(name).unwrap_or_default()
}

/// What rendering remembers between frames.
#[derive(Debug, Default)]
pub struct View {
    /// First playlist row on screen in the sidebar ("Library" is always
    /// drawn above them).
    sidebar_offset: usize,
    /// First row on screen in the main pane.
    main_offset: usize,
    /// Rows of the focused pane that fit on screen in the last frame.
    page_size: usize,
    /// Help lines the overlay showed when last drawn (0 until it is drawn).
    help_rows: usize,
}

impl View {
    /// Rows of the focused pane shown in the last frame (at least 1).
    #[must_use]
    pub const fn page_size(&self) -> usize {
        if self.page_size == 0 {
            1
        } else {
            self.page_size
        }
    }

    /// Help lines the overlay showed when last drawn (at least 1).
    #[must_use]
    pub const fn help_rows(&self) -> usize {
        if self.help_rows == 0 {
            1
        } else {
            self.help_rows
        }
    }
}

/// The key help for what is on screen.
fn key_help<A: Audio>(app: &App<A>) -> &'static str {
    match app.mode() {
        Mode::Search => KEYS_SEARCH,
        Mode::Picker(_) => KEYS_PICKER,
        Mode::NameInput(input) => match input.purpose() {
            NamePurpose::Create(_) => KEYS_NAME,
            NamePurpose::Rename(_) => KEYS_RENAME,
        },
        Mode::ConfirmDelete(_) => KEYS_CONFIRM,
        Mode::Help { .. } => KEYS_HELP,
        Mode::Normal => match app.view() {
            Screen::Library => KEYS_LIBRARY,
            Screen::Playlists => KEYS_PLAYLISTS,
            Screen::Playlist => KEYS_PLAYLIST,
        },
    }
}

/// Draws the whole screen.
pub fn render<A: Audio>(frame: &mut Frame, app: &App<A>, view: &mut View, now: Instant) {
    let [body, playing, status, keys] = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(NOW_PLAYING_HEIGHT),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    render_body(frame, app, view, body);
    render_now_playing(frame, app, playing);
    render_status(frame, app, status, now);
    frame.render_widget(Paragraph::new(key_help(app)).style(theme::DIM), keys);

    match app.mode() {
        Mode::Picker(picker) => render_picker(frame, picker, body),
        Mode::NameInput(input) => render_name_input(frame, input, body),
        Mode::ConfirmDelete(name) => render_confirm_delete(frame, name, body),
        // The help overlay is drawn from task 5.3 of the ui-refresh change.
        Mode::Normal | Mode::Search | Mode::Help { .. } => {}
    }
}

/// The sidebar's width for a body `body_width` columns wide in the two-pane
/// layout.
fn sidebar_width(body_width: u16) -> u16 {
    (body_width / 4).clamp(SIDEBAR_WIDTH.0, SIDEBAR_WIDTH.1)
}

/// Draws the sidebar and the main pane side by side, or only the focused
/// one on a narrow terminal, and records the focused pane's page size.
fn render_body<A: Audio>(frame: &mut Frame, app: &App<A>, view: &mut View, area: Rect) {
    let focus = app.focus();
    view.page_size = if area.width >= TWO_PANE_MIN_WIDTH {
        let [side, main] = Layout::horizontal([
            Constraint::Length(sidebar_width(area.width)),
            Constraint::Fill(1),
        ])
        .areas(area);
        let side_rows = render_sidebar(frame, app, &mut view.sidebar_offset, side, focus);
        let main_rows = render_main(frame, app, &mut view.main_offset, main, focus);
        match focus {
            Focus::Sidebar => side_rows,
            Focus::Main => main_rows,
        }
    } else {
        match focus {
            Focus::Sidebar => render_sidebar(frame, app, &mut view.sidebar_offset, area, focus),
            Focus::Main => render_main(frame, app, &mut view.main_offset, area, focus),
        }
    };
}

/// `" N <noun> "`, or how many of the `total` entries (`shown` of them
/// listed) match `list`'s filter, in at most `max` columns: the query is
/// dropped first, then the words.
fn count_title(list: &ListView, shown: usize, total: usize, noun: &str, max: usize) -> String {
    let candidates = if list.query.is_empty() {
        [
            format!(" {total} {noun} "),
            format!(" {total} "),
            String::new(),
        ]
    } else {
        [
            format!(" {shown} of {total} match '{}' ", list.query),
            format!(" {shown} of {total} "),
            format!(" {shown}/{total} "),
        ]
    };
    candidates
        .into_iter()
        .find(|text| width(text) <= max)
        .unwrap_or_default()
}

/// A bordered pane titled `title(room)` on the left, where `room` is the
/// number of columns `count` (drawn on the right) leaves free, styled as
/// focused or not.
fn pane_block<'a>(
    area: Rect,
    focused: bool,
    title: impl FnOnce(usize) -> String,
    count: String,
) -> Block<'a> {
    // Two borders, the count and a one-column gap.
    let room = usize::from(area.width).saturating_sub(2 + width(&count) + 1);
    let (border, title_style) = theme::border(focused);
    Block::bordered()
        .border_style(border)
        .title_style(title_style)
        .padding(Padding::horizontal(1))
        .title(title(room))
        .title_top(Line::from(count).right_aligned())
}

/// The count title of a pane `area_width` columns wide: as long as it fits
/// beside the borders.
fn pane_count(list: &ListView, shown: usize, total: usize, noun: &str, area_width: u16) -> String {
    count_title(
        list,
        shown,
        total,
        noun,
        usize::from(area_width).saturating_sub(2),
    )
}

/// The library pane's title in `free` columns: the root keeps its end, the
/// part that tells folders apart.
fn library_title(root: &str, free: usize) -> String {
    // The prefix, the trailing space and at least "…" plus one character.
    let fixed = width(LIBRARY_PREFIX) + 1;
    if !root.is_empty() && free >= fixed + 2 {
        format!("{LIBRARY_PREFIX}{} ", elide_start(root, free - fixed))
    } else if free >= width(LIBRARY_TITLE) {
        LIBRARY_TITLE.to_owned()
    } else {
        String::new()
    }
}

/// An open playlist pane's title in `room` columns: the name keeps its
/// start; the read-only suffix goes first when there is no room for it.
fn playlist_title(name: &str, read_only: bool, room: usize) -> String {
    let suffix = if read_only { READ_ONLY_SUFFIX } else { "" };
    let with_suffix = 2 + width(suffix); // the two spaces around
    if room >= with_suffix + 2 {
        format!(" {}{suffix} ", elide_end(name, room - with_suffix))
    } else if room >= 4 {
        format!(" {} ", elide_end(name, room - 2))
    } else {
        String::new()
    }
}

/// Draws `message` in place of an empty list; returns the rows it took.
fn render_empty(frame: &mut Frame, offset: &mut usize, inner: Rect, message: String) -> usize {
    *offset = 0;
    frame.render_widget(
        Paragraph::new(message)
            .style(theme::MESSAGE)
            .wrap(Wrap { trim: false }),
        inner,
    );
    usize::from(inner.height)
}

/// Draws the main pane; returns the list rows that fit.
fn render_main<A: Audio>(
    frame: &mut Frame,
    app: &App<A>,
    offset: &mut usize,
    area: Rect,
    focus: Focus,
) -> usize {
    let focused = focus == Focus::Main;
    match app.main_view() {
        MainView::Library => render_library(frame, app, offset, area, focused),
        MainView::Playlist => render_open_playlist(frame, app, offset, area, focused),
    }
}

fn render_library<A: Audio>(
    frame: &mut Frame,
    app: &App<A>,
    offset: &mut usize,
    area: Rect,
    focused: bool,
) -> usize {
    let list = app.main_list();
    let total = app.playback().tracks().len();
    let count = pane_count(&list, list.rows.len(), total, "tracks", area.width);
    let block = pane_block(area, focused, |room| library_title(app.root(), room), count);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if list.rows.is_empty() {
        let message = if total == 0 {
            format!("No music found in {}", app.root())
        } else {
            format!("No tracks match '{}'", list.query)
        };
        return render_empty(frame, offset, inner, message);
    }
    let current = app.playback().current();
    render_tracks(frame, app, &list, offset, inner, focused, |index| {
        (index, current == Some(index))
    })
}

fn render_open_playlist<A: Audio>(
    frame: &mut Frame,
    app: &App<A>,
    offset: &mut usize,
    area: Rect,
    focused: bool,
) -> usize {
    let Some(open) = app.open_playlist() else {
        return 0;
    };
    let list = app.main_list();
    let items = open.items();
    let name = shown_name(open.name());
    let read_only = open.playlist().editable().is_err();
    let count = pane_count(&list, list.rows.len(), items.len(), "tracks", area.width);
    let block = pane_block(
        area,
        focused,
        |room| playlist_title(&name, read_only, room),
        count,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if list.rows.is_empty() {
        let message = if !items.is_empty() {
            format!("No tracks match '{}'", list.query)
        } else if open.playlist().unmatched() > 0 {
            "None of this playlist's entries are in the library".to_owned()
        } else {
            "This playlist is empty. Add tracks from the library with a or A.".to_owned()
        };
        return render_empty(frame, offset, inner, message);
    }
    // Only the playlist that is the play order shows what is playing: the
    // same song may be listed here while it plays from elsewhere.
    let playback = app.playback();
    let playing_item = playback
        .is_playing_playlist(open.name())
        .then(|| playback.current_item())
        .flatten();
    render_tracks(frame, app, &list, offset, inner, focused, |item| {
        (items[item], playing_item == Some(item))
    })
}

/// Draws the rows of a track list; `row(value)` gives the library index of
/// the track a row shows and whether it is the one playing. Returns the
/// rows that fit.
fn render_tracks<A: Audio>(
    frame: &mut Frame,
    app: &App<A>,
    list: &ListView,
    offset: &mut usize,
    inner: Rect,
    focused: bool,
    row: impl Fn(usize) -> (usize, bool),
) -> usize {
    let shown = list.rows.len();
    // One line is the header.
    let rows = usize::from(inner.height.saturating_sub(1));
    let selected = list.selected.unwrap_or(0);
    *offset = scroll(*offset, selected, rows, shown);

    let tracks = app.playback().tracks();
    let state = app.playback().state();
    let end = offset.saturating_add(rows).min(shown);
    let body = list.rows[*offset..end].iter().filter_map(|&value| {
        let (index, playing) = row(value);
        let track = tracks.get(index)?;
        let length = track.duration.map(clock).unwrap_or_default();
        let row = Row::new([
            Cell::from(if playing { state_marker(state) } else { " " }),
            Cell::from(track.title.as_str()),
            Cell::from(track.artist.as_str()),
            Cell::from(track.album.as_str()),
            Cell::from(Line::from(length).right_aligned()),
        ]);
        Some(if playing {
            row.style(theme::PLAYING)
        } else {
            row
        })
    });
    let header = Row::new([
        Cell::from(""),
        Cell::from("Title"),
        Cell::from("Artist"),
        Cell::from("Album"),
        Cell::from(Line::from("Time").right_aligned()),
    ])
    .style(theme::HEADER);
    let table = Table::new(
        body,
        [
            Constraint::Length(1),
            Constraint::Fill(4),
            Constraint::Fill(3),
            Constraint::Fill(3),
            Constraint::Length(6),
        ],
    )
    .header(header)
    .row_highlight_style(theme::selected(focused));
    let mut state = TableState::new().with_selected(Some(selected - *offset));
    frame.render_stateful_widget(table, inner, &mut state);
    rows
}

/// One sidebar row: `marker`, the name (elided at the end, styled
/// `name_style`) and `count` right-aligned, `columns` columns in all. The
/// count is dropped when the name would get less than 4 columns.
fn sidebar_line<'a>(
    marker: &'a str,
    name: &str,
    name_style: Style,
    count: &str,
    columns: usize,
) -> Line<'a> {
    let lead = 2; // marker and a space
    let count_w = width(count);
    let (room, count) = if columns >= lead + 4 + 1 + count_w {
        (columns - lead - 1 - count_w, count)
    } else {
        (columns.saturating_sub(lead), "")
    };
    let name = elide_end(name, room);
    let gap = columns.saturating_sub(lead + width(&name) + width(count));
    Line::from(vec![
        Span::raw(marker),
        Span::raw(" "),
        Span::styled(name, name_style),
        Span::raw(" ".repeat(gap)),
        Span::raw(count.to_owned()),
    ])
}

/// What the sidebar says below "Library" when no playlist is listed.
fn sidebar_hint(no_playlists: bool, query: &str) -> Paragraph<'static> {
    let message = if no_playlists {
        "No playlists yet. In the library, press a (selected track) or A (all listed \
         tracks) and choose \"New playlist…\"."
            .to_owned()
    } else {
        format!("No playlists match '{query}'")
    };
    Paragraph::new(message)
        .style(theme::DIM)
        .wrap(Wrap { trim: false })
}

/// Draws the sidebar: the "Library" row, a separator when there is room,
/// then the playlists, or a hint when none is listed. "Library" stays on
/// top; only the playlists scroll (`offset` counts playlist rows). Returns
/// the rows that fit.
fn render_sidebar<A: Audio>(
    frame: &mut Frame,
    app: &App<A>,
    offset: &mut usize,
    area: Rect,
    focus: Focus,
) -> usize {
    let focused = focus == Focus::Sidebar;
    let list = app.sidebar_list();
    let infos = app.playlists();
    // (row in `list.rows`, index in `infos`) of each playlist row; value 0
    // is the "Library" row.
    let playlist_rows: Vec<(usize, usize)> = list
        .rows
        .iter()
        .enumerate()
        .filter_map(|(row, &value)| Some((row, value.checked_sub(1)?)))
        .collect();
    let count = pane_count(
        &list,
        playlist_rows.len(),
        infos.len(),
        "playlists",
        area.width,
    );
    let block = pane_block(
        area,
        focused,
        |room| {
            if room >= width(SIDEBAR_TITLE) {
                SIDEBAR_TITLE.to_owned()
            } else {
                String::new()
            }
        },
        count,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return 0;
    }

    let playback = app.playback();
    let state = playback.state();
    let playing = app.sidebar_playing();
    let selected_value = list.selected.and_then(|row| list.rows.get(row).copied());
    let columns = usize::from(inner.width);
    let selection = theme::selected(focused);
    let draw_row = |frame: &mut Frame, value: usize, name: &str, name_style, count: &str, y| {
        let is_playing = playing == Some(value);
        let marker = if is_playing { state_marker(state) } else { " " };
        let row_style = if is_playing {
            theme::PLAYING
        } else {
            Style::new()
        };
        // The playing style wins over a read-only name's dimming.
        let name_style = if is_playing { Style::new() } else { name_style };
        let line = sidebar_line(marker, name, name_style, count, columns).style(row_style);
        let at = Rect::new(inner.x, y, inner.width, 1);
        frame.render_widget(line, at);
        if selected_value == Some(value) {
            frame.buffer_mut().set_style(at, selection);
        }
    };

    let library_count = playback.tracks().len().to_string();
    draw_row(frame, 0, LIBRARY_ROW, Style::new(), &library_count, inner.y);

    // A separator under "Library" when at least one row fits below it.
    let mut rest = inner;
    rest.y += 1;
    rest.height -= 1;
    if rest.height >= 2 {
        frame.render_widget(
            Line::styled("─".repeat(columns), theme::DIM),
            Rect::new(rest.x, rest.y, rest.width, 1),
        );
        rest.y += 1;
        rest.height -= 1;
    }
    let rows = usize::from(rest.height);

    if playlist_rows.is_empty() {
        *offset = 0;
        frame.render_widget(sidebar_hint(infos.is_empty(), list.query), rest);
        return rows;
    }

    // With "Library" selected the playlists keep their scroll position.
    let selected = list
        .selected
        .and_then(|row| playlist_rows.iter().position(|&(r, _)| r == row))
        .unwrap_or(*offset);
    *offset = scroll(*offset, selected, rows, playlist_rows.len());
    let end = offset.saturating_add(rows).min(playlist_rows.len());
    for (y, &(_, i)) in (rest.y..).zip(&playlist_rows[*offset..end]) {
        let Some(info) = infos.get(i) else {
            continue;
        };
        let count = info
            .tracks
            .map_or_else(|| "?".to_owned(), |n| n.to_string());
        let name_style = if info.read_only.is_some() {
            theme::DIM
        } else {
            Style::new()
        };
        draw_row(frame, i + 1, &shown_name(&info.name), name_style, &count, y);
    }
    rows
}

/// A `width`×`height` rectangle centred in `area` (shrunk to fit it).
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

fn render_picker(frame: &mut Frame, picker: &Picker, area: Rect) {
    let tracks = picker.track_count();
    let title = format!(
        " Add {tracks} {} to ",
        if tracks == 1 { "track" } else { "tracks" }
    );
    let names: Vec<(String, bool)> = picker
        .choices()
        .iter()
        .map(|info| (shown_name(&info.name), info.read_only.is_some()))
        .collect();
    let widest = names
        .iter()
        .map(|(name, read_only)| width(name) + if *read_only { 12 } else { 0 })
        .chain([width(NEW_PLAYLIST), width(&title)])
        .max()
        .unwrap_or(0);
    // Borders, padding and the highlight symbol.
    let popup_width = u16::try_from(widest + 6)
        .unwrap_or(u16::MAX)
        .clamp(30, POPUP_MAX_WIDTH);
    let height = u16::try_from(names.len() + 3).unwrap_or(u16::MAX);
    let popup = centered(area, popup_width, height);

    let items = std::iter::once(ListItem::new(Line::styled(NEW_PLAYLIST, theme::MESSAGE))).chain(
        names.into_iter().map(|(name, read_only)| {
            if read_only {
                ListItem::new(Line::from(vec![
                    Span::styled(name, theme::DIM),
                    Span::styled(" (read-only)", theme::DIM),
                ]))
            } else {
                ListItem::new(Line::from(name))
            }
        }),
    );
    let list = List::new(items)
        .block(
            Block::bordered()
                .padding(Padding::horizontal(1))
                .title(Span::styled(title, theme::STRONG)),
        )
        .highlight_style(theme::SELECTED_FOCUSED);
    let mut state = ListState::default().with_selected(Some(picker.selected()));
    frame.render_widget(Clear, popup);
    frame.render_stateful_widget(list, popup, &mut state);
}

fn render_name_input(frame: &mut Frame, input: &NameInput, area: Rect) {
    let (title, hint) = match input.purpose() {
        NamePurpose::Create(_) => (" New playlist ", "Enter to create, Esc to cancel"),
        NamePurpose::Rename(_) => (" Rename playlist ", "Enter to rename, Esc to cancel"),
    };
    let popup = centered(area, POPUP_MAX_WIDTH, 4);
    let block = Block::bordered()
        .padding(Padding::horizontal(1))
        .title(Span::styled(title, theme::STRONG));
    let inner = block.inner(popup);
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);
    let [text_line, message_line] = Layout::vertical([Constraint::Length(1); 2]).areas(inner);

    // Show the end of a name too long for the line, so the cursor stays visible.
    let room = usize::from(text_line.width.saturating_sub(1));
    let text = tail_fitting(input.text(), room);
    frame.render_widget(Line::from(text), text_line);
    if text_line.width > 0 && text_line.height > 0 {
        let text_width = u16::try_from(width(text)).unwrap_or(u16::MAX);
        let x = text_line.x.saturating_add(text_width);
        frame.set_cursor_position(Position::new(x.min(text_line.right() - 1), text_line.y));
    }
    let message = input.error().map_or_else(
        || Line::styled(hint, theme::DIM),
        |error| Line::styled(error, theme::ERROR),
    );
    frame.render_widget(message, message_line);
}

/// The "delete playlist?" question.
fn render_confirm_delete(frame: &mut Frame, name: &str, area: Rect) {
    let popup = centered(area, POPUP_MAX_WIDTH, 4);
    let block = Block::bordered()
        .padding(Padding::horizontal(1))
        .title(Span::styled(" Delete playlist ", theme::STRONG));
    let inner = block.inner(popup);
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);
    let [question_line, hint_line] = Layout::vertical([Constraint::Length(1); 2]).areas(inner);
    // `Delete "` and `"?` around the name, which is cut to fit.
    let room = usize::from(question_line.width).saturating_sub(10);
    let question = format!("Delete \"{}\"?", elide_end(&shown_name(name), room));
    frame.render_widget(Line::from(question), question_line);
    frame.render_widget(
        Line::styled(
            "y deletes the file (songs stay), other keys cancel",
            theme::DIM,
        ),
        hint_line,
    );
}

/// The first row to show so that `selected` is on screen, moving the previous
/// `offset` as little as possible and never leaving blank rows at the bottom.
/// The result is always within the list (`< len`, or 0 when it is empty), so
/// `offset..` is a valid slice start even when no row fits (`rows == 0`).
fn scroll(offset: usize, selected: usize, rows: usize, len: usize) -> usize {
    if rows == 0 {
        return selected.min(len.saturating_sub(1));
    }
    let offset = offset.min(len.saturating_sub(rows)).min(selected);
    if selected >= offset + rows {
        selected + 1 - rows
    } else {
        offset
    }
}

const fn state_marker(state: PlayState) -> &'static str {
    match state {
        PlayState::Playing => "▶",
        PlayState::Paused => "‖",
        PlayState::Stopped => "■",
    }
}

fn render_now_playing<A: Audio>(frame: &mut Frame, app: &App<A>, area: Rect) {
    let block = Block::bordered()
        .padding(Padding::horizontal(1))
        .title(" Now playing ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [heading, details, progress, modes] =
        Layout::vertical([Constraint::Length(1); 4]).areas(inner);

    let playback = app.playback();
    let state = playback.state();
    let state_style = match state {
        PlayState::Playing => theme::PLAYING,
        PlayState::Paused => theme::PAUSED,
        PlayState::Stopped => theme::STOPPED,
    };
    let state_label = Span::styled(
        format!("{} {}", state_marker(state), state.label()),
        state_style,
    );

    if let Some(track) = playback.current_track() {
        frame.render_widget(
            Line::from(vec![
                state_label,
                Span::raw("  "),
                Span::styled(track.title.as_str(), theme::STRONG),
            ]),
            heading,
        );
        frame.render_widget(
            Line::from(format!("{} — {}", track.artist, track.album)),
            details,
        );
    } else {
        frame.render_widget(
            Line::from(vec![
                state_label,
                Span::raw("  "),
                Span::styled("select a track and press Enter", theme::MESSAGE),
            ]),
            heading,
        );
    }

    let position = playback.position();
    let duration = playback.duration();
    let label = format!(
        "{} / {}",
        clock(position),
        duration.map_or_else(|| "-:--".to_owned(), clock)
    );
    frame.render_widget(
        Gauge::default()
            .ratio(progress_ratio(position, duration))
            .label(label)
            .use_unicode(true)
            .gauge_style(theme::PROGRESS),
        progress,
    );

    let on_off = |on: bool| if on { "on" } else { "off" };
    frame.render_widget(
        Line::from(format!(
            "Volume {}%   Shuffle {}   Repeat {}",
            playback.volume().percent(),
            on_off(playback.shuffle()),
            playback.repeat().label()
        )),
        modes,
    );
}

/// Fraction of the track played, always within 0–1 (the gauge panics
/// otherwise); 0 when the length is unknown.
fn progress_ratio(position: Duration, duration: Option<Duration>) -> f64 {
    match duration {
        Some(d) if !d.is_zero() => (position.as_secs_f64() / d.as_secs_f64()).clamp(0.0, 1.0),
        _ => 0.0,
    }
}

fn render_status<A: Audio>(frame: &mut Frame, app: &App<A>, area: Rect, now: Instant) {
    let notice = app.status(now).map(|notice| {
        let style = match notice.level {
            Level::Info => Style::new(),
            Level::Error => theme::ERROR,
        };
        Line::styled(notice.text.as_str(), style)
    });
    if !matches!(app.mode(), Mode::Search) {
        if let Some(line) = notice {
            frame.render_widget(line, area);
        }
        return;
    }

    // Show the end of a query too long for the line, so the cursor stays visible.
    let room = usize::from(area.width.saturating_sub(2));
    let query = tail_fitting(app.query(), room);
    let query_width = u16::try_from(width(query)).unwrap_or(u16::MAX);
    frame.render_widget(
        Line::from(vec![Span::styled("/", theme::STRONG), Span::raw(query)]),
        area,
    );
    if area.width > 0 && area.height > 0 {
        let x = area.x.saturating_add(1).saturating_add(query_width);
        frame.set_cursor_position(Position::new(x.min(area.right() - 1), area.y));
    }
    // Messages (e.g. a track skipped while typing) go after the query, at the
    // right edge when there is room, so they are not missed during a search.
    if let Some(line) = notice {
        let used = query_width.saturating_add(4); // "/", cursor, gap
        let free = area.width.saturating_sub(used);
        if free > 0 {
            let len = u16::try_from(line.width()).unwrap_or(u16::MAX).min(free);
            let at = Rect::new(area.right() - len, area.y, len, area.height);
            frame.render_widget(line, at);
        }
    }
}

/// The longest suffix of `text` at most `width` columns wide.
fn tail_fitting(text: &str, max: usize) -> &str {
    text.char_indices()
        .map(|(i, _)| &text[i..])
        .find(|tail| width(tail) <= max)
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playlist::{
        FsStore, Playlist, PlaylistInfo, PlaylistStore, Resolver, SaveOutcome, StoreError,
    };
    use crate::test_support::{SilentAudio, no_store, track};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::style::{Color, Modifier};

    /// Renders `app` at `width`×`height` and returns the screen's cells.
    fn draw_buffer(app: &App<SilentAudio>, view: &mut View, width: u16, height: u16) -> Buffer {
        let mut terminal =
            Terminal::new(TestBackend::new(width, height)).unwrap_or_else(|never| match never {});
        let now = Instant::now();
        terminal
            .draw(|f| render(f, app, view, now))
            .unwrap_or_else(|never| match never {});
        terminal.backend().buffer().clone()
    }

    /// The lines of a drawn screen.
    fn lines(buffer: &Buffer) -> Vec<String> {
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    /// Renders `app` at `width`×`height` and returns the screen as lines.
    fn draw(app: &App<SilentAudio>, view: &mut View, width: u16, height: u16) -> Vec<String> {
        lines(&draw_buffer(app, view, width, height))
    }

    fn screen_contains(lines: &[String], needle: &str) -> bool {
        lines.iter().any(|l| l.contains(needle))
    }

    fn library(n: usize) -> App<SilentAudio> {
        let tracks = (0..n)
            .map(|i| track(&format!("Song {i:02}"), "Band", "LP"))
            .collect();
        App::new(
            tracks,
            SilentAudio::default(),
            no_store(),
            "/music".to_owned(),
            None,
        )
    }

    fn press(app: &mut App<SilentAudio>, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn elide_start_keeps_the_end_within_the_width() {
        assert_eq!(elide_start("/music", 6), "/music");
        assert_eq!(elide_start("/home/me/Music", 8), "…e/Music");
        assert_eq!(elide_start("/a/音楽", 4), "…楽");
        assert_eq!(elide_start("/a/音楽", 3), "…楽");
        assert_eq!(
            elide_start("/a/音楽", 2),
            "…",
            "a wide char never overflows"
        );
        assert_eq!(elide_start("/music", 1), "…");
        assert_eq!(elide_start("/music", 0), "");
    }

    #[test]
    fn long_root_is_elided_and_keeps_the_track_count() {
        let root = format!("/very/{}long/Music", "deep/".repeat(30));
        let app = App::new(
            vec![track("Song", "Band", "LP")],
            SilentAudio::default(),
            no_store(),
            root,
            None,
        );
        let screen = draw(&app, &mut View::default(), 60, 12);
        assert!(screen[0].contains("…"), "{screen:#?}");
        assert!(screen[0].contains("/long/Music ─ 1 tracks "), "{screen:#?}");
        // " 1 tracks " leaves exactly 2 columns for the root at width 27:
        // the ellipsis and the last character.
        let app = App::new(
            vec![track("Song", "Band", "LP")],
            SilentAudio::default(),
            no_store(),
            "/music".to_owned(),
            None,
        );
        let screen = draw(&app, &mut View::default(), 27, 12);
        assert!(screen[0].contains(" Library · …c "), "{screen:#?}");
        let screen = draw(&app, &mut View::default(), 26, 12);
        assert!(screen[0].contains(" Library "), "{screen:#?}");
        assert!(!screen[0].contains('…'), "{screen:#?}");
        // Too narrow for any title: only the count is left.
        let screen = draw(&app, &mut View::default(), 20, 12);
        assert!(!screen[0].contains("Library"), "{screen:#?}");
        assert!(screen[0].contains(" 1 tracks "), "{screen:#?}");
    }

    #[test]
    fn empty_library_shows_no_music_found() {
        let app = App::new(
            Vec::new(),
            SilentAudio::default(),
            no_store(),
            "/home/me/Music".to_owned(),
            None,
        );
        let screen = draw(&app, &mut View::default(), 80, 20);
        assert!(
            screen_contains(&screen, "No music found in /home/me/Music"),
            "{screen:#?}"
        );
    }

    #[test]
    fn now_playing_shows_track_progress_and_modes() {
        let mut tracks = vec![track("Song", "Band", "LP")];
        tracks[0].duration = Some(Duration::from_secs(210));
        let mut app = App::new(
            tracks,
            SilentAudio::default(),
            no_store(),
            "/m".to_owned(),
            None,
        );
        press(&mut app, KeyCode::Enter);
        app.playback_mut().audio_mut().position = Duration::from_secs(65);
        let screen = draw(&app, &mut View::default(), 80, 20);
        assert!(screen_contains(&screen, "▶ Playing  Song"), "{screen:#?}");
        assert!(screen_contains(&screen, "Band — LP"));
        assert!(screen_contains(&screen, "1:05 / 3:30"));
        assert!(screen_contains(
            &screen,
            "Volume 100%   Shuffle off   Repeat off"
        ));
        let ratio = progress_ratio(Duration::from_secs(65), Some(Duration::from_secs(210)));
        assert!((ratio - 0.31).abs() < 0.01, "{ratio}");
    }

    #[test]
    fn playing_and_selected_rows_are_marked_differently() {
        let mut app = library(6);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        for _ in 0..3 {
            press(&mut app, KeyCode::Down);
        }
        let mut terminal =
            Terminal::new(TestBackend::new(60, 20)).unwrap_or_else(|never| match never {});
        let mut view = View::default();
        let now = Instant::now();
        terminal
            .draw(|f| render(f, &app, &mut view, now))
            .unwrap_or_else(|never| match never {});
        let buffer = terminal.backend().buffer();
        let row_of = |needle: &str| {
            (0..buffer.area.height).find(|&y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .contains(needle)
            })
        };
        let playing = row_of("▶ Song 01").unwrap_or(0);
        let selected = row_of("Song 04").unwrap_or(0);
        assert!(playing > 0 && selected > 0);
        let title_x = 4; // border, padding, marker, spacing
        assert!(
            buffer[(title_x, selected)]
                .modifier
                .contains(Modifier::REVERSED)
        );
        assert!(
            !buffer[(title_x, playing)]
                .modifier
                .contains(Modifier::REVERSED)
        );
        assert_eq!(buffer[(title_x, playing)].fg, Color::Green);
    }

    #[test]
    fn jumping_to_the_end_scrolls_the_last_track_into_view() {
        let mut app = library(100);
        let mut view = View::default();
        draw(&app, &mut view, 60, 20);
        press(&mut app, KeyCode::End);
        let screen = draw(&app, &mut view, 60, 20);
        assert!(screen_contains(&screen, "Song 99"), "{screen:#?}");
        assert!(!screen_contains(&screen, "Song 00"));
        // Moving up a little keeps the view where it is.
        press(&mut app, KeyCode::Up);
        let again = draw(&app, &mut view, 60, 20);
        assert_eq!(screen, again, "same rows on screen");
    }

    #[test]
    fn page_size_follows_the_terminal_height() {
        let app = library(100);
        let mut view = View::default();
        draw(&app, &mut view, 60, 20);
        // 20 - 6 (now playing) - 2 (status, keys) - 2 (borders) - 1 (header)
        assert_eq!(view.page_size(), 9);
        draw(&app, &mut view, 60, 40);
        assert_eq!(view.page_size(), 29, "resize is picked up");
    }

    #[test]
    fn search_line_and_filter_title() {
        let mut app = library(12);
        app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        for c in "song 1".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let screen = draw(&app, &mut View::default(), 80, 20);
        assert!(screen_contains(&screen, "/song 1"));
        assert!(
            screen_contains(&screen, "2 of 12 match 'song 1'"),
            "{screen:#?}"
        );

        for c in "zz".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let screen = draw(&app, &mut View::default(), 80, 20);
        assert!(screen_contains(&screen, "No tracks match 'song 1zz'"));
    }

    #[test]
    fn messages_stay_visible_while_searching() {
        let mut app = library(3);
        app.playback_mut().audio_mut().broken = true;
        press(&mut app, KeyCode::Enter);
        app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        app.tick(Instant::now());
        let screen = draw(&app, &mut View::default(), 100, 20);
        let status = &screen[18];
        assert!(status.starts_with("/x"), "{status}");
        assert!(status.contains("Cannot play"), "{status}");
    }

    #[test]
    fn tiny_and_odd_sizes_do_not_panic() {
        let mut app = library(30);
        press(&mut app, KeyCode::Enter);
        app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        for (w, h) in [(0, 0), (1, 1), (2, 3), (10, 5), (80, 4), (5, 80), (300, 2)] {
            draw(&app, &mut View::default(), w, h);
        }
    }

    #[test]
    fn progress_ratio_is_always_in_range() {
        let s = Duration::from_secs;
        assert!(progress_ratio(s(10), None).abs() < f64::EPSILON);
        assert!(progress_ratio(s(10), Some(Duration::ZERO)).abs() < f64::EPSILON);
        assert!((progress_ratio(s(500), Some(s(200))) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn scroll_keeps_the_selection_visible() {
        assert_eq!(scroll(0, 0, 10, 100), 0);
        assert_eq!(scroll(0, 15, 10, 100), 6);
        assert_eq!(scroll(6, 10, 10, 100), 6, "no jump while visible");
        assert_eq!(scroll(6, 3, 10, 100), 3);
        assert_eq!(scroll(90, 2, 10, 5), 0, "list shrank under the view");
        assert_eq!(scroll(3, 7, 0, 100), 7);
        assert_eq!(scroll(9, 9, 0, 3), 2, "no row fits: still within the list");
        assert_eq!(scroll(9, 9, 0, 0), 0);
    }

    /// A library of A, B, C by "Band" under a temporary root, with playlists
    /// "Running" (B, C) and "Gym" (A, B) in it.
    fn with_playlists() -> anyhow::Result<(tempfile::TempDir, App<SilentAudio>)> {
        let tmp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(tmp.path())?;
        let tracks = ["A", "B", "C"]
            .into_iter()
            .map(|t| {
                let mut track = track(t, "Band", "LP");
                track.path = root.join(format!("Band/LP/{t}.mp3"));
                track
            })
            .collect();
        let dir = root.join("Playlists");
        std::fs::create_dir(&dir)?;
        std::fs::write(
            dir.join("Running.m3u8"),
            "../Band/LP/B.mp3\n../Band/LP/C.mp3\n",
        )?;
        std::fs::write(dir.join("Gym.m3u8"), "../Band/LP/A.mp3\n../Band/LP/B.mp3\n")?;
        let store = Box::new(crate::playlist::FsStore::new(&root));
        let app = App::new(tracks, SilentAudio::default(), store, "/m".to_owned(), None);
        Ok((tmp, app))
    }

    fn keys(app: &mut App<SilentAudio>, codes: &[KeyCode]) {
        for &code in codes {
            press(app, code);
        }
    }

    fn row_with<'a>(screen: &'a [String], needle: &str) -> Option<&'a String> {
        screen.iter().find(|l| l.contains(needle))
    }

    #[test]
    fn empty_playlists_view_says_how_to_make_one() {
        let mut app = library(3);
        press(&mut app, KeyCode::Tab);
        // Narrow: the sidebar alone, full width.
        let screen = draw(&app, &mut View::default(), 60, 20);
        assert!(screen[0].contains(" kasset "), "{screen:#?}");
        assert!(screen[0].contains(" 0 playlists "), "{screen:#?}");
        assert!(screen[1].contains("Library"), "{screen:#?}");
        assert!(screen_contains(&screen, "No playlists yet"), "{screen:#?}");
        assert!(screen_contains(&screen, "\"New"), "{screen:#?}");
        assert!(screen_contains(&screen, "playlist…\""), "{screen:#?}");
        assert!(screen[19].starts_with("Enter open"), "{screen:#?}");
        assert!(screen[19].contains("Tab focus"), "{screen:#?}");
    }

    #[test]
    fn playlists_view_lists_names_and_counts() -> anyhow::Result<()> {
        let (_tmp, mut app) = with_playlists()?;
        press(&mut app, KeyCode::Tab);
        let screen = draw(&app, &mut View::default(), 80, 20);
        assert!(screen[0].contains(" 2 playlists "), "{screen:#?}");
        let gym = screen.iter().position(|l| l.contains("Gym"));
        let running = screen.iter().position(|l| l.contains("Running"));
        assert!(gym < running, "sorted ignoring case: {screen:#?}");
        assert!(
            row_with(&screen, "Running").is_some_and(|l| l.contains("  2 ")),
            "{screen:#?}"
        );
        Ok(())
    }

    #[test]
    fn sidebar_count_counts_playlists_not_the_library_row() -> anyhow::Result<()> {
        let (_tmp, mut app) = with_playlists()?;
        press(&mut app, KeyCode::Tab);
        for c in "/gy".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let screen = draw(&app, &mut View::default(), 60, 20);
        assert!(screen[0].contains(" 1 of 2 match 'gy' "), "{screen:#?}");
        assert!(screen_contains(&screen, "Library"), "{screen:#?}");
        for c in "zz".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let screen = draw(&app, &mut View::default(), 60, 20);
        assert!(screen[0].contains(" 0 of 2 match 'gyzz' "), "{screen:#?}");
        assert!(screen_contains(&screen, "Library"), "{screen:#?}");
        assert!(
            screen_contains(&screen, "No playlists match 'gyzz'"),
            "{screen:#?}"
        );
        // A 22-column sidebar has no room for the query: the count gets shorter.
        let screen = draw(&app, &mut View::default(), 80, 20);
        assert!(screen[0].starts_with("┌"), "{screen:#?}");
        assert!(screen[0].contains(" 0 of 2 ┐"), "{screen:#?}");
        Ok(())
    }

    #[test]
    fn picker_lists_new_playlist_then_the_playlists() -> anyhow::Result<()> {
        let (tmp, mut app) = with_playlists()?;
        std::fs::write(
            tmp.path().join("Playlists/Old.m3u8"),
            b"../Band/LP/A.mp3\n# caf\xe9\n",
        )?;
        app.handle_key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT));
        press(&mut app, KeyCode::Down);
        // Narrow, so the sidebar (which lists "Gym" too) is not drawn.
        let screen = draw(&app, &mut View::default(), 60, 24);
        assert!(screen_contains(&screen, " Add 3 tracks to "), "{screen:#?}");
        let new = screen.iter().position(|l| l.contains("New playlist…"));
        let gym = screen.iter().position(|l| l.contains("Gym"));
        assert!(new.is_some() && new < gym, "{screen:#?}");
        assert!(screen_contains(&screen, "Old (read-only)"), "{screen:#?}");
        assert!(
            screen.last().is_some_and(|l| l.starts_with("↑/↓ choose")),
            "{screen:#?}"
        );

        // The selected row (Gym) is highlighted.
        let buffer = draw_buffer(&app, &mut View::default(), 60, 24);
        let y = u16::try_from(gym.unwrap_or(0))?;
        let x = (0..buffer.area.width)
            .find(|&x| buffer[(x, y)].symbol() == "G")
            .unwrap_or(0);
        assert!(buffer[(x, y)].modifier.contains(Modifier::REVERSED));
        Ok(())
    }

    #[test]
    fn name_prompt_shows_the_reason_a_name_was_refused() -> anyhow::Result<()> {
        let (_tmp, mut app) = with_playlists()?;
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        press(&mut app, KeyCode::Enter);
        for c in "a/b".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let screen = draw(&app, &mut View::default(), 80, 24);
        assert!(screen_contains(&screen, " New playlist "), "{screen:#?}");
        assert!(screen_contains(&screen, "a/b"), "{screen:#?}");
        press(&mut app, KeyCode::Enter);
        let screen = draw(&app, &mut View::default(), 80, 24);
        assert!(
            screen_contains(&screen, "the name cannot contain '/'"),
            "{screen:#?}"
        );
        assert!(
            screen.last().is_some_and(|l| l.starts_with("type a name")),
            "{screen:#?}"
        );
        Ok(())
    }

    #[test]
    fn open_playlist_marks_the_playing_entry_only_when_it_is_the_source() -> anyhow::Result<()> {
        let (_tmp, mut app) = with_playlists()?;
        // Play B from "Running" (its first entry), then open "Running"
        // (sidebar rows: "Library", "Gym", "Running").
        keys(&mut app, &[KeyCode::Tab, KeyCode::End, KeyCode::Enter]);
        keys(&mut app, &[KeyCode::Enter]);
        assert!(app.playback().is_playing_playlist("Running"));
        // Narrow, so only the main pane is drawn.
        let screen = draw(&app, &mut View::default(), 60, 20);
        assert!(screen[0].contains(" Running "), "{screen:#?}");
        assert!(screen_contains(&screen, "▶ B"), "{screen:#?}");
        assert!(!screen_contains(&screen, "▶ C"), "{screen:#?}");
        assert!(
            screen[19].starts_with("Enter play · d remove · J/K move · Esc sidebar"),
            "{screen:#?}"
        );

        // "Gym" also lists B, but it is not the play order. `Esc` focuses
        // the sidebar with "Running" selected.
        keys(&mut app, &[KeyCode::Esc, KeyCode::Up, KeyCode::Enter]);
        let screen = draw(&app, &mut View::default(), 60, 20);
        assert!(screen[0].contains(" Gym "), "{screen:#?}");
        // The list is the top 12 rows; the now-playing panel shows ▶ too.
        assert!(!screen_contains(&screen[..12], "▶"), "{screen:#?}");

        // The library marks the playing track whatever the source.
        keys(&mut app, &[KeyCode::Tab, KeyCode::Home, KeyCode::Enter]);
        let screen = draw(&app, &mut View::default(), 60, 20);
        assert!(screen_contains(&screen, "▶ B"), "{screen:#?}");
        Ok(())
    }

    #[test]
    fn playlist_played_from_the_library_has_no_marker() -> anyhow::Result<()> {
        let (_tmp, mut app) = with_playlists()?;
        keys(&mut app, &[KeyCode::Down, KeyCode::Enter]);
        assert_eq!(app.playback().current(), Some(1), "B from the library");
        keys(&mut app, &[KeyCode::Tab, KeyCode::End, KeyCode::Enter]);
        let screen = draw(&app, &mut View::default(), 60, 20);
        assert!(screen[0].contains("Running"), "{screen:#?}");
        assert!(screen_contains(&screen, "  B"), "{screen:#?}");
        // The list is the top 12 rows; the now-playing panel shows ▶ too.
        assert!(!screen_contains(&screen[..12], "▶"), "{screen:#?}");
        Ok(())
    }

    #[test]
    fn library_help_mentions_playlist_keys() {
        // At a common 80-column width: the bar is cut there, so these keys
        // must come before the transport keys.
        let app = library(3);
        let screen = draw(&app, &mut View::default(), 80, 20);
        assert!(
            screen[19].contains("a/A add selected/all to playlist"),
            "{screen:#?}"
        );
        assert!(screen[19].contains("Tab focus"), "{screen:#?}");
    }

    #[test]
    fn open_playlist_help_bar_shows_the_edit_keys() -> anyhow::Result<()> {
        let (_tmp, mut app) = with_playlists()?;
        keys(&mut app, &[KeyCode::Tab, KeyCode::Down, KeyCode::Enter]);
        // Narrow enough that the bar is cut: the edit keys come first.
        let screen = draw(&app, &mut View::default(), 50, 20);
        let bar = &screen[19];
        assert!(bar.contains("d remove"), "{screen:#?}");
        assert!(bar.contains("J/K move"), "{screen:#?}");
        assert!(bar.contains("Esc sidebar"), "{screen:#?}");

        press(&mut app, KeyCode::Esc);
        let screen = draw(&app, &mut View::default(), 200, 20);
        assert!(screen[19].contains("R rename"), "{screen:#?}");
        assert!(screen[19].contains("D delete"), "{screen:#?}");
        Ok(())
    }

    #[test]
    fn rename_prompt_is_prefilled_and_titled() -> anyhow::Result<()> {
        let (_tmp, mut app) = with_playlists()?;
        keys(&mut app, &[KeyCode::Tab, KeyCode::End]);
        app.handle_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT));
        let screen = draw(&app, &mut View::default(), 80, 24);
        assert!(screen_contains(&screen, " Rename playlist "), "{screen:#?}");
        assert!(
            screen_contains(&screen, "│ Running"),
            "prefilled: {screen:#?}"
        );
        assert!(
            screen_contains(&screen, "Enter to rename, Esc to cancel"),
            "{screen:#?}"
        );
        assert!(
            screen
                .last()
                .is_some_and(|l| l.starts_with("type a name · Enter rename")),
            "{screen:#?}"
        );
        Ok(())
    }

    #[test]
    fn delete_confirmation_names_the_playlist() -> anyhow::Result<()> {
        let (_tmp, mut app) = with_playlists()?;
        keys(&mut app, &[KeyCode::Tab, KeyCode::Down]);
        app.handle_key(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::SHIFT));
        let screen = draw(&app, &mut View::default(), 80, 24);
        assert!(screen_contains(&screen, " Delete playlist "), "{screen:#?}");
        assert!(screen_contains(&screen, "Delete \"Gym\"?"), "{screen:#?}");
        assert!(screen_contains(&screen, "y deletes"), "{screen:#?}");
        assert!(
            screen.last().is_some_and(|l| l.starts_with("y delete")),
            "{screen:#?}"
        );
        for (w, h) in [(0, 0), (1, 1), (2, 3), (10, 5), (80, 4), (5, 80), (300, 2)] {
            draw(&app, &mut View::default(), w, h);
        }
        Ok(())
    }

    #[test]
    fn popups_at_tiny_sizes_do_not_panic() -> anyhow::Result<()> {
        let (_tmp, mut app) = with_playlists()?;
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        for (w, h) in [(0, 0), (1, 1), (2, 3), (10, 5), (80, 4), (5, 80), (300, 2)] {
            draw(&app, &mut View::default(), w, h);
        }
        press(&mut app, KeyCode::Enter);
        for c in "日本語のプレイリスト".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        for (w, h) in [(0, 0), (1, 1), (2, 3), (10, 5), (80, 4), (5, 80), (300, 2)] {
            draw(&app, &mut View::default(), w, h);
        }
        Ok(())
    }

    #[test]
    fn elide_end_keeps_the_start_within_the_width() {
        assert_eq!(elide_end("Running", 7), "Running");
        assert_eq!(elide_end("Running", 4), "Run…");
        assert_eq!(elide_end("音楽です", 4), "音…");
        assert_eq!(elide_end("音楽", 2), "…", "a wide char never overflows");
        assert_eq!(elide_end("abc", 0), "");
    }

    #[test]
    fn long_queries_show_their_end() {
        assert_eq!(tail_fitting("abcdef", 3), "def");
        assert_eq!(tail_fitting("abc", 10), "abc");
        assert_eq!(tail_fitting("日本語", 4), "本語");
        assert_eq!(tail_fitting("日本語", 1), "");
        assert_eq!(tail_fitting("", 0), "");
    }

    /// A store that lists `infos` as given, including names the file
    /// system listing would refuse; everything else goes to a folder that
    /// does not exist.
    #[derive(Debug)]
    struct ListingStore {
        infos: Vec<PlaylistInfo>,
        rest: FsStore,
    }

    impl PlaylistStore for ListingStore {
        fn list(&self, _: &Resolver) -> Result<Vec<PlaylistInfo>, StoreError> {
            Ok(self.infos.clone())
        }
        fn load(&self, name: &str, resolver: &Resolver) -> Result<Playlist, StoreError> {
            self.rest.load(name, resolver)
        }
        fn changed(&self, playlist: &Playlist) -> Result<bool, StoreError> {
            self.rest.changed(playlist)
        }
        fn blank(&self, name: &str) -> Result<Playlist, StoreError> {
            self.rest.blank(name)
        }
        fn create(&self, playlist: &mut Playlist) -> Result<(), StoreError> {
            self.rest.create(playlist)
        }
        fn save(&self, playlist: &mut Playlist) -> Result<SaveOutcome, StoreError> {
            self.rest.save(playlist)
        }
        fn rename(&self, from: &str, to: &str) -> Result<(), StoreError> {
            self.rest.rename(from, to)
        }
        fn delete(&self, name: &str) -> Result<(), StoreError> {
            self.rest.delete(name)
        }
    }

    /// A 3-track library whose sidebar lists `infos` as given.
    fn listing(infos: Vec<PlaylistInfo>) -> App<SilentAudio> {
        let tracks = ["A", "B", "C"]
            .into_iter()
            .map(|t| track(t, "Band", "LP"))
            .collect();
        let store = Box::new(ListingStore {
            infos,
            rest: FsStore::new(std::path::Path::new("/nonexistent/kasset-test-library")),
        });
        App::new(tracks, SilentAudio::default(), store, "/m".to_owned(), None)
    }

    /// `n` tracks and `playlists` empty playlists "P00", "P01", … under a
    /// temporary root.
    fn many(n: usize, playlists: usize) -> anyhow::Result<(tempfile::TempDir, App<SilentAudio>)> {
        let tmp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(tmp.path())?;
        let dir = root.join("Playlists");
        std::fs::create_dir(&dir)?;
        for i in 0..playlists {
            std::fs::write(dir.join(format!("P{i:02}.m3u8")), "")?;
        }
        let tracks = (0..n)
            .map(|i| track(&format!("Song {i:02}"), "Band", "LP"))
            .collect();
        let store = Box::new(FsStore::new(&root));
        let app = App::new(tracks, SilentAudio::default(), store, "/m".to_owned(), None);
        Ok((tmp, app))
    }

    /// The columns `[from, to)` of each line.
    fn columns(screen: &[String], from: usize, to: usize) -> Vec<String> {
        screen
            .iter()
            .map(|l| l.chars().skip(from).take(to - from).collect())
            .collect()
    }

    /// Column of the main pane's left border at 100 columns.
    const MAIN_X: u16 = 25;

    /// Spec *Both panes visible*.
    #[test]
    fn both_panes_are_drawn_side_by_side() -> anyhow::Result<()> {
        let (_tmp, app) = with_playlists()?;
        let screen = draw(&app, &mut View::default(), 100, 30);
        assert_eq!(sidebar_width(100), MAIN_X, "a quarter of the width");
        let sidebar = columns(&screen, 0, usize::from(MAIN_X));
        let main = columns(&screen, usize::from(MAIN_X), 100);
        assert!(sidebar[0].contains(" kasset "), "{screen:#?}");
        assert!(sidebar[0].contains(" 2 playlists "), "{screen:#?}");
        assert!(sidebar[1].contains("Library") && sidebar[1].contains('3'));
        assert!(
            row_with(&sidebar, "Gym").is_some_and(|l| l.contains("2 │")),
            "{screen:#?}"
        );
        assert!(
            row_with(&sidebar, "Running").is_some_and(|l| l.contains("2 │")),
            "{screen:#?}"
        );
        assert!(main[0].contains(" Library · /m "), "{screen:#?}");
        assert!(main[0].contains(" 3 tracks "), "{screen:#?}");
        for title in ["A", "B", "C"] {
            assert!(
                screen_contains(&main, &format!("  {title}  ")),
                "{screen:#?}"
            );
        }
        Ok(())
    }

    /// Spec *Narrow terminal*.
    #[test]
    fn narrow_terminal_shows_only_the_focused_pane() -> anyhow::Result<()> {
        let (_tmp, mut app) = with_playlists()?;
        let screen = draw(&app, &mut View::default(), 60, 24);
        assert!(screen[0].starts_with("┌ Library · /m "), "{screen:#?}");
        assert!(!screen_contains(&screen, "kasset"), "{screen:#?}");
        assert!(!screen_contains(&screen, "Gym"), "{screen:#?}");
        press(&mut app, KeyCode::Tab);
        let screen = draw(&app, &mut View::default(), 60, 24);
        assert!(screen[0].starts_with("┌ kasset "), "{screen:#?}");
        assert!(screen_contains(&screen, "Gym"), "{screen:#?}");
        assert!(!screen_contains(&screen, "Title"), "{screen:#?}");
        // 69 columns is still narrow, 70 is not.
        let screen = draw(&app, &mut View::default(), 69, 24);
        assert!(!screen_contains(&screen, "Title"), "{screen:#?}");
        let screen = draw(&app, &mut View::default(), TWO_PANE_MIN_WIDTH, 24);
        assert!(screen_contains(&screen, "Title"), "{screen:#?}");
        Ok(())
    }

    /// Spec *Widening the terminal*.
    #[test]
    fn widening_the_terminal_shows_both_panes() -> anyhow::Result<()> {
        let (_tmp, mut app) = with_playlists()?;
        press(&mut app, KeyCode::Tab);
        let mut view = View::default();
        let narrow = draw(&app, &mut view, 60, 24);
        assert!(!screen_contains(&narrow, "Title"), "{narrow:#?}");
        let wide = draw(&app, &mut view, 100, 24);
        assert!(wide[0].contains(" kasset "), "{wide:#?}");
        assert!(wide[0].contains(" Library · /m "), "{wide:#?}");
        assert_eq!(app.focus(), Focus::Sidebar);
        Ok(())
    }

    #[test]
    fn only_the_focused_pane_has_the_focused_border() -> anyhow::Result<()> {
        let (_tmp, mut app) = with_playlists()?;
        let buffer = draw_buffer(&app, &mut View::default(), 100, 20);
        assert_eq!(buffer[(MAIN_X, 1)].fg, Color::Cyan, "main focused");
        assert_eq!(buffer[(0, 1)].fg, Color::Reset);
        assert!(buffer[(MAIN_X + 2, 0)].modifier.contains(Modifier::BOLD));
        assert!(!buffer[(2, 0)].modifier.contains(Modifier::BOLD));
        press(&mut app, KeyCode::Tab);
        let buffer = draw_buffer(&app, &mut View::default(), 100, 20);
        assert_eq!(buffer[(0, 1)].fg, Color::Cyan, "sidebar focused");
        assert_eq!(buffer[(MAIN_X, 1)].fg, Color::Reset);
        Ok(())
    }

    #[test]
    fn selection_is_reversed_only_in_the_focused_pane() -> anyhow::Result<()> {
        let (_tmp, mut app) = with_playlists()?;
        // Library selected in the sidebar (row 1), A in the main pane (row 2).
        let (sidebar_cell, main_cell) = ((4, 1), (MAIN_X + 4, 2));
        let buffer = draw_buffer(&app, &mut View::default(), 100, 20);
        assert_eq!(buffer[main_cell].symbol(), "A");
        assert!(buffer[main_cell].modifier.contains(Modifier::REVERSED));
        assert_eq!(buffer[sidebar_cell].symbol(), "L");
        let unfocused = buffer[sidebar_cell].modifier;
        assert!(!unfocused.contains(Modifier::REVERSED), "{unfocused:?}");
        assert!(unfocused.contains(Modifier::UNDERLINED | Modifier::BOLD));

        press(&mut app, KeyCode::Tab);
        let buffer = draw_buffer(&app, &mut View::default(), 100, 20);
        assert!(buffer[sidebar_cell].modifier.contains(Modifier::REVERSED));
        let unfocused = buffer[main_cell].modifier;
        assert!(!unfocused.contains(Modifier::REVERSED), "{unfocused:?}");
        assert!(unfocused.contains(Modifier::UNDERLINED));
        Ok(())
    }

    #[test]
    fn panes_keep_their_own_scroll_position() -> anyhow::Result<()> {
        let (_tmp, mut app) = many(100, 30)?;
        let mut view = View::default();
        draw(&app, &mut view, 100, 20);
        // Body 12 rows: the table has 12 - 2 borders - 1 header rows.
        assert_eq!(view.page_size(), 9, "the main pane's page");
        press(&mut app, KeyCode::End);
        draw(&app, &mut view, 100, 20);
        keys(&mut app, &[KeyCode::Tab, KeyCode::End]);
        let screen = draw(&app, &mut view, 100, 20);
        // The sidebar's rows below "Library" and its separator.
        assert_eq!(view.page_size(), 8, "the sidebar's page");
        assert!(screen_contains(&screen, "P29"), "{screen:#?}");
        assert!(!screen_contains(&screen, "P00"), "{screen:#?}");
        assert!(
            screen_contains(&screen, "Song 99"),
            "main kept: {screen:#?}"
        );
        assert!(screen[1].contains("Library"), "always on top: {screen:#?}");

        // Back in the main pane, the sidebar stays scrolled.
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Up);
        let screen = draw(&app, &mut view, 100, 20);
        assert!(screen_contains(&screen, "P29"), "{screen:#?}");
        assert!(screen_contains(&screen, "Song 99"), "{screen:#?}");
        // Selecting "Library" does not scroll the playlists.
        keys(&mut app, &[KeyCode::Tab, KeyCode::Home]);
        let screen = draw(&app, &mut view, 100, 20);
        assert!(screen_contains(&screen, "P29"), "{screen:#?}");
        Ok(())
    }

    #[test]
    fn picker_popup_is_drawn_over_the_body() -> anyhow::Result<()> {
        let (_tmp, mut app) = with_playlists()?;
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        let screen = draw(&app, &mut View::default(), 100, 30);
        // The body is the top 30 - 8 rows.
        let title = screen.iter().position(|l| l.contains(" Add 1 track to "));
        let bottom = screen
            .iter()
            .rposition(|l| l.contains("Running"))
            .map(|y| y + 1);
        assert!(title.is_some_and(|y| y > 0), "{screen:#?}");
        assert!(bottom.is_some_and(|y| y < 22), "{screen:#?}");
        Ok(())
    }

    /// The sidebar row of `name` at 100 columns, if listed.
    fn sidebar_row(screen: &[String], name: &str) -> Option<String> {
        row_with(&columns(screen, 0, usize::from(MAIN_X)), name).cloned()
    }

    /// Spec *Playlist playing* (sidebar).
    #[test]
    fn sidebar_marks_the_playing_playlist() -> anyhow::Result<()> {
        let (_tmp, mut app) = with_playlists()?;
        keys(&mut app, &[KeyCode::Tab, KeyCode::End, KeyCode::Enter]);
        keys(&mut app, &[KeyCode::Enter]);
        let screen = draw(&app, &mut View::default(), 100, 20);
        let running = sidebar_row(&screen, "Running").unwrap_or_default();
        assert!(running.starts_with("│ ▶ Running"), "{screen:#?}");
        let library = sidebar_row(&screen, "Library").unwrap_or_default();
        assert!(library.starts_with("│   Library"), "{screen:#?}");
        // Paused, the marker follows the state.
        press(&mut app, KeyCode::Char(' '));
        let screen = draw(&app, &mut View::default(), 100, 20);
        let running = sidebar_row(&screen, "Running").unwrap_or_default();
        assert!(running.starts_with("│ ‖ Running"), "{screen:#?}");
        Ok(())
    }

    /// Spec *Different source* (sidebar).
    #[test]
    fn sidebar_marks_the_library_when_it_is_the_play_order() -> anyhow::Result<()> {
        let (_tmp, mut app) = with_playlists()?;
        keys(&mut app, &[KeyCode::Down, KeyCode::Enter]);
        keys(&mut app, &[KeyCode::Tab, KeyCode::End, KeyCode::Enter]);
        let screen = draw(&app, &mut View::default(), 100, 20);
        let library = sidebar_row(&screen, "Library").unwrap_or_default();
        assert!(library.starts_with("│ ▶ Library"), "{screen:#?}");
        let running = sidebar_row(&screen, "Running").unwrap_or_default();
        assert!(running.starts_with("│   Running"), "{screen:#?}");
        Ok(())
    }

    /// Spec *Nothing played yet* (sidebar).
    #[test]
    fn sidebar_marks_nothing_before_a_track_is_played() -> anyhow::Result<()> {
        let (_tmp, app) = with_playlists()?;
        let screen = draw(&app, &mut View::default(), 100, 20);
        let sidebar = columns(&screen[..12], 0, usize::from(MAIN_X));
        for marker in ["▶", "‖", "■"] {
            assert!(!screen_contains(&sidebar, marker), "{screen:#?}");
        }
        Ok(())
    }

    /// Spec *No folder* (drawn).
    #[test]
    fn no_folder_shows_the_hint_below_library() {
        let app = library(3);
        let screen = draw(&app, &mut View::default(), 100, 20);
        let sidebar = columns(&screen, 0, usize::from(MAIN_X));
        assert!(sidebar[0].contains(" 0 playlists "), "{screen:#?}");
        assert!(sidebar[1].contains("Library"), "{screen:#?}");
        let hint = sidebar.iter().position(|l| l.contains("No playlists"));
        assert!(hint.is_some_and(|y| y > 1), "{screen:#?}");
    }

    #[test]
    fn long_and_hostile_playlist_names_stay_in_the_sidebar() {
        let info = |name: String, read_only| PlaylistInfo {
            name,
            tracks: Some(12),
            read_only,
        };
        let long = "x".repeat(300);
        let mut app = listing(vec![
            info("Bad\u{1b}[31mName\u{7}".to_owned(), None),
            info(long, Some(crate::playlist::ReadOnlyReason::NotUtf8)),
        ]);
        let buffer = draw_buffer(&app, &mut View::default(), 100, 20);
        let screen = lines(&buffer);
        for line in &screen {
            assert!(!line.chars().any(char::is_control), "{screen:#?}");
        }
        // The sidebar's right border is intact on every body row (20 - 8).
        for line in &screen[..12] {
            let edge = line.chars().nth(usize::from(MAIN_X) - 1);
            assert!(
                matches!(edge, Some('┐' | '│' | '┘')),
                "{edge:?}: {screen:#?}"
            );
        }
        assert!(
            sidebar_row(&screen, "Bad [31mName").is_some_and(|l| l.contains("12 │")),
            "sanitised: {screen:#?}"
        );
        let long_row = sidebar_row(&screen, "xxx").unwrap_or_default();
        assert!(long_row.contains("x… 12 │"), "elided: {screen:#?}");
        // The read-only name is dim; its count is not.
        let y =
            u16::try_from(screen.iter().position(|l| l.contains("xxx")).unwrap_or(0)).unwrap_or(0);
        assert_eq!(buffer[(4, y)].fg, Color::DarkGray);
        assert_eq!(buffer[(MAIN_X - 3, y)].fg, Color::Reset);

        // Full width in the narrow layout too.
        press(&mut app, KeyCode::Tab);
        let screen = draw(&app, &mut View::default(), 40, 20);
        assert!(
            row_with(&screen, "xxx").is_some_and(|l| l.ends_with("x… 12 │")),
            "{screen:#?}"
        );
    }

    #[test]
    fn main_pane_titles() {
        assert_eq!(
            library_title("/home/me/Music", 40),
            " Library · /home/me/Music "
        );
        assert_eq!(library_title("/home/me/Music", 18), " Library · …Music ");
        assert_eq!(library_title("/home/me/Music", 9), " Library ");
        assert_eq!(library_title("/home/me/Music", 8), "");
        assert_eq!(playlist_title("Running", false, 40), " Running ");
        assert_eq!(playlist_title("Running", true, 40), " Running · read-only ");
        assert_eq!(playlist_title("Running", true, 18), " Run… · read-only ");
        assert_eq!(
            playlist_title("Running", true, 10),
            " Running ",
            "suffix goes first"
        );
        assert_eq!(playlist_title("Running", true, 6), " Run… ");
        assert_eq!(playlist_title("Running", true, 3), "");
    }

    #[test]
    fn read_only_open_playlist_says_so_in_its_title() -> anyhow::Result<()> {
        let (tmp, mut app) = with_playlists()?;
        std::fs::write(
            tmp.path().join("Playlists/Old.m3u8"),
            b"../Band/LP/A.mp3\n# caf\xe9\n",
        )?;
        // Sidebar rows: "Library", "Gym", "Old", "Running".
        keys(
            &mut app,
            &[KeyCode::Tab, KeyCode::Down, KeyCode::Down, KeyCode::Enter],
        );
        let screen = draw(&app, &mut View::default(), 100, 20);
        assert!(screen[0].contains(" Old · read-only "), "{screen:#?}");
        Ok(())
    }

    #[test]
    fn count_title_shortens_to_fit() {
        let list = ListView {
            rows: &[],
            selected: None,
            query: "a long query",
        };
        assert_eq!(
            count_title(&list, 1, 9, "tracks", 80),
            " 1 of 9 match 'a long query' "
        );
        assert_eq!(count_title(&list, 1, 9, "tracks", 10), " 1 of 9 ");
        assert_eq!(count_title(&list, 1, 9, "tracks", 6), " 1/9 ");
        assert_eq!(count_title(&list, 1, 9, "tracks", 2), "");
        let list = ListView { query: "", ..list };
        assert_eq!(count_title(&list, 9, 9, "tracks", 10), " 9 tracks ");
        assert_eq!(count_title(&list, 9, 9, "tracks", 9), " 9 ");
        assert_eq!(count_title(&list, 9, 9, "tracks", 2), "");
    }

    #[test]
    fn two_pane_layout_at_tiny_heights_does_not_panic() -> anyhow::Result<()> {
        let (_tmp, mut app) = many(5, 5)?;
        for focus_moves in 0..2 {
            for h in 0..14 {
                draw(&app, &mut View::default(), 100, h);
                draw(&app, &mut View::default(), TWO_PANE_MIN_WIDTH, h);
            }
            if focus_moves == 0 {
                keys(&mut app, &[KeyCode::Tab, KeyCode::End]);
            }
        }
        Ok(())
    }

    /// A `View` reused across content and size changes: a sidebar scrolled
    /// far down must survive its list shrinking while no playlist row fits.
    #[test]
    fn stale_sidebar_offset_with_no_room_does_not_panic() -> anyhow::Result<()> {
        let (tmp, mut app) = many(5, 30)?;
        let mut view = View::default();
        keys(&mut app, &[KeyCode::Tab, KeyCode::End]);
        draw(&app, &mut view, 100, 20);
        assert!(view.sidebar_offset > 5, "scrolled: {view:?}");
        press(&mut app, KeyCode::Home); // "Library": the playlists keep their offset
        for i in 3..30 {
            std::fs::remove_file(tmp.path().join(format!("Playlists/P{i:02}.m3u8")))?;
        }
        keys(&mut app, &[KeyCode::Tab, KeyCode::Tab]); // listed again
        assert_eq!(app.playlists().len(), 3);
        for h in [11, 10, 9, 30, 11] {
            draw(&app, &mut view, 100, h);
            draw(&app, &mut view, 60, h);
        }
        // A filter shrinks the rows too.
        for c in "/p0".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        draw(&app, &mut view, 100, 11);
        let screen = draw(&app, &mut view, 100, 30);
        assert!(screen_contains(&screen, "P02"), "{screen:#?}");
        Ok(())
    }

    #[test]
    fn wide_characters_are_elided_within_their_pane() -> anyhow::Result<()> {
        assert!(width(&elide_end("日本語", 4)) <= 4);
        assert!(width(&elide_start("/音楽/日本語", 5)) <= 5);
        let tmp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(tmp.path())?;
        let dir = root.join("Playlists");
        std::fs::create_dir(&dir)?;
        std::fs::write(dir.join("日本語のプレイリストとても長い名前.m3u8"), "")?;
        let app = App::new(
            vec![track("曲", "バンド", "アルバム")],
            SilentAudio::default(),
            Box::new(FsStore::new(&root)),
            format!("/ホーム/{}", "音楽/".repeat(20)),
            None,
        );
        let mut view = View::default();
        for w in [100, 87, 71, 70] {
            // Cells, not joined lines: a wide character's second cell is
            // blank in the buffer.
            let buffer = draw_buffer(&app, &mut view, w, 20);
            let screen = lines(&buffer);
            let side = sidebar_width(w);
            // Both borders between the panes are intact on every body row.
            for y in 0..12 {
                let edges = (buffer[(side - 1, y)].symbol(), buffer[(side, y)].symbol());
                assert!(
                    matches!(edges, ("┐", "┌") | ("│", "│") | ("┘", "└")),
                    "{edges:?} at row {y}: {screen:#?}"
                );
            }
            assert!(screen_contains(&screen, "日 本 語"), "{screen:#?}");
            assert!(screen[0].contains(" Library · …"), "{screen:#?}");
        }
        Ok(())
    }
}
