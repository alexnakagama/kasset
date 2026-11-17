//! Drawing the screen from the [`App`] state. Rendering never changes the app;
//! the only state kept here is the list's scroll position ([`View`]).
//!
//! All text drawn comes from sanitised sources (tags and file names through
//! `text::sanitize`/`display_path`, playlist names through `text::sanitize`
//! here, typed text through input filtering), so no terminal control
//! sequence can reach the screen.

use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Cell, Clear, Gauge, List, ListItem, ListState, Padding, Paragraph, Row, Table,
    TableState, Wrap,
};

use crate::app::{App, Mode, NameInput, NamePurpose, Picker, View as Screen};
use crate::playback::{Level, PlayState, clock};
use crate::player::Audio;
use crate::text::sanitize;

/// Height of the now-playing panel, borders included.
const NOW_PLAYING_HEIGHT: u16 = 6;

// Each bar puts its view's own keys first: the bar is cut at the terminal's width.
const KEYS_LIBRARY: &str = "Enter play · a/A add selected/all to playlist · Tab playlists · / search · \
                            Space pause · s stop · n/p next/prev · ←/→ seek · \
                            +/- volume · z shuffle · r repeat · q quit";
const KEYS_PLAYLISTS: &str = "Enter open · R rename · D delete · / search · Tab library · \
                              Space pause · s stop · n/p next/prev · ←/→ seek · +/- volume · \
                              z shuffle · r repeat · q quit";
// The edit keys come first: the bar is cut at the terminal's width.
const KEYS_PLAYLIST: &str = "Enter play · d remove · J/K move · Esc back · / search · \
                             Tab library · Space pause · s stop · n/p next/prev · ←/→ seek · \
                             +/- volume · z shuffle · r repeat · q quit";
const KEYS_SEARCH: &str = "type to filter · Enter keep · Esc clear · ↑/↓ move · Ctrl+C quit";
const KEYS_PICKER: &str = "↑/↓ choose · Enter add · Esc cancel";
const KEYS_NAME: &str = "type a name · Enter create · Esc cancel";
const KEYS_RENAME: &str = "type a name · Enter rename · Esc cancel";
const KEYS_CONFIRM: &str = "y delete · any other key cancel";

/// Start of the library title, before the library root.
const TITLE_PREFIX: &str = " kasset · ";

/// Start of an open playlist's title, before its name.
const PLAYLIST_PREFIX: &str = " Playlist · ";

/// Label of the picker's first row.
pub const NEW_PLAYLIST: &str = "New playlist…";

/// Widest popup, in columns.
const POPUP_MAX_WIDTH: u16 = 60;

