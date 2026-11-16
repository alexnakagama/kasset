//! Drawing the screen from the [`App`] state. Rendering never changes the app;
//! the only state kept here is the list's scroll position ([`View`]).
//!
//! All text drawn comes from sanitised sources (tags and file names through
//! `text::sanitize`/`display_path`, the query through input filtering), so no
//! terminal control sequence can reach the screen.

use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Gauge, Padding, Paragraph, Row, Table, TableState, Wrap};

use crate::app::{App, Mode};
use crate::playback::{Level, PlayState, clock};
use crate::player::Audio;

/// Height of the now-playing panel, borders included.
const NOW_PLAYING_HEIGHT: u16 = 6;

const KEYS_NORMAL: &str = "Enter play · Space pause · s stop · n/p next/prev · ←/→ seek · \
                           +/- volume · z shuffle · r repeat · / search · q quit";
const KEYS_SEARCH: &str = "type to filter · Enter keep · Esc clear · ↑/↓ move · Ctrl+C quit";

/// Start of the list title, before the library root.
const TITLE_PREFIX: &str = " kasset · ";

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

/// Draws the whole screen.
pub fn render<A: Audio>(frame: &mut Frame, app: &App<A>, view: &mut View, now: Instant) {
    let [list, playing, status, keys] = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(NOW_PLAYING_HEIGHT),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    render_list(frame, app, view, list);
    render_now_playing(frame, app, playing);
    render_status(frame, app, status, now);
    let help = match app.mode() {
        Mode::Normal => KEYS_NORMAL,
        Mode::Search => KEYS_SEARCH,
    };
    frame.render_widget(Paragraph::new(help).dark_gray(), keys);
}

fn render_list<A: Audio>(frame: &mut Frame, app: &App<A>, view: &mut View, area: Rect) {
    let total = app.playback().tracks().len();
    let shown = app.visible().len();
    let count = if app.query().is_empty() {
        format!(" {total} tracks ")
    } else {
        format!(" {shown} of {total} match '{}' ", app.query())
    };
    // The left title gets whatever the count leaves free: two borders, the
    // count and a one-column gap. Long roots keep their end, the part that
    // tells folders apart.
    let free = usize::from(area.width).saturating_sub(2 + Span::raw(&count).width() + 1);
    let root_room = free.saturating_sub(Span::raw(TITLE_PREFIX).width() + 1);
    let title = if root_room < 2 {
        " kasset ".to_owned()
    } else {
        format!("{TITLE_PREFIX}{} ", elide_start(app.root(), root_room))
    };
    let block = Block::bordered()
        .padding(Padding::horizontal(1))
        .title(title.bold())
        .title_top(Line::from(count).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if shown == 0 {
        view.offset = 0;
        view.page_size = usize::from(inner.height);
        let message = if total == 0 {
            format!("No music found in {}", app.root())
        } else {
            format!("No tracks match '{}'", app.query())
        };
        frame.render_widget(
            Paragraph::new(message).italic().wrap(Wrap { trim: false }),
            inner,
        );
        return;
    }

    // One line is the header.
    let rows = usize::from(inner.height.saturating_sub(1));
    let selected = app.selected().unwrap_or(0);
    view.offset = scroll(view.offset, selected, rows, shown);
    view.page_size = rows;

    let tracks = app.playback().tracks();
    let current = app.playback().current();
    let state = app.playback().state();
    let end = view.offset.saturating_add(rows).min(shown);
    let body = app.visible()[view.offset..end].iter().map(|&index| {
        let track = &tracks[index];
        let length = track.duration.map(clock).unwrap_or_default();
        let row = Row::new([
            Cell::from(if current == Some(index) {
                state_marker(state)
            } else {
                " "
            }),
            Cell::from(track.title.as_str()),
            Cell::from(track.artist.as_str()),
            Cell::from(track.album.as_str()),
            Cell::from(Line::from(length).right_aligned()),
        ]);
        if current == Some(index) {
            row.style(Style::new().fg(Color::Green).add_modifier(Modifier::BOLD))
        } else {
            row
        }
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
    if app.mode() != Mode::Search {
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
    use crate::test_support::{SilentAudio, track};
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
        App::new(tracks, SilentAudio::default(), "/music".to_owned(), None)
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
        let mut app = App::new(tracks, SilentAudio::default(), "/m".to_owned(), None);
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

    #[test]
    fn long_queries_show_their_end() {
        assert_eq!(tail_fitting("abcdef", 3), "def");
        assert_eq!(tail_fitting("abc", 10), "abc");
        assert_eq!(tail_fitting("日本語", 4), "本語");
        assert_eq!(tail_fitting("日本語", 1), "");
        assert_eq!(tail_fitting("", 0), "");
    }
}