/// `text` cut from the start to fit `max` columns, with `…` marking the cut.
fn elide_start(text: &str, max: usize) -> String {
    if Span::raw(text).width() <= max {
        return text.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    let mut width = 1; // the ellipsis
    let mut start = text.len();
    for (i, c) in text.char_indices().rev() {
        let w = Span::raw(&text[i..i + c.len_utf8()]).width();
        if width + w > max {
            break;
        }
        width += w;
        start = i;
    }
    format!("…{}", &text[start..])
}

/// `text` cut at the end to fit `max` columns, with `…` marking the cut.
fn elide_end(text: &str, max: usize) -> String {
    if Span::raw(text).width() <= max {
        return text.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    let mut width = 1; // the ellipsis
    let mut end = 0;
    for (i, c) in text.char_indices() {
        let w = Span::raw(&text[i..i + c.len_utf8()]).width();
        if width + w > max {
            break;
        }
        width += w;
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
    /// First list row on screen.
    offset: usize,
    /// List rows that fit on screen in the last frame.
    page_size: usize,
}

impl View {
    /// List rows shown in the last frame (at least 1).
    #[must_use]
    pub const fn page_size(&self) -> usize {
        if self.page_size == 0 {
            1
        } else {
            self.page_size
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
        Mode::Normal => match app.view() {
            Screen::Library => KEYS_LIBRARY,
            Screen::Playlists => KEYS_PLAYLISTS,
            Screen::Playlist => KEYS_PLAYLIST,
        },
    }
}

/// Draws the whole screen.
pub fn render<A: Audio>(frame: &mut Frame, app: &App<A>, view: &mut View, now: Instant) {
    let [list, playing, status, keys] = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(NOW_PLAYING_HEIGHT),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    match app.view() {
        Screen::Library => render_library(frame, app, view, list),
        Screen::Playlists => render_playlists(frame, app, view, list),
        Screen::Playlist => render_open_playlist(frame, app, view, list),
    }
    render_now_playing(frame, app, playing);
    render_status(frame, app, status, now);
    frame.render_widget(Paragraph::new(key_help(app)).dark_gray(), keys);

    match app.mode() {
        Mode::Picker(picker) => render_picker(frame, picker, list),
        Mode::NameInput(input) => render_name_input(frame, input, list),
        Mode::ConfirmDelete(name) => render_confirm_delete(frame, name, list),
        Mode::Normal | Mode::Search => {}
    }
}

/// `" N <noun> "`, or how many rows match the filter.
fn count_title<A: Audio>(app: &App<A>, total: usize, noun: &str) -> String {
    if app.query().is_empty() {
        format!(" {total} {noun} ")
    } else {
        format!(
            " {} of {total} match '{}' ",
            app.visible().len(),
            app.query()
        )
    }
}

/// A bordered list block titled `prefix` + `text` on the left (elided by
/// `elide` to leave room for `count` on the right, or just `fallback` when
/// there is no room for `text`), and `count` on the right.
fn list_block<'a>(
    area: Rect,
    (prefix, text, fallback): (&str, &str, &str),
    elide: fn(&str, usize) -> String,
    count: String,
) -> Block<'a> {
    // The left title gets whatever the count leaves free: two borders, the
    // count and a one-column gap.
    let free = usize::from(area.width).saturating_sub(2 + Span::raw(&count).width() + 1);
    let room = free.saturating_sub(Span::raw(prefix).width() + 1);
    let title = if text.is_empty() {
        prefix.to_owned()
    } else if room < 2 {
        fallback.to_owned()
    } else {
        format!("{prefix}{} ", elide(text, room))
    };
    Block::bordered()
        .padding(Padding::horizontal(1))
        .title(title.bold())
        .title_top(Line::from(count).right_aligned())
}

/// Draws `message` in place of an empty list.
fn render_empty(frame: &mut Frame, view: &mut View, inner: Rect, message: String) {
    view.offset = 0;
    view.page_size = usize::from(inner.height);
    frame.render_widget(
        Paragraph::new(message).italic().wrap(Wrap { trim: false }),
        inner,
    );
}

fn render_library<A: Audio>(frame: &mut Frame, app: &App<A>, view: &mut View, area: Rect) {
    let total = app.playback().tracks().len();
    let count = count_title(app, total, "tracks");
    // Long roots keep their end, the part that tells folders apart.
    let block = list_block(
        area,
        (TITLE_PREFIX, app.root(), " kasset "),
        elide_start,
        count,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.visible().is_empty() {
        let message = if total == 0 {
            format!("No music found in {}", app.root())
        } else {
            format!("No tracks match '{}'", app.query())
        };
        render_empty(frame, view, inner, message);
        return;
    }
    let current = app.playback().current();
    render_tracks(frame, app, view, inner, |index| {
        (index, current == Some(index))
    });
}

fn render_open_playlist<A: Audio>(frame: &mut Frame, app: &App<A>, view: &mut View, area: Rect) {
    let Some(open) = app.open_playlist() else {
        return;
    };
    let items = open.items();
    let mut name = shown_name(open.name());
    if open.playlist().editable().is_err() {
        name.push_str(" · read-only");
    }
    let count = count_title(app, items.len(), "tracks");
    let block = list_block(
        area,
        (PLAYLIST_PREFIX, &name, " Playlist "),
        elide_end,
        count,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.visible().is_empty() {
        let message = if !items.is_empty() {
            format!("No tracks match '{}'", app.query())
        } else if open.playlist().unmatched() > 0 {
            "None of this playlist's entries are in the library".to_owned()
        } else {
            "This playlist is empty. Add tracks from the library with a or A.".to_owned()
        };
        render_empty(frame, view, inner, message);
        return;
    }
    // Only the playlist that is the play order shows what is playing: the
    // same song may be listed here while it plays from elsewhere.
    let playback = app.playback();
    let playing_item = playback
        .is_playing_playlist(open.name())
        .then(|| playback.current_item())
        .flatten();
    render_tracks(frame, app, view, inner, |item| {
        (items[item], playing_item == Some(item))
    });
}

/// Draws the rows of a track list; `row(value)` gives the library index of
/// the track a row shows and whether it is the one playing.
fn render_tracks<A: Audio>(
    frame: &mut Frame,
    app: &App<A>,
    view: &mut View,
    inner: Rect,
    row: impl Fn(usize) -> (usize, bool),
) {
    let shown = app.visible().len();
    // One line is the header.
    let rows = usize::from(inner.height.saturating_sub(1));
    let selected = app.selected().unwrap_or(0);
    view.offset = scroll(view.offset, selected, rows, shown);
    view.page_size = rows;

    let tracks = app.playback().tracks();
    let state = app.playback().state();
    let end = view.offset.saturating_add(rows).min(shown);
    let body = app.visible()[view.offset..end].iter().filter_map(|&value| {
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
            row.style(Style::new().fg(Color::Green).add_modifier(Modifier::BOLD))
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
    .style(Style::new().add_modifier(Modifier::BOLD | Modifier::UNDERLINED));
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
    .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut state = TableState::new().with_selected(Some(selected - view.offset));
    frame.render_stateful_widget(table, inner, &mut state);
}

fn render_playlists<A: Audio>(frame: &mut Frame, app: &App<A>, view: &mut View, area: Rect) {
    let infos = app.playlists();
    let count = count_title(app, infos.len(), "playlists");
    let block = list_block(area, (" Playlists ", "", ""), elide_end, count);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let shown = app.visible().len();
    if shown == 0 {
        let message = if infos.is_empty() {
            "No playlists yet. In the library, press a (selected track) or A (all listed \
             tracks) and choose \"New playlist…\"."
                .to_owned()
        } else {
            format!("No playlists match '{}'", app.query())
        };
        render_empty(frame, view, inner, message);
        return;
    }

    let rows = usize::from(inner.height.saturating_sub(1));
    let selected = app.selected().unwrap_or(0);
    view.offset = scroll(view.offset, selected, rows, shown);
    view.page_size = rows;

    let playback = app.playback();
    let state = playback.state();
    let end = view.offset.saturating_add(rows).min(shown);
    let body = app.visible()[view.offset..end].iter().filter_map(|&i| {
        let info = infos.get(i)?;
        let playing = playback.is_playing_playlist(&info.name);
        let tracks = info
            .tracks
            .map_or_else(|| "?".to_owned(), |n| n.to_string());
        let row = Row::new([
            Cell::from(if playing { state_marker(state) } else { " " }),
            Cell::from(shown_name(&info.name)),
            Cell::from(Line::from(tracks).right_aligned()),
            Cell::from(if info.read_only.is_some() {
                "read-only"
            } else {
                ""
            }),
        ]);
        Some(if playing {
            row.style(Style::new().fg(Color::Green).add_modifier(Modifier::BOLD))
        } else {
            row
        })
    });
    let header = Row::new([
        Cell::from(""),
        Cell::from("Name"),
        Cell::from(Line::from("Tracks").right_aligned()),
        Cell::from(""),
    ])
    .style(Style::new().add_modifier(Modifier::BOLD | Modifier::UNDERLINED));
    let table = Table::new(
        body,
        [
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(7),
            Constraint::Length(9),
        ],
    )
    .header(header)
    .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut table_state = TableState::new().with_selected(Some(selected - view.offset));
    frame.render_stateful_widget(table, inner, &mut table_state);
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
        .map(|(name, read_only)| Span::raw(name).width() + if *read_only { 12 } else { 0 })
        .chain([Span::raw(NEW_PLAYLIST).width(), Span::raw(&title).width()])
        .max()
        .unwrap_or(0);
    // Borders, padding and the highlight symbol.
    let width = u16::try_from(widest + 6)
        .unwrap_or(u16::MAX)
        .clamp(30, POPUP_MAX_WIDTH);
    let height = u16::try_from(names.len() + 3).unwrap_or(u16::MAX);
    let popup = centered(area, width, height);

    let items = std::iter::once(ListItem::new(Line::from(NEW_PLAYLIST.italic()))).chain(
        names.into_iter().map(|(name, read_only)| {
            if read_only {
                ListItem::new(Line::from(vec![
                    Span::raw(name).dark_gray(),
                    Span::raw(" (read-only)").dark_gray(),
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
                .title(title.bold()),
        )
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
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
        .title(title.bold());
    let inner = block.inner(popup);
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);
    let [text_line, message_line] = Layout::vertical([Constraint::Length(1); 2]).areas(inner);

    // Show the end of a name too long for the line, so the cursor stays visible.
    let room = usize::from(text_line.width.saturating_sub(1));
    let text = tail_fitting(input.text(), room);
    frame.render_widget(Line::from(text), text_line);
    if text_line.width > 0 && text_line.height > 0 {
        let width = u16::try_from(Span::raw(text).width()).unwrap_or(u16::MAX);
        let x = text_line.x.saturating_add(width);
        frame.set_cursor_position(Position::new(x.min(text_line.right() - 1), text_line.y));
    }
    let message = input.error().map_or_else(
        || Line::from(hint).dark_gray(),
        |error| Line::styled(error, Style::new().fg(Color::Red)),
    );
    frame.render_widget(message, message_line);
}

/// The "delete playlist?" question.
fn render_confirm_delete(frame: &mut Frame, name: &str, area: Rect) {
    let popup = centered(area, POPUP_MAX_WIDTH, 4);
    let block = Block::bordered()
        .padding(Padding::horizontal(1))
        .title(" Delete playlist ".bold());
    let inner = block.inner(popup);
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);
    let [question_line, hint_line] = Layout::vertical([Constraint::Length(1); 2]).areas(inner);
    // `Delete "` and `"?` around the name, which is cut to fit.
    let room = usize::from(question_line.width).saturating_sub(10);
    let question = format!("Delete \"{}\"?", elide_end(&shown_name(name), room));
    frame.render_widget(Line::from(question), question_line);
    frame.render_widget(
        Line::from("y deletes the file (songs stay), other keys cancel").dark_gray(),
        hint_line,
    );
}

/// The first row to show so that `selected` is on screen, moving the previous
/// `offset` as little as possible and never leaving blank rows at the bottom.
fn scroll(offset: usize, selected: usize, rows: usize, len: usize) -> usize {
    if rows == 0 {
        return selected;
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
        PlayState::Playing => Style::new().fg(Color::Green),
        PlayState::Paused => Style::new().fg(Color::Yellow),
        PlayState::Stopped => Style::new().fg(Color::DarkGray),
    }
    .add_modifier(Modifier::BOLD);
    let state_label = Span::styled(
        format!("{} {}", state_marker(state), state.label()),
        state_style,
    );

    if let Some(track) = playback.current_track() {
        frame.render_widget(
            Line::from(vec![
                state_label,
                Span::raw("  "),
                track.title.as_str().bold(),
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
                "select a track and press Enter".italic(),
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
            .gauge_style(Style::new().fg(Color::Cyan)),
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
            Level::Error => Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
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
    let width = u16::try_from(Span::raw(query).width()).unwrap_or(u16::MAX);
    frame.render_widget(Line::from(vec!["/".bold(), Span::raw(query)]), area);
    if area.width > 0 && area.height > 0 {
        let x = area.x.saturating_add(1).saturating_add(width);
        frame.set_cursor_position(Position::new(x.min(area.right() - 1), area.y));
    }
    // Messages (e.g. a track skipped while typing) go after the query, at the
    // right edge when there is room, so they are not missed during a search.
    if let Some(line) = notice {
        let used = width.saturating_add(4); // "/", cursor, gap
        let free = area.width.saturating_sub(used);
        if free > 0 {
            let len = u16::try_from(line.width()).unwrap_or(u16::MAX).min(free);
            let at = Rect::new(area.right() - len, area.y, len, area.height);
            frame.render_widget(line, at);
        }
    }
}

/// The longest suffix of `text` at most `width` columns wide.
fn tail_fitting(text: &str, width: usize) -> &str {
    text.char_indices()
        .map(|(i, _)| &text[i..])
        .find(|tail| Span::raw(*tail).width() <= width)
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{SilentAudio, no_store, track};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Renders `app` at `width`×`height` and returns the screen as lines.
    fn draw(app: &App<SilentAudio>, view: &mut View, width: u16, height: u16) -> Vec<String> {
        let mut terminal =
            Terminal::new(TestBackend::new(width, height)).unwrap_or_else(|never| match never {});
        let now = Instant::now();
        terminal
            .draw(|f| render(f, app, view, now))
            .unwrap_or_else(|never| match never {});
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
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
        // " 1 tracks " leaves exactly 2 columns for the root at width 26:
        // the ellipsis and the last character.
        let app = App::new(
            vec![track("Song", "Band", "LP")],
            SilentAudio::default(),
            no_store(),
            "/music".to_owned(),
            None,
        );
        let screen = draw(&app, &mut View::default(), 26, 12);
        assert!(screen[0].contains(" kasset · …c "), "{screen:#?}");
        let screen = draw(&app, &mut View::default(), 25, 12);
        assert!(screen[0].contains(" kasset "), "{screen:#?}");
        assert!(!screen[0].contains('…'), "{screen:#?}");
        // Too narrow for the root at all: only the name is left.
        let screen = draw(&app, &mut View::default(), 22, 12);
        assert!(screen[0].contains(" kasset "), "{screen:#?}");
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
        let screen = draw(&app, &mut View::default(), 100, 20);
        assert!(screen[0].contains(" Playlists "), "{screen:#?}");
        assert!(screen[0].contains(" 0 playlists "), "{screen:#?}");
        assert!(screen_contains(&screen, "No playlists yet"), "{screen:#?}");
        assert!(screen_contains(&screen, "New playlist…"), "{screen:#?}");
        assert!(screen[19].starts_with("Enter open"), "{screen:#?}");
        assert!(screen[19].contains("Tab library"), "{screen:#?}");
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
    fn picker_lists_new_playlist_then_the_playlists() -> anyhow::Result<()> {
        let (tmp, mut app) = with_playlists()?;
        std::fs::write(
            tmp.path().join("Playlists/Old.m3u8"),
            b"../Band/LP/A.mp3\n# caf\xe9\n",
        )?;
        app.handle_key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT));
        press(&mut app, KeyCode::Down);
        let screen = draw(&app, &mut View::default(), 80, 24);
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
        let mut terminal =
            Terminal::new(TestBackend::new(80, 24)).unwrap_or_else(|never| match never {});
        let now = Instant::now();
        terminal
            .draw(|f| render(f, &app, &mut View::default(), now))
            .unwrap_or_else(|never| match never {});
        let buffer = terminal.backend().buffer();
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
        // Play B from "Running" (its first entry), then open "Running".
        keys(&mut app, &[KeyCode::Tab, KeyCode::Down, KeyCode::Enter]);
        keys(&mut app, &[KeyCode::Enter]);
        assert!(app.playback().is_playing_playlist("Running"));
        let screen = draw(&app, &mut View::default(), 80, 20);
        assert!(screen[0].contains(" Playlist · Running "), "{screen:#?}");
        assert!(screen_contains(&screen, "▶ B"), "{screen:#?}");
        assert!(!screen_contains(&screen, "▶ C"), "{screen:#?}");
        assert!(
            screen[19].starts_with("Enter play · d remove · J/K move · Esc back"),
            "{screen:#?}"
        );

        // "Gym" also lists B, but it is not the play order.
        keys(&mut app, &[KeyCode::Esc, KeyCode::Up, KeyCode::Enter]);
        let screen = draw(&app, &mut View::default(), 80, 20);
        assert!(screen[0].contains(" Playlist · Gym "), "{screen:#?}");
        // The list is the top 12 rows; the now-playing panel shows ▶ too.
        assert!(!screen_contains(&screen[..12], "▶"), "{screen:#?}");

        // The library marks the playing track whatever the source.
        press(&mut app, KeyCode::Tab);
        let screen = draw(&app, &mut View::default(), 80, 20);
        assert!(screen_contains(&screen, "▶ B"), "{screen:#?}");
        Ok(())
    }

    #[test]
    fn playlist_played_from_the_library_has_no_marker() -> anyhow::Result<()> {
        let (_tmp, mut app) = with_playlists()?;
        keys(&mut app, &[KeyCode::Down, KeyCode::Enter]);
        assert_eq!(app.playback().current(), Some(1), "B from the library");
        keys(&mut app, &[KeyCode::Tab, KeyCode::Down, KeyCode::Enter]);
        let screen = draw(&app, &mut View::default(), 80, 20);
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
        assert!(screen[19].contains("Tab playlists"), "{screen:#?}");
    }

    #[test]
    fn open_playlist_help_bar_shows_the_edit_keys() -> anyhow::Result<()> {
        let (_tmp, mut app) = with_playlists()?;
        keys(&mut app, &[KeyCode::Tab, KeyCode::Enter]);
        // Narrow enough that the bar is cut: the edit keys come first.
        let screen = draw(&app, &mut View::default(), 50, 20);
        let bar = &screen[19];
        assert!(bar.contains("d remove"), "{screen:#?}");
        assert!(bar.contains("J/K move"), "{screen:#?}");
        assert!(bar.contains("Esc back"), "{screen:#?}");

        press(&mut app, KeyCode::Esc);
        let screen = draw(&app, &mut View::default(), 200, 20);
        assert!(screen[19].contains("R rename"), "{screen:#?}");
        assert!(screen[19].contains("D delete"), "{screen:#?}");
        Ok(())
    }

    #[test]
    fn rename_prompt_is_prefilled_and_titled() -> anyhow::Result<()> {
        let (_tmp, mut app) = with_playlists()?;
        keys(&mut app, &[KeyCode::Tab, KeyCode::Down]);
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
        keys(&mut app, &[KeyCode::Tab]);
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
}
