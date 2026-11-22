//! Drawing the screen from the [`App`] state. Rendering never changes the app;
//! the only state kept here is what the event loop feeds back to it and the
//! panes' scroll positions ([`View`]).
//!
//! Layout, top to bottom: the body, the now-playing area (a top rule, then
//! the track line with the shuffle, repeat and volume indicators at its
//! right end, then the progress line), the status line and the key bar. The
//! body is two panes side by side, the sidebar (the fixed rows "Library",
//! "Artists", "Albums" and "Queue" with their counts, a separator, then the
//! playlists) and the main pane (the library, the open playlist, a browse
//! level or the up-next queue), when the terminal is at least
//! [`TWO_PANE_MIN_WIDTH`] columns wide; below that only the focused pane is
//! drawn, at full width. The focused pane has the
//! focused border and the strong selection style ([`theme`]); each pane
//! keeps its own scroll offset, and the page size fed back to the app is the
//! focused pane's. The sidebar's fixed rows stay on top (in a short pane
//! they are cut from the bottom), its separator is drawn only with a
//! playlist row below it, and only its playlist rows scroll and count as its
//! page. With the visualizer on and the terminal at least
//! [`SPECTRUM_MIN_HEIGHT`] rows tall, the bottom [`SPECTRUM_HEIGHT`] rows of
//! the body are the spectrum strip, across the full width below the panes:
//! one bar per level, 1 column wide with a 1-column gap, filled from the
//! bottom in eighths of a row. It never has focus, so it always has the
//! unfocused border. It draws at most as many bars as fit, so levels
//! measured for an older width never reach outside it. Whether the strip
//! fits and how many bars it has are fed back to the app through [`View`].
//! With the lyrics pane on and the terminal at least [`LYRICS_MIN_WIDTH`]
//! columns wide, a third column, the lyrics pane, is drawn to the right of
//! the main pane (about a third of the width, [`LYRICS_WIDTH`] columns at
//! the least and the most), above the strip. The sidebar keeps the width it
//! has in the two-pane layout and the main pane takes what is left. The
//! lyrics pane never has focus, so it always has the unfocused border.
//! Whether it fits is fed back to the app through [`View`], whether or not
//! it is on. It shows "Nothing playing" before any track, nothing until the
//! current track's lyrics are looked up ([`App::lyrics_ready`]), then "No
//! lyrics" or the lyrics, wrapped by display width ([`lyric_rows`], kept
//! in [`View`] until the lyrics or the pane's width change): timed
//! lyrics with the current line in the playing style near the middle and
//! earlier lines dimmed, plain lyrics scrolled in proportion to the
//! position in the track.
//! Browse levels are tables under a header: artists (name, albums, tracks),
//! an artist's albums ("All tracks" first, then title and tracks), albums
//! (title, album artist, tracks), or tracks with the library's columns. The
//! pane title names the level (an album with its artist), elided at the
//! end; the count leaves "All tracks" out. A row has the playing marker
//! exactly when [`App::browse_row_playing`] says so, so a track list marks
//! its track only when its own album or artist is the play order.
//! The queue view lists the queued tracks in queue order with the library's
//! columns and no playing marker, titled "Queue" with its count; an empty
//! queue says "Queue is empty". `D` on the sidebar's "Queue" row asks in a
//! popup ("Clear the queue (N tracks)? y/n").
//! Popups are centred over the whole body, panes and strip together.
//!
//! The key bar and the help overlay are drawn from [`keys`], the single key
//! table. The bar is cut at whole labels; in Normal mode `? help` keeps its
//! place at the right end. The overlay is centred over everything above the
//! status line, scrolls, and cuts its lines instead of wrapping them, so it
//! always has [`keys::help_line_count`] lines; the rows it showed are kept
//! in [`View`] (only while it is drawn) for the app to clamp its scroll.
//!
//! Every style comes from [`theme`]; this module holds no colour of its own.
//!
//! All text drawn comes from sanitised sources (tags and file names through
//! `text::sanitize`/`display_path`, playlist names through `text::sanitize`
//! here, typed text through input filtering), so no terminal control
//! sequence can reach the screen. Text that can be long (names, the library
//! root, the search query) is elided to its area, never wrapped into others.

use std::ops::Range;
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, List, ListItem, ListState, Padding, Paragraph, Row, Table,
    TableState, Wrap,
};

use crate::app::{
    App, BrowseKind, FIXED_ROWS, Focus, ListView, MainView, Mode, NameInput, NamePurpose, Picker,
    SidebarEntry, View as Screen, artist_album_of_value,
};
use crate::keys::{self, BarContext, HelpLine};
use crate::lyrics::Lyrics;
use crate::playback::{Level, PlayState, clock};
use crate::player::Audio;
use crate::text::sanitize;
use crate::theme;

/// Height of the now-playing area: its top rule and two lines.
const NOW_PLAYING_HEIGHT: u16 = 3;

/// Narrowest terminal, in columns, that shows the sidebar and the main pane
/// side by side.
pub const TWO_PANE_MIN_WIDTH: u16 = 70;

/// Narrowest terminal, in columns, that shows the lyrics pane (with the
/// lyrics pane on). The app also names it in the `l` notice.
pub const LYRICS_MIN_WIDTH: u16 = 100;

/// Narrowest and widest lyrics pane, borders included.
const LYRICS_WIDTH: (u16, u16) = (28, 48);

/// The lyrics pane's title.
const LYRICS_TITLE: &str = " Lyrics ";

/// The lyrics pane's message before any track was played.
const NOTHING_PLAYING: &str = "Nothing playing";

/// The lyrics pane's message for a track without lyrics.
const NO_LYRICS: &str = "No lyrics";

/// Shortest terminal, in rows, that shows the spectrum strip (with the
/// visualizer on). The app also names it in the `v` notice.
pub const SPECTRUM_MIN_HEIGHT: u16 = 24;

/// Height of the spectrum strip: its border and 4 rows of bars.
const SPECTRUM_HEIGHT: u16 = 6;

/// The spectrum strip's title.
const SPECTRUM_TITLE: &str = " Spectrum ";

/// The top cell of a bar, by how many eighths of the cell it fills (1–7);
/// 8 eighths is [`BAR_FULL`].
const BAR_EIGHTHS: [&str; 7] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇"];

/// A cell of a bar that is filled completely.
const BAR_FULL: &str = "█";

/// Narrowest and widest sidebar in the two-pane layout, borders included.
const SIDEBAR_WIDTH: (u16, u16) = (22, 32);

/// Drawn between two labels of the key bar.
const BAR_SEPARATOR: &str = " · ";

/// Least space kept between the key bar's labels and `? help`, and between
/// the now-playing line's text and its indicators.
const GAP: usize = 2;

/// Least width the now-playing line keeps for the state and the start of
/// the title when the indicators take the rest: marker, space, 3 columns
/// of title and the ellipsis.
const NOW_PLAYING_MIN_LEFT: usize = 6;

/// The help overlay's title.
const HELP_TITLE: &str = " Keys ";

/// Start of the library pane's title, before the library root.
const LIBRARY_PREFIX: &str = " Library · ";

/// The library pane's title when there is no room for the root.
const LIBRARY_TITLE: &str = " Library ";

/// The sidebar's title.
const SIDEBAR_TITLE: &str = " kasset ";

/// Names of the sidebar's fixed rows, top down (values 0 to 3).
const FIXED_ROW_NAMES: [&str; FIXED_ROWS] = ["Library", "Artists", "Albums", "Queue"];

/// The first row of an artist's albums.
const ALL_TRACKS_ROW: &str = "All tracks";

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

/// `spans` cut at the end to fit `max` columns, with `…` in the style of the
/// span that was cut marking the cut. Spans after the cut are dropped.
fn fit_spans(spans: Vec<Span<'_>>, max: usize) -> Vec<Span<'_>> {
    if spans.iter().map(Span::width).sum::<usize>() <= max {
        return spans;
    }
    if max == 0 {
        return Vec::new();
    }
    let mut room = max - 1; // the ellipsis
    let mut fitted = Vec::with_capacity(spans.len());
    for span in spans {
        let span_width = span.width();
        if span_width <= room {
            room -= span_width;
            fitted.push(span);
            continue;
        }
        let mut end = 0;
        for (i, c) in span.content.char_indices() {
            let w = width(&span.content[i..i + c.len_utf8()]);
            if w > room {
                break;
            }
            room -= w;
            end = i + c.len_utf8();
        }
        fitted.push(Span::styled(
            format!("{}…", &span.content[..end]),
            span.style,
        ));
        break;
    }
    fitted
}

/// A playlist name ready to draw.
fn shown_name(name: &str) -> String {
    sanitize(name).unwrap_or_default()
}

/// What rendering remembers between frames.
#[derive(Debug, Default)]
pub struct View {
    /// First playlist row on screen in the sidebar (its fixed rows are always
    /// drawn above them).
    sidebar_offset: usize,
    /// First row on screen in the main pane.
    main_offset: usize,
    /// Rows of the focused pane that fit on screen in the last frame.
    page_size: usize,
    /// Help lines the overlay showed when last drawn (0 until it is drawn).
    help_rows: usize,
    /// Whether the last frame was tall enough for the spectrum strip,
    /// whether or not the visualizer is on.
    spectrum_fits: bool,
    /// Bars the spectrum strip had room for in the last frame (0 when it was
    /// not drawn).
    spectrum_bars: usize,
    /// Whether the last frame was wide enough for the lyrics pane, whether
    /// or not the pane is on.
    lyrics_fits: bool,
    /// The lyrics pane's rows as last wrapped.
    lyrics_rows: Option<WrappedLyrics>,
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

    /// Whether the last frame was tall enough for the spectrum strip.
    #[must_use]
    pub const fn spectrum_fits(&self) -> bool {
        self.spectrum_fits
    }

    /// Bars the spectrum strip had room for in the last frame (0 when it was
    /// not drawn).
    #[must_use]
    pub const fn spectrum_bars(&self) -> usize {
        self.spectrum_bars
    }

    /// Whether the last frame was wide enough for the lyrics pane.
    #[must_use]
    pub const fn lyrics_fits(&self) -> bool {
        self.lyrics_fits
    }
}

/// Which key bar fits what is on screen.
fn bar_context<A: Audio>(app: &App<A>) -> BarContext {
    match app.mode() {
        Mode::Search => BarContext::Search,
        Mode::Picker(_) => BarContext::Picker,
        Mode::NameInput(input) => match input.purpose() {
            NamePurpose::Create(_) => BarContext::Name,
            NamePurpose::Rename(_) => BarContext::Rename,
        },
        Mode::ConfirmDelete(_) => BarContext::ConfirmDelete,
        Mode::ConfirmClearQueue => BarContext::ConfirmClearQueue,
        Mode::Help { .. } => BarContext::Help,
        Mode::Normal => match app.view() {
            Screen::Library => BarContext::Library,
            Screen::Playlists => BarContext::Sidebar,
            Screen::Playlist => BarContext::Playlist,
            // The artist or album list, where `Esc` goes to the sidebar.
            Screen::Groups if app.browse_depth() <= 1 => BarContext::BrowseTop,
            Screen::Groups => BarContext::BrowseGroups,
            Screen::Tracks => BarContext::BrowseTracks,
            Screen::Queue => BarContext::Queue,
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

    view.spectrum_fits = frame.area().height >= SPECTRUM_MIN_HEIGHT;
    let panes = if app.visualizer_on() && view.spectrum_fits {
        let [panes, strip] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(SPECTRUM_HEIGHT)])
                .areas(body);
        view.spectrum_bars = render_spectrum(frame, app.spectrum_levels(), strip);
        panes
    } else {
        view.spectrum_bars = 0;
        body
    };
    render_body(frame, app, view, panes);
    render_now_playing(frame, app, playing);
    render_status(frame, app, status, now);
    render_key_bar(frame, bar_context(app), keys);

    match app.mode() {
        Mode::Picker(picker) => render_picker(frame, picker, body),
        Mode::NameInput(input) => render_name_input(frame, input, body),
        Mode::ConfirmDelete(name) => render_confirm_delete(frame, name, body),
        Mode::ConfirmClearQueue => {
            render_confirm_clear_queue(frame, app.playback().up_next().len(), body);
        }
        Mode::Help { scroll } => render_help(frame, *scroll, body.union(playing), view),
        Mode::Normal | Mode::Search => {}
    }
}

/// Draws `context`'s key bar: its labels left to right, stopping before the
/// first one that does not fit whole, and in the Normal-mode contexts
/// [`keys::HELP_HINT`] at the right end, which always keeps its place.
fn render_key_bar(frame: &mut Frame, context: BarContext, area: Rect) {
    let hint = context.is_normal();
    let room = if hint {
        usize::from(area.width).saturating_sub(width(keys::HELP_HINT) + GAP)
    } else {
        usize::from(area.width)
    };
    let mut text = String::new();
    for label in keys::bar(context) {
        let separator = if text.is_empty() { "" } else { BAR_SEPARATOR };
        if width(&text) + width(separator) + width(label) > room {
            break;
        }
        text.push_str(separator);
        text.push_str(label);
    }
    frame.render_widget(Line::styled(text, theme::DIM), area);
    if hint {
        frame.render_widget(
            Line::styled(keys::HELP_HINT, theme::DIM).right_aligned(),
            area,
        );
    }
}

/// The sidebar's width for a body `body_width` columns wide in the two-pane
/// layout.
fn sidebar_width(body_width: u16) -> u16 {
    (body_width / 4).clamp(SIDEBAR_WIDTH.0, SIDEBAR_WIDTH.1)
}

/// The lyrics pane's width for a body `body_width` columns wide: about a
/// third of it, within [`LYRICS_WIDTH`].
fn lyrics_width(body_width: u16) -> u16 {
    (body_width / 3).clamp(LYRICS_WIDTH.0, LYRICS_WIDTH.1)
}

/// Draws the sidebar and the main pane side by side, with the lyrics pane
/// to their right when it is on and fits, or only the focused pane on a
/// narrow terminal, and records the focused pane's page size and whether
/// the lyrics pane fits.
fn render_body<A: Audio>(frame: &mut Frame, app: &App<A>, view: &mut View, area: Rect) {
    let focus = app.focus();
    view.lyrics_fits = area.width >= LYRICS_MIN_WIDTH;
    view.page_size = if area.width >= TWO_PANE_MIN_WIDTH {
        let lyrics = app.lyrics_on() && view.lyrics_fits;
        let [side, main, lyrics_area] = Layout::horizontal([
            Constraint::Length(sidebar_width(area.width)),
            Constraint::Fill(1),
            Constraint::Length(if lyrics { lyrics_width(area.width) } else { 0 }),
        ])
        .areas(area);
        if lyrics {
            render_lyrics(frame, app, &mut view.lyrics_rows, lyrics_area);
        }
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

/// How a row of the lyrics pane is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LyricRole {
    /// A timed line before the current one: dimmed.
    Past,
    /// The current timed line: the playing style.
    Current,
    /// A timed line after the current one (or any, before the first line).
    Future,
    /// A line of untimed lyrics.
    Plain,
}

impl LyricRole {
    const fn style(self) -> Style {
        match self {
            Self::Past => theme::DIM,
            Self::Current => theme::PLAYING,
            Self::Future | Self::Plain => Style::new(),
        }
    }
}

/// One row of the lyrics pane: a lyric line, or part of a wrapped one.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LyricRow {
    /// Index of the lyric line the row belongs to.
    line: usize,
    text: String,
}

/// The lyrics' rows as last wrapped, kept while neither the lyrics (their
/// [`App::lyrics_generation`]) nor the pane's width change, so a frame
/// only picks the rows it shows.
#[derive(Debug)]
struct WrappedLyrics {
    generation: u64,
    columns: usize,
    rows: Vec<LyricRow>,
}

/// Width of `text` as drawn: the sum of its grapheme clusters' widths, as
/// the screen buffer counts them.
fn drawn_width(text: &str) -> usize {
    Span::raw(text)
        .styled_graphemes(Style::new())
        .map(|g| width(g.symbol))
        .sum()
}

/// `text` wrapped into rows of at most `columns` display columns (at least
/// 1): at spaces where possible, with a word wider than a row broken
/// between grapheme clusters. A cluster wider than a whole row gets a row
/// of its own. Blank text is one empty row.
fn wrap(text: &str, columns: usize) -> Vec<String> {
    let columns = columns.max(1);
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut used = 0;
    for word in text.split_whitespace() {
        let word_width = drawn_width(word);
        let gap = usize::from(!row.is_empty());
        if used + gap + word_width <= columns {
            if gap == 1 {
                row.push(' ');
            }
            row.push_str(word);
            used += gap + word_width;
            continue;
        }
        if !row.is_empty() {
            rows.push(std::mem::take(&mut row));
            used = 0;
        }
        if word_width <= columns {
            row.push_str(word);
            used = word_width;
            continue;
        }
        let span = Span::raw(word);
        for grapheme in span.styled_graphemes(Style::new()) {
            let grapheme_width = width(grapheme.symbol);
            if used + grapheme_width > columns && !row.is_empty() {
                rows.push(std::mem::take(&mut row));
                used = 0;
            }
            row.push_str(grapheme.symbol);
            used += grapheme_width;
        }
    }
    if !row.is_empty() || rows.is_empty() {
        rows.push(row);
    }
    rows
}

/// The rows of `lyrics` wrapped to `columns` display columns, in order,
/// each with the index of its line.
fn lyric_rows(lyrics: &Lyrics, columns: usize) -> Vec<LyricRow> {
    let texts: Vec<&str> = match lyrics {
        Lyrics::Synced(lines) => lines.iter().map(|l| l.text.as_str()).collect(),
        Lyrics::Plain(lines) => lines.iter().map(String::as_str).collect(),
    };
    texts
        .into_iter()
        .enumerate()
        .flat_map(|(line, text)| {
            wrap(text, columns)
                .into_iter()
                .map(move |text| LyricRow { line, text })
        })
        .collect()
}

/// The rows (of `rows`, in line order) that line `current` takes.
fn current_rows(rows: &[LyricRow], current: Option<usize>) -> Option<Range<usize>> {
    let current = current?;
    let start = rows.partition_point(|row| row.line < current);
    let end = rows.partition_point(|row| row.line <= current);
    (start < end).then_some(start..end)
}

/// How a row of line `line` is drawn while line `current` is current.
fn row_role(lyrics: &Lyrics, line: usize, current: Option<usize>) -> LyricRole {
    match (lyrics, current) {
        (Lyrics::Plain(_), _) => LyricRole::Plain,
        (Lyrics::Synced(_), Some(c)) if line < c => LyricRole::Past,
        (Lyrics::Synced(_), Some(c)) if line == c => LyricRole::Current,
        (Lyrics::Synced(_), _) => LyricRole::Future,
    }
}

/// First row shown of `total` timed-lyric rows in a pane `height` rows
/// tall: the current line's rows in the middle where possible (its first
/// row at the top when it is taller than the pane), the top when there is
/// no current line.
fn synced_top(current: Option<&Range<usize>>, total: usize, height: usize) -> usize {
    current
        .map_or(0, |rows| {
            rows.start
                .saturating_sub(height.saturating_sub(rows.len()) / 2)
        })
        .min(total.saturating_sub(height))
}

/// First row shown of `total` plain-lyric rows in a pane `height` rows tall,
/// at `position` in a track `duration` long: the rows that do not fit
/// scroll by in proportion to the position, the top when the length is
/// unknown or zero. Never past the last full screen.
fn plain_top(total: usize, height: usize, position: Duration, duration: Option<Duration>) -> usize {
    let span = total.saturating_sub(height);
    let Some(length) = duration.map(|d| d.as_millis()).filter(|&ms| ms > 0) else {
        return 0;
    };
    if span == 0 {
        return 0;
    }
    let played = position.as_millis().min(length);
    // Real spans (at most a few million rows) times `played` (under 2^74)
    // fit a u128 easily; absurd ones saturate, and the result is clamped
    // to `span` either way.
    let top = u128::try_from(span).map_or(0, |span| span.saturating_mul(played) / length);
    usize::try_from(top).map_or(span, |top| top.min(span))
}

/// Draws the lyrics pane in `area`: always unfocused, titled " Lyrics ".
/// `wrapped` keeps the wrapped rows between frames.
fn render_lyrics<A: Audio>(
    frame: &mut Frame,
    app: &App<A>,
    wrapped: &mut Option<WrappedLyrics>,
    area: Rect,
) {
    let (border, title) = theme::border(false);
    let block = Block::bordered()
        .border_style(border)
        .title_style(title)
        .padding(Padding::horizontal(1))
        .title(LYRICS_TITLE);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let playback = app.playback();
    // The offset of an empty pane is never used: it does not scroll.
    let mut unused = 0;
    if playback.current_track().is_none() {
        *wrapped = None;
        render_empty(frame, &mut unused, inner, NOTHING_PLAYING.to_owned());
        return;
    }
    if !app.lyrics_ready() {
        // Not looked up yet: drawn on the next frame, without a flash of
        // "No lyrics" first.
        return;
    }
    let Some(lyrics) = app.lyrics() else {
        *wrapped = None;
        render_empty(frame, &mut unused, inner, NO_LYRICS.to_owned());
        return;
    };

    let generation = app.lyrics_generation();
    let columns = usize::from(inner.width);
    let rows = match wrapped {
        Some(w) if w.generation == generation && w.columns == columns => &w.rows,
        _ => {
            &wrapped
                .insert(WrappedLyrics {
                    generation,
                    columns,
                    rows: lyric_rows(lyrics, columns),
                })
                .rows
        }
    };
    let position = playback.position();
    let height = usize::from(inner.height);
    let current = lyrics.current(position);
    let top = match lyrics {
        Lyrics::Synced(_) => synced_top(current_rows(rows, current).as_ref(), rows.len(), height),
        Lyrics::Plain(_) => plain_top(rows.len(), height, position, playback.duration()),
    };
    let lines: Vec<Line<'_>> = rows
        .iter()
        .skip(top)
        .take(height)
        .map(|row| {
            Line::styled(
                row.text.as_str(),
                row_role(lyrics, row.line, current).style(),
            )
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// How many bars fit `columns` inner columns: 1 column each, with a
/// 1-column gap between two bars.
fn spectrum_bar_count(columns: u16) -> usize {
    usize::from(columns).div_ceil(2)
}

/// How many eighths of a cell a bar `rows` rows tall fills at `level`
/// (clamped to `0.0..=1.0`, so infinities draw an empty or a full bar; NaN
/// draws nothing), rounded to the nearest eighth.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is a whole number in 0..=rows * 8 (at most 524 280), \
              which every usize holds and an f32 represents exactly"
)]
fn bar_eighths(level: f32, rows: u16) -> usize {
    if level.is_nan() {
        return 0;
    }
    (level.clamp(0.0, 1.0) * f32::from(rows) * 8.0).round() as usize
}

/// Draws the spectrum strip in `area`: one bar per level, lowest band on
/// the left, as many as fit. Returns how many bars fit.
fn render_spectrum(frame: &mut Frame, levels: &[f32], area: Rect) -> usize {
    let (border, title) = theme::border(false);
    let block = Block::bordered()
        .border_style(border)
        .title(Span::styled(SPECTRUM_TITLE, title));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let bars = spectrum_bar_count(inner.width);
    let rows = inner.height;
    let buffer = frame.buffer_mut();
    for (x, &level) in (inner.x..inner.right()).step_by(2).zip(levels) {
        let mut eighths = bar_eighths(level, rows);
        for y in (inner.y..inner.bottom()).rev() {
            if eighths == 0 {
                break;
            }
            let symbol = match eighths {
                8.. => BAR_FULL,
                // `1..=7` here, so the index is in `0..=6`.
                partial => BAR_EIGHTHS[partial - 1],
            };
            buffer[(x, y)].set_symbol(symbol).set_style(theme::SPECTRUM);
            eighths = eighths.saturating_sub(8);
        }
    }
    bars
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
        MainView::Browse(kind) => render_browse(frame, app, offset, area, focused, kind),
        MainView::Queue => render_queue(frame, app, offset, area, focused),
    }
}

/// The up-next queue in the main pane, in queue order, with the library's
/// columns and no playing marker (a queued track leaves the queue when it
/// starts, and the sidebar's "Queue" row marks it then). An empty queue
/// says so; a filter with no match says what was looked for. Returns the
/// list rows that fit.
fn render_queue<A: Audio>(
    frame: &mut Frame,
    app: &App<A>,
    offset: &mut usize,
    area: Rect,
    focused: bool,
) -> usize {
    let list = app.main_list();
    let up_next = app.playback().up_next();
    let count = pane_count(&list, list.rows.len(), up_next.len(), "tracks", area.width);
    let block = pane_block(area, focused, |room| plain_title("Queue", room), count);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if list.rows.is_empty() {
        let message = if up_next.is_empty() {
            "Queue is empty".to_owned()
        } else {
            format!("No tracks match '{}'", list.query)
        };
        return render_empty(frame, offset, inner, message);
    }
    // Rows are positions valid for this frame's queue: `App::handle_key` and
    // `App::tick` sync the view (`sync_queue_view`) and the event loop always
    // ticks before it draws, so tests that change the queue through
    // `playback_mut` must tick (or press a key) before drawing. The queue
    // view marks no row as playing.
    debug_assert!(
        list.rows.iter().all(|&pos| pos < up_next.len()),
        "queue view rows are synced with the queue"
    );
    // An out-of-range position (never, see above) names no track and its
    // row is dropped.
    render_tracks(frame, app, &list, offset, inner, focused, |pos| {
        (up_next.get(pos).copied().unwrap_or(usize::MAX), false)
    })
}

/// A pane title ` text ` cut at the end to fit `room` columns (nothing when
/// not even a few characters fit).
fn plain_title(text: &str, room: usize) -> String {
    if room >= 4 {
        format!(" {} ", elide_end(text, room - 2))
    } else {
        String::new()
    }
}

/// One column of an artist or album table, after the marker column.
struct Column {
    header: &'static str,
    width: Constraint,
    /// A count: right-aligned, header included.
    count: bool,
}

impl Column {
    /// A name column taking `share` of the room left by the counts.
    const fn name(header: &'static str, share: u16) -> Self {
        Self {
            header,
            width: Constraint::Fill(share),
            count: false,
        }
    }

    /// A count column, as wide as its header ("Albums", "Tracks").
    const fn count(header: &'static str) -> Self {
        Self {
            header,
            // The headers are ASCII, so their length is their width.
            #[expect(clippy::cast_possible_truncation, reason = "short constant headers")]
            width: Constraint::Length(header.len() as u16),
            count: true,
        }
    }
}

/// The artist table: name, albums, tracks.
const ARTIST_COLUMNS: [Column; 3] = [
    Column::name("Artist", 1),
    Column::count("Albums"),
    Column::count("Tracks"),
];

/// An artist's albums: title, tracks.
const ARTIST_ALBUM_COLUMNS: [Column; 2] = [Column::name("Album", 1), Column::count("Tracks")];

/// The album table: title, album artist, tracks.
const ALBUM_COLUMNS: [Column; 3] = [
    Column::name("Album", 4),
    Column::name("Artist", 3),
    Column::count("Tracks"),
];

/// Draws a browse level: the artist table (name, albums, tracks), an
/// artist's albums ("All tracks", then title and tracks), the album table
/// (title, album artist, tracks), or a list of tracks with the library's
/// columns. The title names the level (an album with its artist) and the
/// count says how many entries are listed ("All tracks" is listed but not
/// counted). Rows get the playing marker per [`App::browse_row_playing`].
/// Names come from the catalog, whose text is the library's sanitised tag
/// text. Returns the list rows that fit.
fn render_browse<A: Audio>(
    frame: &mut Frame,
    app: &App<A>,
    offset: &mut usize,
    area: Rect,
    focused: bool,
    kind: BrowseKind,
) -> usize {
    let catalog = app.catalog();
    let list = app.main_list();
    let artist_name = |a: usize| catalog.artist(a).map_or("", |artist| artist.name());
    let album_title = |b: usize| catalog.album(b).map_or("", |album| album.title());
    let album_artist = |b: usize| {
        catalog
            .album(b)
            .map_or("", |album| artist_name(album.artist()))
    };
    let (title, total, noun) = match kind {
        BrowseKind::Artists => ("Artists".to_owned(), catalog.artists().len(), "artists"),
        BrowseKind::ArtistAlbums(a) => (
            artist_name(a).to_owned(),
            catalog.artist(a).map_or(0, |artist| artist.albums().len()),
            "albums",
        ),
        BrowseKind::Albums => ("Albums".to_owned(), catalog.albums().len(), "albums"),
        BrowseKind::Album(b) => (
            format!("{} · {}", album_title(b), album_artist(b)),
            app.browse_tracks().len(),
            "tracks",
        ),
        BrowseKind::ArtistTracks(a) => (
            format!("{} · {ALL_TRACKS_ROW}", artist_name(a)),
            app.browse_tracks().len(),
            "tracks",
        ),
    };
    let shown = match kind {
        BrowseKind::ArtistAlbums(_) => list
            .rows
            .iter()
            .filter(|&&v| artist_album_of_value(v).is_some())
            .count(),
        _ => list.rows.len(),
    };
    let count = pane_count(&list, shown, total, noun, area.width);
    let block = pane_block(area, focused, |room| plain_title(&title, room), count);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if list.rows.is_empty() {
        let message = if total == 0 {
            format!("No {noun}")
        } else {
            format!("No {noun} match '{}'", list.query)
        };
        return render_empty(frame, offset, inner, message);
    }
    render_browse_rows(frame, app, &list, offset, inner, focused, kind)
}

/// Draws the rows of browse level `kind` (not empty) in `inner`; returns
/// the rows that fit.
fn render_browse_rows<A: Audio>(
    frame: &mut Frame,
    app: &App<A>,
    list: &ListView,
    offset: &mut usize,
    inner: Rect,
    focused: bool,
    kind: BrowseKind,
) -> usize {
    let catalog = app.catalog();
    let album_title = |b: usize| catalog.album(b).map_or("", |album| album.title());
    let album_tracks = |b: usize| catalog.album(b).map_or(0, |album| album.tracks().len());
    let state = app.playback().state();
    let playing = |value: usize| app.browse_row_playing(kind, value);
    let count_cell = |n: usize| Cell::from(Line::from(n.to_string()).right_aligned());
    match kind {
        BrowseKind::Album(_) | BrowseKind::ArtistTracks(_) => {
            let items = app.browse_tracks();
            render_tracks(frame, app, list, offset, inner, focused, |item| {
                (
                    items.get(item).copied().unwrap_or(usize::MAX),
                    playing(item),
                )
            })
        }
        BrowseKind::Artists => {
            render_groups(frame, list, offset, inner, focused, &ARTIST_COLUMNS, |a| {
                let artist = catalog.artist(a)?;
                let cells = [
                    Cell::from(artist.name()),
                    count_cell(artist.albums().len()),
                    count_cell(artist.track_count()),
                ];
                Some(group_row(cells, playing(a), state))
            })
        }
        BrowseKind::ArtistAlbums(a) => {
            let artist = catalog.artist(a);
            render_groups(
                frame,
                list,
                offset,
                inner,
                focused,
                &ARTIST_ALBUM_COLUMNS,
                |value| {
                    let (name, tracks) = match artist_album_of_value(value) {
                        None => (ALL_TRACKS_ROW, artist?.track_count()),
                        Some(i) => {
                            let b = *artist?.albums().get(i)?;
                            (album_title(b), album_tracks(b))
                        }
                    };
                    let cells = [Cell::from(name), count_cell(tracks)];
                    Some(group_row(cells, playing(value), state))
                },
            )
        }
        BrowseKind::Albums => {
            render_groups(frame, list, offset, inner, focused, &ALBUM_COLUMNS, |b| {
                let album = catalog.album(b)?;
                let artist = catalog.artist(album.artist()).map_or("", |a| a.name());
                let cells = [
                    Cell::from(album.title()),
                    Cell::from(artist),
                    count_cell(album.tracks().len()),
                ];
                Some(group_row(cells, playing(b), state))
            })
        }
    }
}

/// A row of an artist or album table: the playing marker (in the playing
/// style when `playing`), then `cells`.
fn group_row<const N: usize>(cells: [Cell<'_>; N], playing: bool, state: PlayState) -> Row<'_> {
    let marker = Cell::from(if playing { state_marker(state) } else { " " });
    let row = Row::new(std::iter::once(marker).chain(cells));
    if playing {
        row.style(theme::PLAYING)
    } else {
        row
    }
}

/// Draws a table of artists or albums under a header: a marker column, then
/// `columns`. `row(value)` gives a row ([`group_row`]; `None` skips it).
/// Scrolled to keep the selection on screen. Returns the rows that fit.
fn render_groups<'a>(
    frame: &mut Frame,
    list: &ListView,
    offset: &mut usize,
    inner: Rect,
    focused: bool,
    columns: &[Column],
    row: impl Fn(usize) -> Option<Row<'a>>,
) -> usize {
    let shown = list.rows.len();
    // One line is the header.
    let rows = usize::from(inner.height.saturating_sub(1));
    let selected = list.selected.unwrap_or(0);
    *offset = scroll(*offset, selected, rows, shown);
    let end = offset.saturating_add(rows).min(shown);
    let body = list.rows[*offset..end]
        .iter()
        .filter_map(|&value| row(value));
    let header = Row::new(
        std::iter::once(Cell::from("")).chain(columns.iter().map(|column| {
            let line = Line::from(column.header);
            Cell::from(if column.count {
                line.right_aligned()
            } else {
                line
            })
        })),
    )
    .style(theme::HEADER);
    let table = Table::new(
        body,
        std::iter::once(Constraint::Length(1)).chain(columns.iter().map(|column| column.width)),
    )
    .header(header)
    .row_highlight_style(theme::selected(focused));
    let mut state = TableState::new().with_selected(Some(selected.saturating_sub(*offset)));
    frame.render_stateful_widget(table, inner, &mut state);
    rows
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

/// What the sidebar says below its fixed rows when no playlist is listed.
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

/// `(row in list.rows, sidebar value, index in App::playlists)` of each
/// playlist row of the sidebar `list`, in order.
fn sidebar_playlist_rows(list: &ListView) -> Vec<(usize, usize, usize)> {
    list.rows
        .iter()
        .enumerate()
        .filter_map(|(row, &value)| match SidebarEntry::of_value(value) {
            SidebarEntry::Playlist(i) => Some((row, value, i)),
            SidebarEntry::Library
            | SidebarEntry::Artists
            | SidebarEntry::Albums
            | SidebarEntry::Queue => None,
        })
        .collect()
}

/// The counts drawn on the sidebar's fixed rows, top down: tracks, artists,
/// albums and queued tracks.
fn fixed_row_counts<A: Audio>(app: &App<A>) -> [usize; FIXED_ROWS] {
    let playback = app.playback();
    let catalog = app.catalog();
    [
        playback.tracks().len(),
        catalog.artists().len(),
        catalog.albums().len(),
        playback.up_next().len(),
    ]
}

/// Draws the sidebar: the fixed rows ("Library", "Artists", "Albums",
/// "Queue"), a separator when a playlist row fits below it, then the
/// playlists, or a hint when none is listed. The fixed rows stay on top (in a short pane
/// they are cut from the bottom); only the playlists scroll (`offset` counts
/// playlist rows). Returns the playlist rows that fit.
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
    let playlist_rows = sidebar_playlist_rows(&list);
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

    // The fixed rows, top down, as many as fit.
    let mut rest = inner;
    for (value, (name, count)) in FIXED_ROW_NAMES
        .iter()
        .zip(fixed_row_counts(app))
        .enumerate()
    {
        if rest.height == 0 {
            break;
        }
        draw_row(frame, value, name, Style::new(), &count.to_string(), rest.y);
        rest.y += 1;
        rest.height -= 1;
    }

    // A separator under the fixed rows when a playlist row fits below it.
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

    // With a fixed row selected the playlists keep their scroll position.
    let selected = list
        .selected
        .and_then(|row| playlist_rows.iter().position(|&(r, _, _)| r == row))
        .unwrap_or(*offset);
    *offset = scroll(*offset, selected, rows, playlist_rows.len());
    let end = offset.saturating_add(rows).min(playlist_rows.len());
    for (y, &(_, value, i)) in (rest.y..).zip(&playlist_rows[*offset..end]) {
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
        draw_row(frame, value, &shown_name(&info.name), name_style, &count, y);
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

/// The "clear the queue?" question, naming how many tracks are queued.
fn render_confirm_clear_queue(frame: &mut Frame, queued: usize, area: Rect) {
    let popup = centered(area, POPUP_MAX_WIDTH, 4);
    let block = Block::bordered()
        .padding(Padding::horizontal(1))
        .title(Span::styled(" Clear queue ", theme::STRONG));
    let inner = block.inner(popup);
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);
    let [question_line, hint_line] = Layout::vertical([Constraint::Length(1); 2]).areas(inner);
    let noun = if queued == 1 { "track" } else { "tracks" };
    let question = format!("Clear the queue ({queued} {noun})? y/n");
    let room = usize::from(question_line.width);
    frame.render_widget(Line::from(elide_end(&question, room)), question_line);
    frame.render_widget(
        Line::styled(
            elide_end("y empties the queue, other keys cancel", room),
            theme::DIM,
        ),
        hint_line,
    );
}

/// Draws the help overlay over `area` from line `scroll` of
/// [`keys::help_lines`], and records how many lines fit in
/// [`View::help_rows`]. Lines are cut with `…`, never wrapped, so the line
/// count is the same at every width.
fn render_help(frame: &mut Frame, scroll: usize, area: Rect, view: &mut View) {
    let keys_width = keys::keys_width();
    let total = keys::help_line_count();
    let widest = keys::help_lines()
        .map(|line| help_spans(line, keys_width).iter().map(Span::width).sum())
        .max()
        .unwrap_or(0);
    // As wide as the widest line (borders and padding included), so nothing
    // is cut on a wide enough terminal.
    let popup_width = u16::try_from(widest + 4).unwrap_or(u16::MAX);
    let height = u16::try_from(total.saturating_add(2)).unwrap_or(u16::MAX);
    let popup = centered(area, popup_width, height);
    let mut block = Block::bordered()
        .padding(Padding::horizontal(1))
        .title(Span::styled(HELP_TITLE, theme::STRONG));
    let inner = block.inner(popup);
    let rows = usize::from(inner.height);
    view.help_rows = rows;
    // The app clamps `scroll` with the previous frame's rows; clamp again
    // so a resize never shows blank lines past the end.
    let first = scroll.min(total.saturating_sub(rows));
    let shown = rows.min(total - first);
    // The position, only when it fits beside the title on the top border.
    let position = format!(" {}–{} of {total} ", first + 1, first + shown);
    let border = usize::from(popup.width).saturating_sub(2);
    if shown < total && width(HELP_TITLE) + width(&position) <= border {
        block = block.title_top(Line::from(position).right_aligned());
    }

    let room = usize::from(inner.width);
    let lines: Vec<Line> = keys::help_lines()
        .skip(first)
        .take(rows)
        .map(|line| Line::from(fit_spans(help_spans(line, keys_width), room)))
        .collect();
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);
    frame.render_widget(Paragraph::new(lines), inner);
}

/// One help line as drawn, laid out like `kasset --help`: titles flush
/// left, keys indented and padded to `keys_width`, then the action.
fn help_spans(line: HelpLine, keys_width: usize) -> Vec<Span<'static>> {
    match line {
        HelpLine::Title(title) => vec![Span::styled(title, theme::SECTION)],
        HelpLine::Key(entry) => vec![
            Span::styled(format!("  {:keys_width$}", entry.keys), theme::STRONG),
            Span::raw("  "),
            Span::raw(entry.action),
        ],
        HelpLine::Blank => Vec::new(),
    }
}

/// The first row to show so that `selected` is on screen, moving the previous
/// `offset` as little as possible and never leaving blank rows at the bottom.
/// The result is always within the list (`< len`, or 0 when it is empty), so
/// `offset..` is a valid slice start even when no row fits (`rows == 0`).
/// A `selected` past the end (e.g. a stale offset passed for want of a
/// selection) counts as the last row.
fn scroll(offset: usize, selected: usize, rows: usize, len: usize) -> usize {
    let selected = selected.min(len.saturating_sub(1));
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

/// Draws the now-playing area: a top rule titled "Now playing", the state,
/// title, artist and album with the indicators at the right, and the
/// progress line.
fn render_now_playing<A: Audio>(frame: &mut Frame, app: &App<A>, area: Rect) {
    let block = Block::default()
        .borders(Borders::TOP)
        .padding(Padding::horizontal(1))
        .title(" Now playing ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [info, progress] = Layout::vertical([Constraint::Length(1); 2]).areas(inner);

    let playback = app.playback();
    let state = playback.state();
    let state_style = match state {
        PlayState::Playing => theme::PLAYING,
        PlayState::Paused => theme::PAUSED,
        PlayState::Stopped => theme::STOPPED,
    };
    let mut left = vec![
        Span::styled(state_marker(state), state_style),
        Span::raw(" "),
    ];
    if let Some(track) = playback.current_track() {
        left.push(Span::styled(track.title.as_str(), theme::STRONG));
        left.push(Span::raw(format!(" — {} · {}", track.artist, track.album)));
    } else {
        left.push(Span::styled(
            "select a track and press Enter",
            theme::MESSAGE,
        ));
    }
    let indicators = format!(
        "shuffle {} · repeat {} · vol {}%",
        if playback.shuffle() { "on" } else { "off" },
        playback.repeat().label(),
        playback.volume().percent()
    );

    // The indicators come first; the text keeps at least the state and the
    // start of the title, and gets the rest.
    let available = usize::from(info.width);
    let left_width: usize = left.iter().map(Span::width).sum();
    let indicators = elide_end(
        &indicators,
        available.saturating_sub(left_width.min(NOW_PLAYING_MIN_LEFT) + GAP),
    );
    let indicators_width = width(&indicators);
    let left_room = if indicators_width == 0 {
        available
    } else {
        available.saturating_sub(indicators_width + GAP)
    };
    frame.render_widget(Line::from(fit_spans(left, left_room)), info);
    frame.render_widget(Line::from(indicators).right_aligned(), info);

    frame.render_widget(
        progress_line(playback.position(), playback.duration(), progress.width),
        progress,
    );
}

/// `elapsed ━━━━╸──── total` in `columns` columns. The bar takes the columns
/// the two times leave; it is empty when the length is unknown.
fn progress_line(position: Duration, duration: Option<Duration>, columns: u16) -> Line<'static> {
    let elapsed = clock(position);
    let total = duration.map_or_else(|| "-:--".to_owned(), clock);
    let times = u16::try_from(width(&elapsed) + width(&total) + 2).unwrap_or(u16::MAX);
    let cells = columns.saturating_sub(times);
    let filled = filled_cells(position, duration, cells);
    // The head marks a track that has started but not reached the end.
    let started = !position.is_zero() && duration.is_some_and(|d| !d.is_zero());
    let head = u16::from(started && filled < cells);
    let cell = |glyph: &str, n: u16| glyph.repeat(usize::from(n));
    Line::from(vec![
        Span::raw(elapsed),
        Span::raw(" "),
        Span::styled(cell("━", filled), theme::PROGRESS),
        Span::styled(cell("╸", head), theme::PROGRESS),
        Span::styled(cell("─", cells - filled - head), theme::DIM),
        Span::raw(" "),
        Span::raw(total),
    ])
}

/// How many of `cells` cells of the progress bar are filled: the played
/// fraction of the track, rounded to the nearest cell and always within
/// `0..=cells`; 0 when the length is unknown or zero.
fn filled_cells(position: Duration, duration: Option<Duration>, cells: u16) -> u16 {
    let Some(total) = duration.map(|d| d.as_millis()).filter(|&ms| ms > 0) else {
        return 0;
    };
    let played = position.as_millis().min(total);
    // Exact rounding in integers. A `Duration` has under 2^74 millis and
    // `cells` under 2^16, so nothing here can overflow a u128.
    let filled = (played * u128::from(cells) + total / 2) / total;
    u16::try_from(filled).map_or(cells, |f| f.min(cells))
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
    use crate::test_support::{SilentAudio, ToneAudio, no_store, track};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::style::{Color, Modifier};

    /// Renders `app` at `width`×`height` and returns the screen's cells.
    fn draw_buffer<A: Audio>(app: &App<A>, view: &mut View, width: u16, height: u16) -> Buffer {
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
    fn draw<A: Audio>(app: &App<A>, view: &mut View, width: u16, height: u16) -> Vec<String> {
        lines(&draw_buffer(app, view, width, height))
    }

    fn screen_contains(lines: &[String], needle: &str) -> bool {
        lines.iter().any(|l| l.contains(needle))
    }

    /// `n` tracks, with the lyrics pane off so the two-pane geometry the
    /// tests below measure is unchanged.
    fn library(n: usize) -> App<SilentAudio> {
        let tracks = (0..n)
            .map(|i| track(&format!("Song {i:02}"), "Band", "LP"))
            .collect();
        let mut app = App::new(
            tracks,
            SilentAudio::default(),
            no_store(),
            "/music".to_owned(),
            None,
        );
        app.set_lyrics_on(false);
        app
    }

    /// In the sidebar: selects the first playlist, below the fixed rows.
    const FIRST_PLAYLIST: [KeyCode; 1 + FIXED_ROWS] = [
        KeyCode::Home,
        KeyCode::Down,
        KeyCode::Down,
        KeyCode::Down,
        KeyCode::Down,
    ];

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

    /// A one-track library ("Song" by "Band" on "LP", 3:30 long) playing
    /// at 1:05.
    fn playing_at_1_05() -> App<SilentAudio> {
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
        app
    }

    /// Spec *Track playing*.
    #[test]
    fn now_playing_shows_the_track_and_its_progress() {
        let app = playing_at_1_05();
        let screen = draw(&app, &mut View::default(), 80, 20);
        // Rule, then the two lines of the area, above the status and key bar.
        assert!(screen[15].contains(" Now playing "), "{screen:#?}");
        assert!(screen[16].starts_with(" ▶ Song — Band · LP"), "{screen:#?}");
        let progress = &screen[17];
        assert!(progress.starts_with(" 1:05 ━"), "{progress}");
        assert!(progress.ends_with("─ 3:30 "), "{progress}");
        // " 1:05 " and " 3:30 " plus the padding leave 78 - 10 cells.
        let cells = 78 - 10;
        let filled = progress.chars().filter(|&c| c == '━').count();
        assert_eq!(filled, (cells * 65 * 10 + 2100 / 2) / 2100, "{progress}");
        assert!(
            filled.abs_diff(cells * 31 / 100) <= 1,
            "about 31%: {progress}"
        );
        assert_eq!(progress.chars().filter(|&c| c == '╸').count(), 1);
        assert_eq!(
            progress.chars().filter(|&c| c == '─').count(),
            cells - filled - 1
        );
    }

    /// Spec *Indicators*.
    #[test]
    fn now_playing_shows_shuffle_repeat_and_volume() {
        let mut app = playing_at_1_05();
        let default = draw(&app, &mut View::default(), 80, 20);
        assert!(
            default[16].ends_with("shuffle off · repeat off · vol 100% "),
            "{default:#?}"
        );
        press(&mut app, KeyCode::Char('z'));
        press(&mut app, KeyCode::Char('r'));
        for _ in 0..6 {
            press(&mut app, KeyCode::Char('-'));
        }
        let screen = draw(&app, &mut View::default(), 80, 20);
        assert!(
            screen[16].ends_with("shuffle on · repeat all · vol 70% "),
            "{screen:#?}"
        );
    }

    #[test]
    fn now_playing_without_a_track_says_what_to_do() {
        let app = library(3);
        let screen = draw(&app, &mut View::default(), 80, 20);
        assert!(
            screen[16].starts_with(" ■ select a track and press Enter"),
            "{screen:#?}"
        );
        assert!(screen[17].starts_with(" 0:00 ─"), "{screen:#?}");
        assert!(screen[17].ends_with("─ -:-- "), "{screen:#?}");
        assert!(!screen[17].contains('━') && !screen[17].contains('╸'));
    }

    #[test]
    fn unknown_length_draws_an_empty_bar() {
        let mut tracks = vec![track("Song", "Band", "LP")];
        tracks[0].duration = None;
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
        let progress = &screen[17];
        assert!(progress.starts_with(" 1:05 ─"), "{progress}");
        assert!(progress.ends_with("─ -:-- "), "{progress}");
        assert!(
            !progress.contains('━') && !progress.contains('╸'),
            "{progress}"
        );
    }

    #[test]
    fn long_title_never_pushes_out_the_indicators() {
        let title = "T".repeat(200);
        let mut app = App::new(
            vec![track(&title, "Band", "LP")],
            SilentAudio::default(),
            no_store(),
            "/m".to_owned(),
            None,
        );
        press(&mut app, KeyCode::Enter);
        let screen = draw(&app, &mut View::default(), 80, 20);
        let line = &screen[16];
        assert!(
            line.ends_with("  shuffle off · repeat off · vol 100% "),
            "{line}"
        );
        assert!(line.starts_with(" ▶ TTT"), "{line}");
        assert!(line.contains("T…"), "the title is cut: {line}");
        // So narrow that the indicators give way: the state and the start
        // of the title stay.
        let screen = draw(&app, &mut View::default(), 20, 20);
        assert!(screen[16].starts_with(" ▶ TTT…"), "{screen:#?}");
        assert!(screen[16].contains("shuf"), "{screen:#?}");
    }

    #[test]
    fn progress_bar_fill_is_always_in_range() {
        let s = Duration::from_secs;
        assert_eq!(filled_cells(s(10), None, 50), 0);
        assert_eq!(filled_cells(s(10), Some(Duration::ZERO), 50), 0);
        assert_eq!(filled_cells(s(500), Some(s(200)), 50), 50, "past the end");
        assert_eq!(filled_cells(s(100), Some(s(200)), 50), 25);
        assert_eq!(filled_cells(s(65), Some(s(210)), 100), 31, "rounded");
        assert_eq!(filled_cells(s(100), Some(s(200)), 0), 0);
        assert_eq!(
            filled_cells(Duration::MAX, Some(Duration::MAX), u16::MAX),
            u16::MAX
        );
        assert_eq!(filled_cells(s(1), Some(Duration::MAX), u16::MAX), 0);
        // The bar takes exactly the columns the times leave.
        for columns in [0, 1, 5, 9, 10, 11, 80, u16::MAX] {
            let line = progress_line(s(65), Some(s(210)), columns);
            let cells: usize = line.spans[2..5].iter().map(Span::width).sum();
            assert_eq!(cells, usize::from(columns.saturating_sub(10)), "{columns}");
        }
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
        let mut app = library(100);
        // Without the spectrum strip, which takes 6 rows from 24 rows up.
        press(&mut app, KeyCode::Char('v'));
        let mut view = View::default();
        draw(&app, &mut view, 60, 20);
        // 20 - 3 (now playing) - 2 (status, keys) - 2 (borders) - 1 (header)
        assert_eq!(view.page_size(), 12);
        draw(&app, &mut view, 60, 40);
        assert_eq!(view.page_size(), 32, "resize is picked up");
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
    fn scroll_keeps_the_selection_visible() {
        assert_eq!(scroll(0, 0, 10, 100), 0);
        assert_eq!(scroll(0, 15, 10, 100), 6);
        assert_eq!(scroll(6, 10, 10, 100), 6, "no jump while visible");
        assert_eq!(scroll(6, 3, 10, 100), 3);
        assert_eq!(scroll(90, 2, 10, 5), 0, "list shrank under the view");
        assert_eq!(scroll(3, 7, 0, 100), 7);
        assert_eq!(scroll(9, 9, 0, 3), 2, "no row fits: still within the list");
        assert_eq!(scroll(9, 9, 0, 0), 0);
        assert_eq!(scroll(18, 18, 2, 3), 1, "selection past the end");
        assert_eq!(scroll(18, 18, 5, 3), 0);
        for (offset, selected, rows, len) in [(18, 18, 2, 3), (50, 99, 1, 1), (0, 7, 3, 0)] {
            let first = scroll(offset, selected, rows, len);
            assert!(first < len.max(1), "{offset} {selected} {rows} {len}");
        }
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
        let mut app = App::new(tracks, SilentAudio::default(), store, "/m".to_owned(), None);
        app.set_lyrics_on(false);
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

    /// The key bar: the screen's last line.
    fn key_bar(screen: &[String]) -> &str {
        screen.last().map_or("", String::as_str)
    }

    #[test]
    fn library_help_mentions_playlist_keys() {
        // At a common 80-column width: the bar is cut there, so these keys
        // must come before the transport keys.
        let app = library(3);
        let screen = draw(&app, &mut View::default(), 80, 20);
        let bar = key_bar(&screen);
        assert!(bar.starts_with("Enter play · a/A add · Tab focus"), "{bar}");
        assert!(bar.ends_with("? help"), "{bar}");
    }

    /// Spec *Help hint always visible*.
    #[test]
    fn help_hint_stays_at_the_right_end_of_a_cut_bar() {
        let app = library(3);
        let screen = draw(&app, &mut View::default(), 40, 20);
        let bar = key_bar(&screen);
        assert!(bar.ends_with("? help"), "{bar}");
        // Cut at whole labels, with room left before the hint.
        assert!(
            bar.starts_with("Enter play · a/A add · Tab focus  "),
            "{bar}"
        );
        assert!(!bar.contains("Space"), "{bar}");
        let labels = &bar[..bar.len() - "? help".len()];
        assert!(!labels.trim_end().ends_with('·'), "{bar}");
        // Every Normal-mode label fits at a width wide enough for all.
        let screen = draw(&app, &mut View::default(), 300, 20);
        let bar = key_bar(&screen);
        for label in keys::bar(BarContext::Library) {
            assert!(bar.contains(label), "{label}: {bar}");
        }
        assert!(bar.ends_with("? help"), "{bar}");
        // Too narrow for anything but the hint, and narrower still.
        for w in [9, 6, 3, 1] {
            let screen = draw(&app, &mut View::default(), w, 20);
            let bar = key_bar(&screen);
            assert!(!bar.contains("Enter"), "{w}: {bar}");
            if w >= 6 {
                assert!(bar.ends_with("? help"), "{w}: {bar}");
            }
        }
    }

    /// Specs *Playlist view help* and *Sidebar help*.
    #[test]
    fn open_playlist_help_bar_shows_the_edit_keys() -> anyhow::Result<()> {
        let (_tmp, mut app) = with_playlists()?;
        keys(&mut app, &[KeyCode::Tab]);
        keys(&mut app, &FIRST_PLAYLIST);
        press(&mut app, KeyCode::Enter);
        // Narrow enough that the bar is cut: the edit keys come first.
        let screen = draw(&app, &mut View::default(), 60, 20);
        let bar = key_bar(&screen);
        assert!(bar.contains("d remove"), "{screen:#?}");
        assert!(bar.contains("J/K move"), "{screen:#?}");
        assert!(bar.ends_with("? help"), "{screen:#?}");

        press(&mut app, KeyCode::Esc);
        let screen = draw(&app, &mut View::default(), 60, 20);
        let bar = key_bar(&screen);
        assert!(bar.starts_with("Enter open · R rename · D delete"), "{bar}");
        assert!(bar.ends_with("? help"), "{bar}");
        Ok(())
    }

    /// Spec *No help hint in prompts*, and none while searching.
    #[test]
    fn prompts_and_search_have_no_help_hint() -> anyhow::Result<()> {
        let (_tmp, mut app) = with_playlists()?;
        press(&mut app, KeyCode::Char('/'));
        let screen = draw(&app, &mut View::default(), 200, 20);
        let bar = key_bar(&screen);
        assert!(
            bar.starts_with("type to filter · Enter keep · Esc clear"),
            "{bar}"
        );
        assert!(!bar.contains("? help"), "{bar}");

        press(&mut app, KeyCode::Esc);
        keys(&mut app, &[KeyCode::Tab]);
        keys(&mut app, &FIRST_PLAYLIST);
        app.handle_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT));
        let screen = draw(&app, &mut View::default(), 200, 20);
        let bar = key_bar(&screen);
        assert_eq!(bar.trim_end(), "type a name · Enter rename · Esc cancel");

        press(&mut app, KeyCode::Esc);
        app.handle_key(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::SHIFT));
        let screen = draw(&app, &mut View::default(), 200, 20);
        assert_eq!(
            key_bar(&screen).trim_end(),
            "y delete · any other key cancel"
        );

        press(&mut app, KeyCode::Esc); // cancels
        press(&mut app, KeyCode::Char('?'));
        let screen = draw(&app, &mut View::default(), 200, 20);
        assert_eq!(
            key_bar(&screen).trim_end(),
            "↑/↓ scroll · any other key close"
        );
        Ok(())
    }

    /// Draws `app` like the event loop does: the frame, then the rows it
    /// showed fed back to the app.
    fn draw_and_feed_back(
        app: &mut App<SilentAudio>,
        view: &mut View,
        width: u16,
        height: u16,
    ) -> Vec<String> {
        let screen = draw(app, view, width, height);
        app.set_page_size(view.page_size());
        app.set_help_rows(view.help_rows());
        screen
    }

    /// The text inside the help overlay's borders, line by line.
    fn help_body(screen: &[String]) -> Vec<String> {
        let corner = format!("┌{HELP_TITLE}");
        let Some((top, line)) = screen.iter().enumerate().find(|(_, l)| l.contains(&corner)) else {
            return Vec::new();
        };
        let chars: Vec<char> = line.chars().collect();
        let left = chars
            .windows(2)
            .position(|w| w[0] == '┌' && w[1] == ' ')
            .unwrap_or(0);
        let right = chars
            .iter()
            .skip(left)
            .position(|&c| c == '┐')
            .map_or(chars.len(), |i| left + i);
        screen
            .iter()
            .skip(top + 1)
            .map(|l| l.chars().collect::<Vec<char>>())
            .take_while(|l| l.get(left) == Some(&'│'))
            .map(|l| {
                let inside: String = l[left + 1..right.min(l.len())].iter().collect();
                inside.trim().to_owned()
            })
            .collect()
    }

    /// Spec *Help does not fit*.
    #[test]
    fn help_overlay_scrolls_when_it_does_not_fit() {
        let mut app = library(3);
        let mut view = View::default();
        press(&mut app, KeyCode::Char('?'));
        let screen = draw_and_feed_back(&mut app, &mut view, 80, 15);
        let body = help_body(&screen);
        // 15 rows - status - keys - 2 borders.
        assert_eq!(view.help_rows(), 11, "{screen:#?}");
        assert_eq!(body.len(), 11, "{screen:#?}");
        assert_eq!(body[0], "Library", "{screen:#?}");
        assert!(body[1].starts_with("Enter"), "{screen:#?}");
        let total = keys::help_line_count();
        assert!(
            screen_contains(&screen, &format!(" 1–11 of {total} ")),
            "{screen:#?}"
        );

        press(&mut app, KeyCode::Char('j'));
        let screen = draw_and_feed_back(&mut app, &mut view, 80, 15);
        let moved = help_body(&screen);
        assert_eq!(moved[..10], body[1..], "one line down: {screen:#?}");
        assert!(screen_contains(&screen, " 2–12 of "), "{screen:#?}");

        // At the end, the last line of the table is the last line shown.
        press(&mut app, KeyCode::End);
        let screen = draw_and_feed_back(&mut app, &mut view, 80, 15);
        let body = help_body(&screen);
        assert!(body[10].starts_with("q, Ctrl+C"), "{screen:#?}");
        assert!(
            screen_contains(&screen, &format!(" {}–{total} of {total} ", total - 10)),
            "{screen:#?}"
        );
    }

    #[test]
    fn full_help_overlay_shows_every_group_and_line() {
        let mut app = library(3);
        let mut view = View::default();
        press(&mut app, KeyCode::Char('?'));
        let screen = draw_and_feed_back(&mut app, &mut view, 100, 60);
        let body = help_body(&screen);
        // The scrollable height is the table's line count, all on screen.
        assert_eq!(view.help_rows(), keys::help_line_count(), "{screen:#?}");
        assert_eq!(body.len(), keys::help_line_count(), "{screen:#?}");
        for group in keys::GROUPS {
            assert!(body.iter().any(|l| l == group.title), "{}", group.title);
        }
        for (line, shown) in keys::help_lines().zip(&body) {
            match line {
                HelpLine::Title(title) => assert_eq!(shown, title),
                HelpLine::Key(entry) => {
                    assert!(shown.starts_with(entry.keys), "{shown}");
                    let kept = shown.strip_suffix('…').unwrap_or(shown);
                    let kept = kept.split_at(keys::keys_width()).1.trim_start();
                    assert!(entry.action.starts_with(kept), "{shown}");
                }
                HelpLine::Blank => assert!(shown.is_empty(), "{shown}"),
            }
        }
        // The position marker reads " N–M of TOTAL ".
        let position = format!(" of {} ", keys::help_line_count());
        assert!(
            !screen_contains(&screen, &position),
            "no position when all fit"
        );
        // Scrolling does nothing when everything fits.
        press(&mut app, KeyCode::Char('j'));
        let again = draw_and_feed_back(&mut app, &mut view, 100, 60);
        assert_eq!(screen, again);
        // The overlay is as wide as its widest line: nothing is cut on a
        // wide terminal.
        let screen = draw_and_feed_back(&mut app, &mut view, 120, 60);
        let body = help_body(&screen);
        assert_eq!(body.len(), keys::help_line_count(), "{screen:#?}");
        assert!(!body.iter().any(|l| l.ends_with('…')), "{screen:#?}");
        assert!(
            body.iter().any(|l| l.ends_with("other keys close)")),
            "{screen:#?}"
        );
    }

    #[test]
    fn narrow_help_overlay_cuts_lines_instead_of_wrapping() {
        let mut app = library(3);
        let mut view = View::default();
        press(&mut app, KeyCode::Char('?'));
        let screen = draw_and_feed_back(&mut app, &mut view, 40, 60);
        let body = help_body(&screen);
        assert_eq!(body.len(), keys::help_line_count(), "never wrapped");
        // Borders and padding leave 36 columns.
        assert!(
            body.iter().all(|l| Span::raw(l.as_str()).width() <= 36),
            "{screen:#?}"
        );
        let cut = body.iter().filter(|l| l.ends_with('…')).count();
        assert!(cut > 0, "{screen:#?}");
        assert!(
            body[1].starts_with("Enter") && body[1].ends_with('…'),
            "{screen:#?}"
        );
    }

    #[test]
    fn help_rows_are_kept_while_the_overlay_is_closed() {
        let mut app = library(3);
        let mut view = View::default();
        press(&mut app, KeyCode::Char('?'));
        draw_and_feed_back(&mut app, &mut view, 80, 15);
        assert_eq!(view.help_rows(), 11);
        press(&mut app, KeyCode::Char('x')); // closes
        draw_and_feed_back(&mut app, &mut view, 80, 30);
        assert_eq!(view.help_rows(), 11, "only the overlay sets it");
        // So the first PgDn after reopening moves by what was shown.
        press(&mut app, KeyCode::Char('?'));
        press(&mut app, KeyCode::PageDown);
        assert_eq!(*app.mode(), Mode::Help { scroll: 11 });
    }

    #[test]
    fn help_overlay_at_tiny_sizes_does_not_panic() {
        let mut app = library(3);
        press(&mut app, KeyCode::Char('?'));
        press(&mut app, KeyCode::End);
        for (w, h) in [
            (0, 0),
            (1, 1),
            (2, 3),
            (10, 5),
            (80, 4),
            (5, 80),
            (300, 2),
            (64, 6),
        ] {
            let mut view = View::default();
            draw_and_feed_back(&mut app, &mut view, w, h);
        }
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
        keys(&mut app, &[KeyCode::Tab]);
        keys(&mut app, &FIRST_PLAYLIST);
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

    /// Text and width of `spans`.
    fn joined(spans: &[Span<'_>]) -> (String, usize) {
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        let w = spans.iter().map(Span::width).sum();
        (text, w)
    }

    #[test]
    fn fit_spans_cuts_across_spans_within_the_width() {
        let spans = || vec![Span::raw("ab"), Span::styled("日本", theme::STRONG)];
        assert_eq!(joined(&fit_spans(spans(), 6)), ("ab日本".to_owned(), 6));
        // The cut lands in the second span: a wide char that does not fit
        // whole is dropped, and the ellipsis takes that span's style.
        let cut = fit_spans(spans(), 5);
        assert_eq!(joined(&cut), ("ab日…".to_owned(), 5));
        assert_eq!(cut[1].style, theme::STRONG);
        assert_eq!(joined(&fit_spans(spans(), 4)), ("ab…".to_owned(), 3));
        assert_eq!(joined(&fit_spans(spans(), 3)), ("ab…".to_owned(), 3));
        // The cut lands in the first span; later spans are dropped.
        let cut = fit_spans(spans(), 2);
        assert_eq!(joined(&cut), ("a…".to_owned(), 2));
        assert_eq!(cut.len(), 1);
        assert_eq!(joined(&fit_spans(spans(), 1)), ("…".to_owned(), 1));
        assert_eq!(joined(&fit_spans(spans(), 0)), (String::new(), 0));
        // Zero-width characters cost nothing and stay with their base.
        let combining = vec![Span::raw("e\u{301}e\u{301}e\u{301}")];
        assert_eq!(joined(&fit_spans(combining, 3)).1, 3);
        let combining = vec![Span::raw("e\u{301}e\u{301}e\u{301}x")];
        assert_eq!(
            joined(&fit_spans(combining, 3)),
            ("e\u{301}e\u{301}…".to_owned(), 3)
        );
        for max in 0..8 {
            assert!(joined(&fit_spans(spans(), max)).1 <= max, "{max}");
        }
    }

    #[test]
    fn help_position_is_dropped_when_it_does_not_fit_the_border() {
        let mut app = library(3);
        press(&mut app, KeyCode::Char('?'));
        press(&mut app, KeyCode::Char('j'));
        let total = keys::help_line_count();
        for w in [6, 10, 14, 20, 24] {
            let mut view = View::default();
            let screen = draw_and_feed_back(&mut app, &mut view, w, 15);
            let top = screen
                .iter()
                .find(|l| l.contains('┌'))
                .cloned()
                .unwrap_or_default();
            let position = format!(" of {total} ");
            let fits = usize::from(w)
                >= 2 + HELP_TITLE.len() + format!(" 2–12 of {total} ").chars().count();
            assert_eq!(top.contains(&position), fits, "{w}: {top}");
            if top.contains(&position) {
                assert!(top.contains(HELP_TITLE), "{w}: {top}");
            }
        }
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
        let mut app = App::new(tracks, SilentAudio::default(), store, "/m".to_owned(), None);
        app.set_lyrics_on(false);
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
        // Body 15 rows: the table has 15 - 2 borders - 1 header rows.
        assert_eq!(view.page_size(), 12, "the main pane's page");
        press(&mut app, KeyCode::End);
        draw(&app, &mut view, 100, 20);
        keys(&mut app, &[KeyCode::Tab, KeyCode::End]);
        let screen = draw(&app, &mut view, 100, 20);
        // The sidebar's rows below its fixed rows and separator: 15 rows
        // less 2 borders, the fixed rows and the separator.
        assert_eq!(view.page_size(), 13 - FIXED_ROWS - 1, "the sidebar's page");
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
        // Sidebar rows: the fixed rows, "Gym", "Old", "Running".
        keys(&mut app, &[KeyCode::Tab]);
        keys(&mut app, &FIRST_PLAYLIST);
        keys(&mut app, &[KeyCode::Down, KeyCode::Enter]);
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
        // Body heights from 6 down to 1: from a few sidebar rows to none.
        for h in [11, 10, 9, 8, 7, 6, 30, 11] {
            draw(&app, &mut view, 100, h);
            draw(&app, &mut view, 60, h);
        }
        // A filter shrinks the rows too.
        for c in "/p0".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        draw(&app, &mut view, 100, 11);
        draw(&app, &mut view, 100, 8);
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

    /// Draws `app` like the event loop does, spectrum layout included.
    fn draw_like_the_loop<A: Audio>(
        app: &mut App<A>,
        view: &mut View,
        width: u16,
        height: u16,
    ) -> Buffer {
        let buffer = draw_buffer(app, view, width, height);
        app.set_page_size(view.page_size());
        app.set_help_rows(view.help_rows());
        app.set_spectrum_layout(view.spectrum_fits(), view.spectrum_bars());
        app.set_lyrics_layout(view.lyrics_fits());
        buffer
    }

    /// Row of the spectrum strip's top border at a height of `height` rows:
    /// the strip is the bottom of the body, right above the now-playing area
    /// and the status and key lines.
    const fn strip_top(height: u16) -> u16 {
        height - NOW_PLAYING_HEIGHT - 2 - SPECTRUM_HEIGHT
    }

    /// Whether `symbol` is part of a bar.
    fn is_bar(symbol: &str) -> bool {
        symbol == BAR_FULL || BAR_EIGHTHS.contains(&symbol)
    }

    /// Spec *Strip shown on a tall terminal*, *Spectrum strip visible*.
    #[test]
    fn strip_spans_the_full_width_below_both_panes() -> anyhow::Result<()> {
        let (_tmp, app) = with_playlists()?;
        for width in [80, 100] {
            let mut view = View::default();
            let screen = draw(&app, &mut view, width, 30);
            let top = usize::from(strip_top(30));
            assert!(screen[0].contains(" 2 playlists "), "sidebar: {screen:#?}");
            assert!(screen[0].contains(" Library · /m "), "main: {screen:#?}");
            assert!(screen[top - 1].starts_with('└'), "panes end: {screen:#?}");
            assert!(screen[top - 1].ends_with('┘'), "{screen:#?}");
            assert!(
                screen[top].starts_with(&format!("┌{SPECTRUM_TITLE}─")),
                "{screen:#?}"
            );
            assert!(screen[top].ends_with('┐'), "full width: {screen:#?}");
            let bottom = top + usize::from(SPECTRUM_HEIGHT) - 1;
            assert!(screen[bottom].starts_with('└'), "{screen:#?}");
            assert!(screen[bottom].ends_with('┘'), "{screen:#?}");
            // The now-playing area follows.
            assert!(screen[bottom + 1].contains("Now playing"), "{screen:#?}");
            assert!(view.spectrum_fits());
            assert_eq!(view.spectrum_bars(), usize::from(width - 2).div_ceil(2));
        }
        Ok(())
    }

    /// Spec *Too short for the strip*.
    #[test]
    fn short_terminal_has_no_strip() -> anyhow::Result<()> {
        let (_tmp, app) = with_playlists()?;
        for height in [20, SPECTRUM_MIN_HEIGHT - 1] {
            let mut view = View::default();
            let screen = draw(&app, &mut view, 80, height);
            assert!(!screen_contains(&screen, "Spectrum"), "{screen:#?}");
            assert!(!view.spectrum_fits());
            assert_eq!(view.spectrum_bars(), 0);
        }
        let mut view = View::default();
        let screen = draw(&app, &mut view, 80, SPECTRUM_MIN_HEIGHT);
        assert!(screen_contains(&screen, "Spectrum"), "{screen:#?}");
        // The panes keep 13 rows, 11 of them inside the borders.
        assert_eq!(view.page_size(), 10, "11 rows less the table header");
        Ok(())
    }

    /// Spec *Shown in the one-pane layout*.
    #[test]
    fn strip_is_shown_below_the_single_pane() -> anyhow::Result<()> {
        let (_tmp, app) = with_playlists()?;
        let screen = draw(&app, &mut View::default(), 60, 30);
        assert!(screen[0].starts_with("┌ Library · /m "), "{screen:#?}");
        assert!(!screen_contains(&screen, "kasset"), "{screen:#?}");
        let top = usize::from(strip_top(30));
        assert!(screen[top].starts_with(&format!("┌{SPECTRUM_TITLE}")));
        assert!(screen[top].ends_with('┐'), "{screen:#?}");
        Ok(())
    }

    /// Spec *Hide and show*.
    #[test]
    fn v_hides_the_strip_and_the_panes_take_its_rows() -> anyhow::Result<()> {
        let (_tmp, mut app) = with_playlists()?;
        let mut view = View::default();
        draw_like_the_loop(&mut app, &mut view, 80, 30);
        let page = view.page_size();
        press(&mut app, KeyCode::Char('v'));
        let screen = lines(&draw_like_the_loop(&mut app, &mut view, 80, 30));
        assert!(!screen_contains(&screen, "Spectrum"), "{screen:#?}");
        assert_eq!(view.page_size(), page + usize::from(SPECTRUM_HEIGHT));
        assert!(view.spectrum_fits(), "still tall enough");
        assert_eq!(view.spectrum_bars(), 0);
        assert_eq!(app.status(Instant::now()), None, "the change shows itself");
        press(&mut app, KeyCode::Char('v'));
        let screen = lines(&draw_like_the_loop(&mut app, &mut view, 80, 30));
        assert!(screen_contains(&screen, "Spectrum"), "{screen:#?}");
        assert_eq!(view.page_size(), page);
        Ok(())
    }

    /// Spec *Shortening the terminal*.
    #[test]
    fn resizing_hides_and_restores_the_strip() -> anyhow::Result<()> {
        let (_tmp, mut app) = with_playlists()?;
        let mut view = View::default();
        for (height, shown) in [(30, true), (20, false), (30, true)] {
            let screen = lines(&draw_like_the_loop(&mut app, &mut view, 80, height));
            assert_eq!(screen_contains(&screen, "Spectrum"), shown, "{screen:#?}");
            assert!(app.visualizer_on());
        }
        Ok(())
    }

    /// Draws `levels` in a strip `width` columns wide (4 bar rows).
    fn strip(levels: &[f32], width: u16) -> (Buffer, usize) {
        let mut terminal =
            Terminal::new(TestBackend::new(width, SPECTRUM_HEIGHT)).unwrap_or_else(|n| match n {});
        let mut bars = 0;
        terminal
            .draw(|f| bars = render_spectrum(f, levels, f.area()))
            .unwrap_or_else(|never| match never {});
        (terminal.backend().buffer().clone(), bars)
    }

    /// The column at `x` of a strip's bar rows, top to bottom.
    fn bar_column(buffer: &Buffer, x: u16) -> Vec<&str> {
        (1..SPECTRUM_HEIGHT - 1)
            .map(|y| buffer[(x, y)].symbol())
            .collect()
    }

    #[test]
    fn levels_fill_bars_from_the_bottom_in_eighths() {
        let levels = [
            0.0,
            0.5,
            1.0,
            0.3,
            0.05,
            2.0,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ];
        let (buffer, bars) = strip(&levels, 20);
        assert_eq!(bars, 9);
        let columns: Vec<Vec<&str>> = (0..9).map(|i| bar_column(&buffer, 1 + 2 * i)).collect();
        assert_eq!(columns[0], [" ", " ", " ", " "], "empty");
        assert_eq!(columns[1], [" ", " ", "█", "█"], "half: 2 of 4 rows");
        assert_eq!(columns[2], ["█", "█", "█", "█"], "full");
        // 0.3 of 32 eighths is 9.6, so 10: a full cell and 2 eighths.
        assert_eq!(columns[3], [" ", " ", "▂", "█"], "partial top");
        // 0.05 of 32 eighths is 1.6, so 2.
        assert_eq!(columns[4], [" ", " ", " ", "▂"], "partial only");
        assert_eq!(columns[5], ["█", "█", "█", "█"], "clamped to full");
        assert_eq!(columns[6], [" ", " ", " ", " "], "not a number");
        assert_eq!(columns[7], ["█", "█", "█", "█"], "infinity is full");
        assert_eq!(columns[8], [" ", " ", " ", " "], "minus infinity is empty");
        for i in 0..9 {
            assert_eq!(bar_column(&buffer, 2 + 2 * i), [" "; 4], "gap {i}");
        }
        assert_eq!(buffer[(5, 4)].fg, Color::Magenta);
        assert_eq!(buffer[(5, 0)].fg, Color::Reset, "unfocused border");
    }

    #[test]
    fn bar_count_is_half_the_inner_width_rounded_up() {
        for (width, bars) in [(2, 0), (3, 1), (4, 1), (5, 2), (80, 39), (81, 40)] {
            let (buffer, drawn) = strip(&[1.0; 100], width);
            assert_eq!(drawn, bars, "width {width}");
            assert_eq!(drawn, spectrum_bar_count(width.saturating_sub(2)));
            let glyphs = (0..width)
                .filter(|&x| is_bar(buffer[(x, SPECTRUM_HEIGHT - 2)].symbol()))
                .count();
            assert_eq!(glyphs, bars, "width {width}");
            assert_eq!(buffer[(width - 1, 2)].symbol(), "│", "border kept");
        }
    }

    /// A playing tone with its bars up, measured for a strip `width` wide.
    fn tone_playing(width: u16) -> (App<ToneAudio>, View) {
        let mut app = App::new(
            vec![track("A", "Band", "LP")],
            ToneAudio::new(1_000.0, 1.0),
            no_store(),
            "/m".to_owned(),
            None,
        );
        let mut view = View::default();
        draw_like_the_loop(&mut app, &mut view, width, 30);
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let start = Instant::now();
        for i in 0..4 {
            app.tick(start + Duration::from_millis(33) * i);
        }
        assert_eq!(app.spectrum_levels().len(), view.spectrum_bars());
        assert!(app.spectrum_levels().iter().any(|&level| level > 0.5));
        (app, view)
    }

    #[test]
    fn widening_or_narrowing_before_a_tick_draws_no_stray_bars() {
        let (app, mut view) = tone_playing(100);
        let levels = app.spectrum_levels().len();
        let top = strip_top(30);
        for width in [140_u16, 60, 3] {
            let buffer = draw_buffer(&app, &mut view, width, 30);
            let bars = spectrum_bar_count(width - 2);
            assert_eq!(view.spectrum_bars(), bars);
            // Every level that fits and fills at least an eighth is drawn.
            let expected: Vec<u16> = app.spectrum_levels()[..levels.min(bars)]
                .iter()
                .zip((1_u16..).step_by(2))
                .filter(|&(&level, _)| bar_eighths(level, SPECTRUM_HEIGHT - 2) > 0)
                .map(|(_, x)| x)
                .collect();
            let mut drawn = Vec::new();
            for y in 0..30 {
                for x in 0..width {
                    if !is_bar(buffer[(x, y)].symbol()) {
                        continue;
                    }
                    let inside = y > top && y < top + SPECTRUM_HEIGHT - 1;
                    assert!(inside && x >= 1 && x < width - 1, "({x}, {y}) at {width}");
                    assert_eq!(x % 2, 1, "bars sit on odd columns");
                    assert!(
                        usize::from(x / 2) < levels.min(bars),
                        "({x}, {y}) at {width}"
                    );
                    if !drawn.contains(&x) {
                        drawn.push(x);
                    }
                }
            }
            drawn.sort_unstable();
            assert_eq!(drawn, expected, "at {width}");
            if width == 140 {
                assert!(!drawn.is_empty(), "the tone's bars are drawn");
            }
        }
    }

    #[test]
    fn popups_are_centred_over_the_panes_and_the_strip() -> anyhow::Result<()> {
        let (_tmp, mut app) = with_playlists()?;
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        let screen = draw(&app, &mut View::default(), 100, 30);
        assert!(screen_contains(&screen, "Spectrum"), "{screen:#?}");
        let top = screen
            .iter()
            .position(|l| l.contains(" Add 1 track to "))
            .unwrap_or_default();
        let left = screen[top]
            .chars()
            .position(|c| c == '┌')
            .unwrap_or_default();
        let bottom = screen
            .iter()
            .enumerate()
            .skip(top + 1)
            .find(|(_, l)| l.chars().nth(left) == Some('└'))
            .map_or(0, |(y, _)| y);
        let height = bottom + 1 - top;
        let body = 30 - usize::from(NOW_PLAYING_HEIGHT) - 2;
        assert!(top > 0 && height > 2, "{screen:#?}");
        assert_eq!(top, (body - height) / 2, "{screen:#?}");
        Ok(())
    }

    /// Spec *Tab skips the strip*.
    #[test]
    fn tab_never_focuses_the_strip() -> anyhow::Result<()> {
        let (_tmp, mut app) = with_playlists()?;
        let top = strip_top(30);
        let main_x = sidebar_width(80);
        for expected in [Focus::Main, Focus::Sidebar, Focus::Main] {
            assert_eq!(app.focus(), expected);
            let buffer = draw_buffer(&app, &mut View::default(), 80, 30);
            for x in 0..80 {
                assert_eq!(buffer[(x, top)].fg, Color::Reset, "unfocused border");
            }
            // Exactly one pane has the focused border: the expected one.
            let focused = |x: u16| buffer[(x, 1)].fg == Color::Cyan;
            assert_eq!(focused(0), expected == Focus::Sidebar, "{expected:?}");
            assert_eq!(focused(main_x), expected == Focus::Main, "{expected:?}");
            press(&mut app, KeyCode::Tab);
        }
        Ok(())
    }

    // --- Sidebar fixed rows and browse levels (group 4 of
    // add-artist-album-browsing) ---

    /// Spec *Both panes visible*: "Library", "Artists", "Albums", "Queue"
    /// with their counts, then the separator and the playlists.
    #[test]
    fn sidebar_lists_the_fixed_rows_then_the_playlists() -> anyhow::Result<()> {
        let (_tmp, app) = many(3, 2)?;
        let screen = draw(&app, &mut View::default(), 100, 20);
        let sidebar = columns(&screen, 0, usize::from(MAIN_X));
        let expected = [
            ("Library", "3"),
            ("Artists", "1"),
            ("Albums", "1"),
            ("Queue", "0"),
            ("─", "─"),
            ("P00", "0"),
            ("P01", "0"),
        ];
        for (row, (name, count)) in expected.iter().enumerate() {
            let line = sidebar[row + 1].trim_end_matches('│').trim_end();
            assert!(
                line.starts_with(&format!("│   {name}")) || *name == "─",
                "{screen:#?}"
            );
            assert!(line.contains(name) && line.ends_with(count), "{screen:#?}");
        }
        Ok(())
    }

    /// The "Queue" row counts the queued tracks, and has the playing marker
    /// (and "Library" not) while a queued track is current.
    #[test]
    fn sidebar_queue_row_counts_and_marks_the_queue() -> anyhow::Result<()> {
        let (_tmp, mut app) = many(3, 2)?;
        app.playback_mut().enqueue(&[2, 1], false);
        let screen = draw(&app, &mut View::default(), 100, 20);
        let sidebar = columns(&screen, 0, usize::from(MAIN_X));
        let queue_row = sidebar[FIXED_ROWS].trim_end_matches('│').trim_end();
        assert!(queue_row.starts_with("│   Queue"), "{screen:#?}");
        assert!(queue_row.ends_with(" 2"), "{screen:#?}");

        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('n'));
        assert!(app.playback().playing_queued());
        let screen = draw(&app, &mut View::default(), 100, 20);
        let sidebar = columns(&screen, 0, usize::from(MAIN_X));
        let marker = state_marker(PlayState::Playing);
        let queue_row = sidebar[FIXED_ROWS].trim_end_matches('│').trim_end();
        assert!(
            queue_row.starts_with(&format!("│ {marker} Queue")),
            "{screen:#?}"
        );
        assert!(queue_row.ends_with(" 1"), "{screen:#?}");
        assert!(sidebar[1].starts_with("│   Library"), "{screen:#?}");
        Ok(())
    }

    /// The queue view: spec *Empty queue*, a 3-entry queue in order with
    /// duplicates, the title and count, the library's columns, rows without
    /// a playing marker even for the playing track, no match with a filter,
    /// and its key bar (spec *Queue view bar*).
    #[test]
    fn queue_view_lists_the_queue_with_its_bar() {
        let mut app = library(3);
        keys(&mut app, &[KeyCode::Tab]);
        keys(&mut app, &FIRST_PLAYLIST[..FIXED_ROWS]);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.main_view(), MainView::Queue);
        let screen = draw(&app, &mut View::default(), 100, 20);
        assert!(screen_contains(&screen, "Queue is empty"), "{screen:#?}");
        let bar = key_bar(&screen);
        assert!(
            bar.starts_with("Enter play · d remove · J/K move · Esc sidebar"),
            "{bar}"
        );
        assert!(
            !bar.contains("e/E queue") && bar.ends_with("? help"),
            "{bar}"
        );

        app.playback_mut().enqueue(&[2, 1, 2], false);
        app.playback_mut().play(2);
        app.tick(Instant::now());
        let screen = draw(&app, &mut View::default(), 100, 20);
        assert!(screen_contains(&screen, " Queue "), "{screen:#?}");
        assert!(screen_contains(&screen, " 3 tracks "), "{screen:#?}");
        let main = columns(&screen, usize::from(MAIN_X), 100);
        assert!(
            main.iter().any(|l| l.contains("Title")
                && l.contains("Artist")
                && l.contains("Album")
                && l.contains("Time")),
            "{screen:#?}"
        );
        let marker = state_marker(PlayState::Playing);
        let main = columns(&screen, usize::from(MAIN_X), 100);
        let rows: Vec<&String> = main.iter().filter(|l| l.contains("Song 0")).collect();
        assert_eq!(rows.len(), 3, "{screen:#?}");
        assert!(rows[0].contains("Song 02") && rows[1].contains("Song 01"));
        assert!(!rows.iter().any(|l| l.contains(marker)), "{screen:#?}");

        press(&mut app, KeyCode::Char('/'));
        for c in "zzz".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        let screen = draw(&app, &mut View::default(), 100, 20);
        assert!(
            screen_contains(&screen, "No tracks match 'zzz'"),
            "{screen:#?}"
        );
        assert!(screen_contains(&screen, " 0 of 3 "), "{screen:#?}");
    }

    /// Below [`TWO_PANE_MIN_WIDTH`] the focused queue view is drawn alone,
    /// full width, with its rows, title and bar.
    #[test]
    fn queue_view_in_a_narrow_terminal() {
        let mut app = library(3);
        app.playback_mut().enqueue(&[1, 0], false);
        keys(&mut app, &[KeyCode::Tab]);
        keys(&mut app, &FIRST_PLAYLIST[..FIXED_ROWS]);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.main_view(), MainView::Queue);
        let screen = draw(&app, &mut View::default(), 60, 20);
        assert!(screen[0].starts_with("┌ Queue "), "{screen:#?}");
        assert!(!screen_contains(&screen, "Library"), "{screen:#?}");
        let rows: Vec<&String> = screen.iter().filter(|l| l.contains("Song 0")).collect();
        assert_eq!(rows.len(), 2, "{screen:#?}");
        assert!(rows[0].contains("Song 01") && rows[1].contains("Song 00"));
        assert!(
            key_bar(&screen).starts_with("Enter play · d remove"),
            "{screen:#?}"
        );

        // Emptied while shown: says so.
        app.playback_mut().clear_queued();
        app.tick(Instant::now());
        let screen = draw(&app, &mut View::default(), 60, 20);
        assert!(screen_contains(&screen, "Queue is empty"), "{screen:#?}");
    }

    /// The clear prompt: its popup names the queued tracks (singular and
    /// plural), centred over the body, and its own bar.
    #[test]
    fn clear_queue_prompt_has_its_popup_and_bar() {
        let mut app = library(3);
        app.playback_mut().enqueue(&[0], false);
        keys(&mut app, &[KeyCode::Tab]);
        keys(&mut app, &FIRST_PLAYLIST[..FIXED_ROWS]);
        app.handle_key(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::SHIFT));
        assert_eq!(*app.mode(), Mode::ConfirmClearQueue);
        let screen = draw(&app, &mut View::default(), 100, 20);
        assert_eq!(
            key_bar(&screen).trim_end(),
            "y clear · any other key cancel"
        );
        assert!(screen_contains(&screen, " Clear queue "), "{screen:#?}");
        assert!(
            screen_contains(&screen, "Clear the queue (1 track)? y/n"),
            "{screen:#?}"
        );
        assert!(
            screen_contains(&screen, "y empties the queue, other keys cancel"),
            "{screen:#?}"
        );

        // The count is the queue's at draw time.
        app.playback_mut().enqueue(&[1, 2], true);
        let screen = draw(&app, &mut View::default(), 100, 20);
        assert!(
            screen_contains(&screen, "Clear the queue (3 tracks)? y/n"),
            "{screen:#?}"
        );

        // Cut to fit, never panics, at any size.
        for (w, h) in [(0, 0), (1, 1), (2, 3), (10, 5), (30, 8), (80, 4), (5, 80)] {
            draw(&app, &mut View::default(), w, h);
        }
        let screen = draw(&app, &mut View::default(), 30, 20);
        let question = screen.iter().find(|l| l.contains("Clear the queue"));
        assert!(
            question.is_some_and(|l| l.contains('…') && !l.contains("y/n")),
            "{screen:#?}"
        );

        // Answering closes it.
        press(&mut app, KeyCode::Char('n'));
        let screen = draw(&app, &mut View::default(), 100, 20);
        assert!(
            !screen_contains(&screen, "Clear the queue ("),
            "{screen:#?}"
        );
        assert_eq!(app.playback().up_next().len(), 3);
    }

    /// Short sidebars: the fixed rows are cut from the bottom, the separator
    /// needs a playlist row below it, and the page counts playlist rows only.
    #[test]
    fn short_sidebar_keeps_the_fixed_rows_first() -> anyhow::Result<()> {
        let (_tmp, mut app) = many(3, 5)?;
        press(&mut app, KeyCode::Tab);
        // Inner height = terminal height - 3 (now playing) - 2 (status, keys)
        // - 2 (borders); narrow, so the sidebar is drawn alone.
        let cases: [(u16, &[&str], usize); 5] = [
            (2, &["Library", "Artists"], 0),
            (3, &["Library", "Artists", "Albums"], 0),
            (4, &["Library", "Artists", "Albums", "Queue"], 0),
            (5, &["Library", "Artists", "Albums", "Queue", "P00"], 1),
            (6, &["Library", "Artists", "Albums", "Queue", "─", "P00"], 1),
        ];
        for (inner, rows, page) in cases {
            let mut view = View::default();
            let screen = draw(&app, &mut view, 60, inner + 7);
            for (y, name) in rows.iter().enumerate() {
                assert!(screen[y + 1].contains(name), "{inner}: {screen:#?}");
            }
            let body = &screen[1..=usize::from(inner)];
            let separators = body.iter().filter(|l| l.contains("──")).count();
            assert_eq!(
                separators,
                usize::from(rows.contains(&"─")),
                "{inner}: {screen:#?}"
            );
            assert!(
                screen[usize::from(inner) + 1].starts_with('└'),
                "{screen:#?}"
            );
            assert_eq!(view.page_size, page, "{inner}");
        }
        Ok(())
    }

    /// Scrolling the playlists keeps every fixed row on top.
    #[test]
    fn scrolled_sidebar_keeps_the_fixed_rows_on_top() -> anyhow::Result<()> {
        let (_tmp, mut app) = many(3, 30)?;
        keys(&mut app, &[KeyCode::Tab, KeyCode::End]);
        let screen = draw(&app, &mut View::default(), 100, 20);
        for (y, name) in FIXED_ROW_NAMES.iter().enumerate() {
            assert!(screen[y + 1].contains(name), "{screen:#?}");
        }
        assert!(screen[FIXED_ROWS + 1].contains("──"), "{screen:#?}");
        assert!(screen_contains(&screen, "P29"), "{screen:#?}");
        assert!(!screen_contains(&screen, "P00"), "{screen:#?}");
        Ok(())
    }

    /// The key bar on each browse level: specs *Artist list bar* and *Album
    /// tracks bar*, and `Esc back` on an artist's albums.
    #[test]
    fn browse_levels_have_their_own_key_bars() {
        let mut app = library(3);
        keys(&mut app, &[KeyCode::Tab]);
        keys(&mut app, &FIRST_PLAYLIST[..2]);
        press(&mut app, KeyCode::Enter);
        let screen = draw(&app, &mut View::default(), 100, 20);
        let bar = key_bar(&screen);
        assert!(bar.starts_with("Enter open · Esc sidebar"), "{bar}");
        assert!(bar.contains("a add") && !bar.contains("Esc back"), "{bar}");
        assert!(bar.ends_with("? help"), "{bar}");

        press(&mut app, KeyCode::Enter);
        let screen = draw(&app, &mut View::default(), 100, 20);
        let bar = key_bar(&screen);
        assert!(bar.starts_with("Enter open"), "{bar}");
        assert!(bar.contains("Esc back") && bar.contains("a add"), "{bar}");
        assert!(!bar.contains("Esc sidebar"), "{bar}");

        press(&mut app, KeyCode::Enter);
        let screen = draw(&app, &mut View::default(), 100, 20);
        let bar = key_bar(&screen);
        assert!(bar.starts_with("Enter play · a/A add"), "{bar}");
        assert!(bar.contains("Esc back"), "{bar}");
        assert!(bar.ends_with("? help"), "{bar}");
    }

    /// Air with "Moon Safari" (3 tracks) and "Talkie Walkie" (2), Björk
    /// with "Debut" (2), and "Hits" by "Various Artists" with guests "A"
    /// and "B"; the lyrics pane is off.
    fn browse_music() -> App<SilentAudio> {
        let specs = [
            ("Venus", "Air", "Talkie Walkie", 2, None),
            ("Cherry Blossom Girl", "Air", "Talkie Walkie", 1, None),
            ("La Femme d'Argent", "Air", "Moon Safari", 1, None),
            ("Sexy Boy", "Air", "Moon Safari", 2, None),
            ("Kelly Watch the Stars", "Air", "Moon Safari", 3, None),
            ("Human Behaviour", "Björk", "Debut", 1, None),
            ("Crying", "Björk", "Debut", 2, None),
            ("First Hit", "A", "Hits", 1, Some("Various Artists")),
            ("Second Hit", "B", "Hits", 2, Some("Various Artists")),
        ];
        let mut tracks: Vec<_> = specs
            .iter()
            .map(|&(title, artist, album, number, album_artist)| {
                let mut t = track(title, artist, album);
                t.number = Some(number);
                t.album_artist = album_artist.map(str::to_owned);
                t
            })
            .collect();
        crate::library::sort_tracks(&mut tracks);
        let mut app = App::new(
            tracks,
            SilentAudio::default(),
            no_store(),
            "/music".to_owned(),
            None,
        );
        app.set_lyrics_on(false);
        app
    }

    /// From anywhere in Normal mode: focuses the sidebar and presses
    /// `Enter` on fixed row `value` (1 = "Artists", 2 = "Albums").
    fn open_fixed_row(app: &mut App<SilentAudio>, value: usize) {
        if app.focus() == Focus::Main {
            press(app, KeyCode::Tab);
        }
        press(app, KeyCode::Home);
        for _ in 0..value {
            press(app, KeyCode::Down);
        }
        press(app, KeyCode::Enter);
    }

    /// Selects the row of the focused list that shows `name` (in a narrow
    /// terminal, so only the main pane is drawn) and presses `Enter`.
    fn enter_on(app: &mut App<SilentAudio>, name: &str) {
        press(app, KeyCode::Home);
        for _ in 0..app.visible().len() {
            let buffer = draw_buffer(app, &mut View::default(), NARROW, 30);
            let screen = lines(&buffer);
            // The selection is reversed, from the marker column on.
            let selected = (0..buffer.area.height)
                .find(|&y| buffer[(2, y)].modifier.contains(Modifier::REVERSED));
            if selected.is_some_and(|y| screen[usize::from(y)].contains(name)) {
                press(app, KeyCode::Enter);
                return;
            }
            press(app, KeyCode::Down);
        }
        panic!("no row shows {name}");
    }

    /// A terminal narrower than [`TWO_PANE_MIN_WIDTH`]: only the focused
    /// pane is drawn.
    const NARROW: u16 = 60;

    /// The words of the main pane's row showing `name` (borders, marker
    /// and padding dropped), in a narrow terminal.
    fn row_words(screen: &[String], name: &str) -> Vec<String> {
        row_with(&screen[1..], name).map_or_else(Vec::new, |row| {
            row.split_whitespace()
                .filter(|w| !["│", "▶", "‖", "■"].contains(w))
                .map(str::to_owned)
                .collect()
        })
    }

    /// Whether any row of the main pane has a playing marker (in a narrow
    /// terminal; the now-playing area below has its own).
    fn any_marked(screen: &[String]) -> bool {
        screen
            .iter()
            .take_while(|l| !l.starts_with(" Now playing"))
            .any(|l| l.starts_with("│ ▶ ") || l.starts_with("│ ‖ "))
    }

    /// Whether the main pane's row showing `name` has the playing marker.
    /// The row must start with `name` (after the border, marker and
    /// padding), so a name inside another row's text never matches.
    fn marked(screen: &[String], name: &str) -> bool {
        let prefix = format!("│ ▶ {name}");
        screen
            .iter()
            .take_while(|l| !l.starts_with(" Now playing"))
            .any(|l| l.starts_with(&prefix))
    }

    /// The artist table: header, name, album and track counts, in order.
    #[test]
    fn artist_table_lists_albums_and_tracks() {
        let mut app = browse_music();
        open_fixed_row(&mut app, 1);
        let screen = draw(&app, &mut View::default(), NARROW, 20);
        assert!(screen[0].starts_with("┌ Artists "), "{screen:#?}");
        assert!(screen[0].ends_with(" 3 artists ┐"), "{screen:#?}");
        assert_eq!(row_words(&screen, "Artist"), ["Artist", "Albums", "Tracks"]);
        assert_eq!(row_words(&screen, "Air"), ["Air", "2", "5"]);
        assert_eq!(row_words(&screen, "Björk"), ["Björk", "1", "2"]);
        assert_eq!(
            row_words(&screen, "Various"),
            ["Various", "Artists", "1", "2"]
        );
        let air = screen.iter().position(|l| l.contains("Air"));
        let various = screen.iter().position(|l| l.contains("Various"));
        assert!(air < various, "{screen:#?}");
        // The counts end at the right border's padding.
        assert!(
            row_with(&screen, "Air").is_some_and(|r| r.ends_with("5 │")),
            "{screen:#?}"
        );
        assert!(!any_marked(&screen), "nothing is playing");
    }

    /// An artist's albums: "All tracks" (counting all the artist's tracks)
    /// first, then the albums by title, the title naming the artist.
    #[test]
    fn artist_albums_start_with_all_tracks() {
        let mut app = browse_music();
        open_fixed_row(&mut app, 1);
        enter_on(&mut app, "Air");
        let screen = draw(&app, &mut View::default(), NARROW, 20);
        assert!(screen[0].starts_with("┌ Air "), "{screen:#?}");
        assert!(screen[0].ends_with(" 2 albums ┐"), "{screen:#?}");
        assert_eq!(row_words(&screen[..2], "Album"), ["Album", "Tracks"]);
        assert_eq!(row_words(&screen, "All tracks"), ["All", "tracks", "5"]);
        assert_eq!(row_words(&screen, "Moon"), ["Moon", "Safari", "3"]);
        assert_eq!(row_words(&screen, "Talkie"), ["Talkie", "Walkie", "2"]);
        let order: Vec<_> = ["All tracks", "Moon Safari", "Talkie Walkie"]
            .iter()
            .map(|n| screen.iter().position(|l| l.contains(n)))
            .collect();
        assert!(order.is_sorted() && order[0].is_some(), "{screen:#?}");

        // A filter keeps "All tracks" and counts only the albums.
        press(&mut app, KeyCode::Char('/'));
        for c in "talk".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        let screen = draw(&app, &mut View::default(), NARROW, 20);
        assert!(screen_contains(&screen, "All tracks"), "{screen:#?}");
        assert!(!screen_contains(&screen, "Moon"), "{screen:#?}");
        assert!(screen[0].contains(" 1 of 2 match 'talk' "), "{screen:#?}");
    }

    /// The album table: title, album artist and tracks, by artist then
    /// title; a compilation is one album under its album artist.
    #[test]
    fn album_table_lists_artist_and_tracks() {
        let mut app = browse_music();
        open_fixed_row(&mut app, 2);
        let screen = draw(&app, &mut View::default(), NARROW, 20);
        assert!(screen[0].starts_with("┌ Albums "), "{screen:#?}");
        assert!(screen[0].ends_with(" 4 albums ┐"), "{screen:#?}");
        assert_eq!(row_words(&screen, "Album"), ["Album", "Artist", "Tracks"]);
        assert_eq!(row_words(&screen, "Moon"), ["Moon", "Safari", "Air", "3"]);
        assert_eq!(row_words(&screen, "Debut"), ["Debut", "Björk", "2"]);
        assert_eq!(
            row_words(&screen, "Hits"),
            ["Hits", "Various", "Artists", "2"]
        );
        let order: Vec<_> = ["Moon Safari", "Talkie Walkie", "Debut", "Hits"]
            .iter()
            .map(|n| screen.iter().position(|l| l.contains(n)))
            .collect();
        assert!(order.is_sorted() && order[0].is_some(), "{screen:#?}");
    }

    /// Spec *Browsing shown in the main pane*: an album's tracks with the
    /// library's columns, in album order, titled with the album and its
    /// artist; "All tracks" is titled with the artist.
    #[test]
    fn browsed_tracks_have_the_library_columns() {
        let mut app = browse_music();
        open_fixed_row(&mut app, 1);
        enter_on(&mut app, "Air");
        enter_on(&mut app, "Moon Safari");
        let screen = draw(&app, &mut View::default(), NARROW, 20);
        assert!(screen[0].starts_with("┌ Moon Safari · Air "), "{screen:#?}");
        assert!(screen[0].ends_with(" 3 tracks ┐"), "{screen:#?}");
        assert_eq!(
            row_words(&screen, "Title"),
            ["Title", "Artist", "Album", "Time"]
        );
        let order: Vec<_> = ["La Femme", "Sexy Boy", "Kelly Watch"]
            .iter()
            .map(|n| screen.iter().position(|l| l.contains(n)))
            .collect();
        assert!(order.is_sorted() && order[0].is_some(), "{screen:#?}");
        assert!(
            row_with(&screen, "Sexy Boy").is_some_and(|r| r.contains("Air")
                && r.contains("Moon Safari")
                && r.contains("3:20")),
            "{screen:#?}"
        );

        press(&mut app, KeyCode::Backspace);
        enter_on(&mut app, "All tracks");
        let screen = draw(&app, &mut View::default(), NARROW, 20);
        assert!(screen[0].starts_with("┌ Air · All tracks "), "{screen:#?}");
        assert!(screen[0].ends_with(" 5 tracks ┐"), "{screen:#?}");
        let moon = screen.iter().position(|l| l.contains("Kelly Watch"));
        let talkie = screen.iter().position(|l| l.contains("Cherry Blossom"));
        assert!(moon.is_some() && moon < talkie, "{screen:#?}");
    }

    /// Specs *Album played through Artists* and *Album playing*.
    #[test]
    fn album_played_through_artists_is_marked_on_every_level() {
        let mut app = browse_music();
        open_fixed_row(&mut app, 1);
        enter_on(&mut app, "Air");
        enter_on(&mut app, "Moon Safari");
        enter_on(&mut app, "Sexy Boy");
        let screen = draw(&app, &mut View::default(), NARROW, 20);
        assert!(marked(&screen, "Sexy Boy"), "{screen:#?}");
        assert!(!marked(&screen, "La Femme"), "{screen:#?}");

        // The playing row is in the playing style.
        let buffer = draw_buffer(&app, &mut View::default(), NARROW, 20);
        let y = screen
            .iter()
            .position(|l| l.contains("Sexy Boy"))
            .unwrap_or(0);
        let y = u16::try_from(y).unwrap_or(0);
        assert_eq!(buffer[(6, y)].fg, Color::Green);

        press(&mut app, KeyCode::Backspace);
        let screen = draw(&app, &mut View::default(), NARROW, 20);
        assert!(marked(&screen, "Moon Safari"), "{screen:#?}");
        assert!(!marked(&screen, "All tracks"), "{screen:#?}");
        assert!(!marked(&screen, "Talkie"), "{screen:#?}");

        press(&mut app, KeyCode::Backspace);
        let screen = draw(&app, &mut View::default(), NARROW, 20);
        assert!(marked(&screen, "Air"), "{screen:#?}");
        assert!(!marked(&screen, "Björk"), "{screen:#?}");

        let wide = draw(&app, &mut View::default(), 100, 20);
        assert!(
            sidebar_row(&wide, "Albums").is_some_and(|r| r.contains('▶')),
            "{wide:#?}"
        );
        assert!(
            sidebar_row(&wide, "Artists").is_some_and(|r| !r.contains('▶')),
            "{wide:#?}"
        );

        // Air's "All tracks" is not the play order, so nothing is marked.
        enter_on(&mut app, "Air");
        enter_on(&mut app, "All tracks");
        let screen = draw(&app, &mut View::default(), NARROW, 20);
        assert!(!any_marked(&screen), "{screen:#?}");

        open_fixed_row(&mut app, 2);
        let screen = draw(&app, &mut View::default(), NARROW, 20);
        assert!(marked(&screen, "Moon Safari"), "{screen:#?}");
        assert!(!marked(&screen, "Talkie"), "{screen:#?}");

        // A paused track keeps its rows marked, with the paused marker.
        press(&mut app, KeyCode::Char(' '));
        let screen = draw(&app, &mut View::default(), NARROW, 20);
        assert!(
            screen.iter().any(|r| r.starts_with("│ ‖ Moon Safari")),
            "{screen:#?}"
        );
    }

    /// An artist's "All tracks" as the play order marks the artist, "All
    /// tracks" and the track, but no album.
    #[test]
    fn artist_play_order_marks_the_artist_and_all_tracks() {
        let mut app = browse_music();
        open_fixed_row(&mut app, 1);
        enter_on(&mut app, "Air");
        enter_on(&mut app, "All tracks");
        enter_on(&mut app, "Venus");
        let screen = draw(&app, &mut View::default(), NARROW, 20);
        assert!(marked(&screen, "Venus"), "{screen:#?}");

        press(&mut app, KeyCode::Backspace);
        let screen = draw(&app, &mut View::default(), NARROW, 20);
        assert!(marked(&screen, "All tracks"), "{screen:#?}");
        assert!(!marked(&screen, "Moon Safari"), "{screen:#?}");
        assert!(!marked(&screen, "Talkie"), "{screen:#?}");

        // The album holding the track is not the play order.
        enter_on(&mut app, "Talkie Walkie");
        let screen = draw(&app, &mut View::default(), NARROW, 20);
        assert!(!any_marked(&screen), "{screen:#?}");

        open_fixed_row(&mut app, 2);
        let screen = draw(&app, &mut View::default(), NARROW, 20);
        assert!(!any_marked(&screen), "{screen:#?}");

        open_fixed_row(&mut app, 1);
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Backspace);
        let screen = draw(&app, &mut View::default(), NARROW, 20);
        assert!(marked(&screen, "Air"), "{screen:#?}");
        let wide = draw(&app, &mut View::default(), 100, 20);
        assert!(
            sidebar_row(&wide, "Artists").is_some_and(|r| r.contains('▶')),
            "{wide:#?}"
        );
    }

    /// Spec *Different play order*: with the library as the play order, the
    /// album holding the playing track marks nothing.
    #[test]
    fn library_play_order_marks_no_browse_row() {
        let mut app = browse_music();
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        open_fixed_row(&mut app, 2);
        let screen = draw(&app, &mut View::default(), NARROW, 20);
        assert!(!any_marked(&screen), "{screen:#?}");
        enter_on(&mut app, "Moon Safari");
        let screen = draw(&app, &mut View::default(), NARROW, 20);
        assert!(!any_marked(&screen), "{screen:#?}");
    }

    /// Spec *Empty library* and a filter matching nothing.
    #[test]
    fn empty_and_unmatched_levels_say_so() {
        let mut empty = library(0);
        open_fixed_row(&mut empty, 1);
        let screen = draw(&empty, &mut View::default(), 100, 20);
        assert!(screen_contains(&screen, "No artists"), "{screen:#?}");
        assert!(
            !screen_contains(&screen, "Tracks"),
            "no header: {screen:#?}"
        );
        open_fixed_row(&mut empty, 2);
        let screen = draw(&empty, &mut View::default(), 100, 20);
        assert!(screen_contains(&screen, "No albums"), "{screen:#?}");

        let mut app = browse_music();
        for (value, noun) in [(1, "artists"), (2, "albums")] {
            open_fixed_row(&mut app, value);
            press(&mut app, KeyCode::Char('/'));
            for c in "zzz".chars() {
                press(&mut app, KeyCode::Char(c));
            }
            let screen = draw(&app, &mut View::default(), NARROW, 20);
            let message = format!("No {noun} match 'zzz'");
            assert!(screen_contains(&screen, &message), "{screen:#?}");
            press(&mut app, KeyCode::Esc);
        }
        enter_on(&mut app, "Debut");
        press(&mut app, KeyCode::Char('/'));
        for c in "zzz".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        let screen = draw(&app, &mut View::default(), NARROW, 20);
        assert!(
            screen_contains(&screen, "No tracks match 'zzz'"),
            "{screen:#?}"
        );
    }

    /// Long and wide names are cut inside the pane, in the title and in the
    /// rows, at every width; the counts keep their place.
    #[test]
    fn long_names_stay_inside_the_pane() {
        let long = "Ω".repeat(200);
        let wide = "音".repeat(100);
        let mut tracks = vec![track("Song", &wide, &long)];
        tracks[0].album_artist = Some(wide.clone());
        let mut app = App::new(
            tracks,
            SilentAudio::default(),
            no_store(),
            "/music".to_owned(),
            None,
        );
        app.set_lyrics_on(false);
        let levels: [&[KeyCode]; 4] = [
            &[],
            &[KeyCode::Enter],
            &[KeyCode::Down, KeyCode::Enter],
            &[KeyCode::Backspace, KeyCode::Home, KeyCode::Enter],
        ];
        open_fixed_row(&mut app, 1);
        for step in levels {
            keys(&mut app, step);
            for width in [NARROW, 40, 100] {
                let screen = draw(&app, &mut View::default(), width, 12);
                // The body is 12 - 5 rows: now playing, status and keys
                // below it; at 100 columns the main pane is on the right.
                let pane = columns(
                    &screen,
                    if width >= TWO_PANE_MIN_WIDTH {
                        usize::from(sidebar_width(width))
                    } else {
                        0
                    },
                    usize::from(width),
                );
                for line in &pane[..7] {
                    assert!(line.starts_with(['┌', '│', '└']), "{screen:#?}");
                    assert!(line.ends_with(['┐', '│', '┘']), "{screen:#?}");
                }
            }
        }
        // The album table keeps the track count beside the cut names (cut
        // by the table, like the library's columns).
        open_fixed_row(&mut app, 2);
        let screen = draw(&app, &mut View::default(), NARROW, 12);
        assert!(
            row_with(&screen[2..], "ΩΩ").is_some_and(|r| r.ends_with(" 1 │")),
            "{screen:#?}"
        );
    }

    /// Every level at tiny and odd sizes, with something playing.
    #[test]
    fn browse_levels_at_tiny_sizes_do_not_panic() {
        let mut app = browse_music();
        open_fixed_row(&mut app, 1);
        enter_on(&mut app, "Air");
        enter_on(&mut app, "Moon Safari");
        enter_on(&mut app, "Sexy Boy");
        let steps: [&[KeyCode]; 4] = [
            &[],
            &[KeyCode::Backspace],
            &[KeyCode::Backspace],
            &[KeyCode::Tab, KeyCode::Down, KeyCode::Enter],
        ];
        for step in steps {
            keys(&mut app, step);
            let mut view = View::default();
            for (w, h) in [(1, 1), (2, 3), (5, 8), (10, 9), (13, 10), (69, 11), (70, 6)] {
                draw(&app, &mut view, w, h);
            }
        }
    }
}

/// The lyrics pane: layout, rows, scrolling and messages.
#[cfg(test)]
mod lyrics_tests {
    use super::*;
    use crate::library::Track;
    use crate::lyrics::Line as LyricLine;
    use crate::test_support::{SilentAudio, no_store, wav_bytes};
    use anyhow::{Result, bail};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::style::{Color, Modifier};

    /// Height without the spectrum strip (shorter than
    /// [`SPECTRUM_MIN_HEIGHT`]): the panes' insides are 13 rows.
    const SHORT: u16 = 20;

    /// Rows inside the panes at [`SHORT`] rows: the body less two borders.
    const INNER_ROWS: usize = (SHORT - NOW_PLAYING_HEIGHT - 2 - 2) as usize;

    /// Tracks 200 s long named after `entries`, each with the given
    /// sidecar text if any, in a temporary folder, in library order. The
    /// lyrics pane is on (the default).
    fn folder(entries: &[(&str, Option<&str>)]) -> Result<(tempfile::TempDir, App<SilentAudio>)> {
        let tmp = tempfile::tempdir()?;
        let mut tracks = Vec::new();
        for (name, lyrics) in entries {
            let path = tmp.path().join(format!("{name}.wav"));
            std::fs::write(&path, wav_bytes())?;
            if let Some(text) = lyrics {
                std::fs::write(path.with_extension("lrc"), text)?;
            }
            tracks.push(Track {
                path,
                title: (*name).to_owned(),
                artist: "Band".to_owned(),
                album: "LP".to_owned(),
                number: None,
                album_artist: None,
                disc: None,
                duration: Some(Duration::from_secs(200)),
            });
        }
        let app = App::new(
            tracks,
            SilentAudio::default(),
            no_store(),
            "/m".to_owned(),
            None,
        );
        Ok((tmp, app))
    }

    fn press(app: &mut App<SilentAudio>, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    /// One turn of the event loop at `width`×`height`: tick, draw, feed
    /// the layout back. Returns the drawn screen.
    fn frame(app: &mut App<SilentAudio>, view: &mut View, width: u16, height: u16) -> Buffer {
        let now = Instant::now();
        app.tick(now);
        let mut terminal =
            Terminal::new(TestBackend::new(width, height)).unwrap_or_else(|never| match never {});
        terminal
            .draw(|f| render(f, app, view, now))
            .unwrap_or_else(|never| match never {});
        app.set_page_size(view.page_size());
        app.set_help_rows(view.help_rows());
        app.set_spectrum_layout(view.spectrum_fits(), view.spectrum_bars());
        app.set_lyrics_layout(view.lyrics_fits());
        terminal.backend().buffer().clone()
    }

    /// Two turns of the loop, so lyrics shown by the first are loaded and
    /// drawn by the second.
    fn settle(app: &mut App<SilentAudio>, view: &mut View, width: u16, height: u16) -> Buffer {
        frame(app, view, width, height);
        frame(app, view, width, height)
    }

    fn line(buffer: &Buffer, y: u16) -> String {
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect()
    }

    fn lines(buffer: &Buffer) -> Vec<String> {
        (0..buffer.area.height).map(|y| line(buffer, y)).collect()
    }

    /// The column where the lyrics pane starts on a `width`-column screen.
    fn pane_x(width: u16) -> u16 {
        width - lyrics_width(width)
    }

    /// The lyrics pane's inside, row by row, on a `width`-column screen:
    /// inside the borders and the one-column padding.
    fn pane_rows(buffer: &Buffer, width: u16) -> Vec<String> {
        let from = pane_x(width) + 2;
        let to = width - 2;
        (1..=INNER_ROWS)
            .filter_map(|y| u16::try_from(y).ok())
            .map(|y| {
                (from..to)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    /// The style of the first character of the inside's row `row`.
    fn row_style(buffer: &Buffer, width: u16, row: usize) -> Style {
        let y = u16::try_from(row + 1).unwrap_or(u16::MAX);
        buffer[(pane_x(width) + 2, y)].style()
    }

    fn is_playing(style: Style) -> bool {
        style.fg == theme::PLAYING.fg && style.add_modifier.contains(Modifier::BOLD)
    }

    /// Whether `style` is the empty-list message style.
    fn is_message(style: Style) -> bool {
        style.add_modifier.contains(theme::MESSAGE.add_modifier)
    }

    /// `count` timed lines "Line 00", "Line 01", … one a second from 0:01.
    fn numbered(count: usize) -> String {
        (0..count)
            .map(|i| format!("[00:{:02}.00]Line {i:02}", i + 1))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Spec *Pane shown on a wide terminal*, *Lyrics pane visible*.
    #[test]
    fn wide_terminal_shows_three_panes_left_to_right() -> Result<()> {
        let (_tmp, mut app) = folder(&[("song", None)])?;
        let mut view = View::default();
        let screen = lines(&settle(&mut app, &mut view, 120, 30));
        let top = &screen[0];
        let (Some(side), Some(main), Some(lyrics)) = (
            top.find("kasset"),
            top.find("Library"),
            top.find(LYRICS_TITLE.trim()),
        ) else {
            bail!("missing a pane: {screen:#?}");
        };
        assert!(side < main && main < lyrics, "{screen:#?}");
        assert!(
            top.contains(&format!("┌{LYRICS_TITLE}─")),
            "titled: {screen:#?}"
        );
        assert!(view.lyrics_fits());
        assert!(app.lyrics_shown());
        Ok(())
    }

    /// Spec *Too narrow*.
    #[test]
    fn narrow_terminal_has_two_panes() -> Result<()> {
        let (_tmp, mut app) = folder(&[("song", None)])?;
        let mut view = View::default();
        for width in [90, LYRICS_MIN_WIDTH - 1] {
            let screen = lines(&settle(&mut app, &mut view, width, 30));
            assert!(!screen[0].contains("Lyrics"), "{screen:#?}");
            assert!(screen[0].contains("Library"), "{screen:#?}");
            assert!(!view.lyrics_fits());
            assert!(app.lyrics_on(), "still on");
        }
        let screen = lines(&settle(&mut app, &mut view, LYRICS_MIN_WIDTH, 30));
        assert!(screen[0].contains(LYRICS_TITLE), "{screen:#?}");
        Ok(())
    }

    /// Spec *Width bounds*: a third of the width, 28 to 48 columns.
    #[test]
    fn pane_width_is_a_third_within_bounds() -> Result<()> {
        assert_eq!(lyrics_width(100), 33);
        assert_eq!(lyrics_width(120), 40);
        assert_eq!(lyrics_width(200), 48);
        assert_eq!(lyrics_width(70), 28);
        let (_tmp, mut app) = folder(&[("song", None)])?;
        let mut view = View::default();
        for (width, pane) in [(100_u16, 33_u16), (200, 48)] {
            let screen = settle(&mut app, &mut view, width, 30);
            let x = width - pane;
            assert_eq!(screen[(x, 0)].symbol(), "┌", "{:#?}", lines(&screen));
            assert_eq!(screen[(x - 1, 0)].symbol(), "┐", "main pane ends");
            assert_eq!(screen[(width - 1, 0)].symbol(), "┐");
            // The sidebar keeps its two-pane width.
            let side = sidebar_width(width);
            assert_eq!(screen[(side - 1, 0)].symbol(), "┐", "{:#?}", lines(&screen));
            assert_eq!(screen[(side, 0)].symbol(), "┌");
        }
        // At 100 columns the main pane keeps 42.
        assert_eq!(100 - sidebar_width(100) - lyrics_width(100), 42);
        Ok(())
    }

    /// Spec *Narrowing the terminal*.
    #[test]
    fn resizing_hides_and_shows_the_pane() -> Result<()> {
        let (_tmp, mut app) = folder(&[("song", Some("[00:01.00]Hello\n"))])?;
        press(&mut app, KeyCode::Enter);
        let mut view = View::default();
        let wide = lines(&settle(&mut app, &mut view, 120, SHORT));
        assert!(wide.iter().any(|l| l.contains("Hello")), "{wide:#?}");
        let narrow = lines(&frame(&mut app, &mut view, 90, SHORT));
        assert!(!narrow.iter().any(|l| l.contains("Lyrics")), "{narrow:#?}");
        assert!(app.lyrics_on());
        assert!(!app.lyrics_shown());
        let wide = lines(&settle(&mut app, &mut view, 120, SHORT));
        assert!(wide[0].contains(LYRICS_TITLE), "{wide:#?}");
        assert!(wide.iter().any(|l| l.contains("Hello")), "{wide:#?}");
        Ok(())
    }

    /// Spec *Spectrum below*.
    #[test]
    fn pane_ends_above_the_full_width_strip() -> Result<()> {
        let (_tmp, mut app) = folder(&[("song", None)])?;
        assert!(app.visualizer_on());
        let mut view = View::default();
        let screen = settle(&mut app, &mut view, 120, 30);
        let strip = 30 - NOW_PLAYING_HEIGHT - 2 - SPECTRUM_HEIGHT;
        let x = pane_x(120);
        assert_eq!(screen[(x, 0)].symbol(), "┌");
        assert_eq!(
            screen[(x, strip - 1)].symbol(),
            "└",
            "{:#?}",
            lines(&screen)
        );
        assert_eq!(screen[(119, strip - 1)].symbol(), "┘");
        let top = line(&screen, strip);
        assert!(top.starts_with(&format!("┌{SPECTRUM_TITLE}")), "{top}");
        assert!(top.ends_with('┐'), "full width: {top}");
        Ok(())
    }

    /// Spec *Tab skips the pane*: focus only moves between the sidebar
    /// and the main pane, and the lyrics border is never focused.
    #[test]
    fn tab_never_focuses_the_pane() -> Result<()> {
        let (_tmp, mut app) = folder(&[("song", None)])?;
        let mut view = View::default();
        let unfocused = theme::border(false).0;
        let focused = theme::border(true).0;
        assert_eq!(app.focus(), Focus::Main);
        for expected in [Focus::Sidebar, Focus::Main, Focus::Sidebar] {
            let screen = settle(&mut app, &mut view, 120, 30);
            let corner = screen[(pane_x(120), 0)].style();
            assert_eq!(corner.fg, unfocused.fg.or(Some(Color::Reset)));
            assert_ne!(corner.fg, focused.fg);
            press(&mut app, KeyCode::Tab);
            assert_eq!(app.focus(), expected);
        }
        Ok(())
    }

    /// Spec *Hide and show*: `l` hides the pane and the main pane widens.
    #[test]
    fn l_hides_the_pane_and_the_main_pane_widens() -> Result<()> {
        let (_tmp, mut app) = folder(&[("song", None)])?;
        let mut view = View::default();
        let screen = settle(&mut app, &mut view, 120, 30);
        assert_eq!(screen[(pane_x(120) - 1, 0)].symbol(), "┐");
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(app.status(Instant::now()), None, "the change shows itself");
        let screen = settle(&mut app, &mut view, 120, 30);
        let top = line(&screen, 0);
        assert!(!top.contains("Lyrics"), "{top}");
        assert_eq!(screen[(pane_x(120) - 1, 0)].symbol(), "─", "{top}");
        assert_eq!(screen[(119, 0)].symbol(), "┐", "main pane to the edge");
        press(&mut app, KeyCode::Char('l'));
        let screen = settle(&mut app, &mut view, 120, 30);
        assert!(line(&screen, 0).contains(LYRICS_TITLE));
        Ok(())
    }

    /// Spec *Widening with lyrics*: no frame shows "No lyrics" first.
    #[test]
    fn widening_never_flashes_no_lyrics() -> Result<()> {
        let (_tmp, mut app) = folder(&[("song", Some("[00:00.50]Hello\n"))])?;
        press(&mut app, KeyCode::Enter);
        let mut view = View::default();
        settle(&mut app, &mut view, 90, SHORT);
        let mut shown = false;
        for _ in 0..3 {
            let screen = lines(&frame(&mut app, &mut view, 120, SHORT));
            assert!(!screen.iter().any(|l| l.contains(NO_LYRICS)), "{screen:#?}");
            shown |= screen.iter().any(|l| l.contains("Hello"));
        }
        assert!(shown, "the lyrics appear");
        Ok(())
    }

    /// Spec *Nothing played yet*.
    #[test]
    fn nothing_playing_at_start() -> Result<()> {
        let (_tmp, mut app) = folder(&[("song", Some("[00:01.00]Hello\n"))])?;
        let mut view = View::default();
        let screen = settle(&mut app, &mut view, 120, SHORT);
        let rows = pane_rows(&screen, 120);
        assert_eq!(rows[0], NOTHING_PLAYING, "{rows:#?}");
        assert!(is_message(row_style(&screen, 120, 0)), "like an empty list");
        Ok(())
    }

    /// Spec *Track without lyrics*: "No lyrics", and the side panes keep
    /// their widths.
    #[test]
    fn no_lyrics_keeps_the_layout() -> Result<()> {
        let (_tmp, mut app) = folder(&[("song", None)])?;
        press(&mut app, KeyCode::Enter);
        let mut view = View::default();
        let screen = settle(&mut app, &mut view, 120, SHORT);
        let rows = pane_rows(&screen, 120);
        assert_eq!(rows[0], NO_LYRICS, "{rows:#?}");
        assert!(is_message(row_style(&screen, 120, 0)));
        assert_eq!(screen[(pane_x(120), 0)].symbol(), "┌");
        assert_eq!(screen[(sidebar_width(120), 0)].symbol(), "┌");
        Ok(())
    }

    /// The row of the pane's inside showing `text`.
    fn row_of(rows: &[String], text: &str) -> Option<usize> {
        rows.iter().position(|r| r == text)
    }

    /// Spec *Current line highlighted*.
    #[test]
    fn current_line_is_highlighted_in_the_middle() -> Result<()> {
        let text = numbered(40);
        let (_tmp, mut app) = folder(&[("song", Some(&text))])?;
        press(&mut app, KeyCode::Enter);
        app.playback_mut().audio_mut().position = Duration::from_millis(21_500);
        let mut view = View::default();
        let screen = settle(&mut app, &mut view, 120, SHORT);
        let rows = pane_rows(&screen, 120);
        let Some(current) = row_of(&rows, "Line 20") else {
            bail!("current line not shown: {rows:#?}");
        };
        assert_eq!(current, INNER_ROWS / 2, "in the middle: {rows:#?}");
        assert!(is_playing(row_style(&screen, 120, current)));
        assert_eq!(row_style(&screen, 120, current - 1).fg, theme::DIM.fg);
        assert_eq!(row_style(&screen, 120, 0).fg, theme::DIM.fg);
        let after = row_style(&screen, 120, current + 1);
        assert!(!is_playing(after) && after.fg != theme::DIM.fg);
        Ok(())
    }

    /// Spec *Before the first line*.
    #[test]
    fn before_the_first_line_nothing_is_highlighted() -> Result<()> {
        let (_tmp, mut app) = folder(&[("song", Some("[00:20.00]First\n[00:25.00]Second\n"))])?;
        press(&mut app, KeyCode::Enter);
        app.playback_mut().audio_mut().position = Duration::from_secs(5);
        let mut view = View::default();
        let screen = settle(&mut app, &mut view, 120, SHORT);
        let rows = pane_rows(&screen, 120);
        assert_eq!(rows[..2], ["First", "Second"], "from the top: {rows:#?}");
        for row in 0..INNER_ROWS {
            assert!(!is_playing(row_style(&screen, 120, row)), "row {row}");
        }
        Ok(())
    }

    /// The text of the highlighted row, if any.
    fn highlighted(screen: &Buffer) -> Option<String> {
        let rows = pane_rows(screen, 120);
        (0..INNER_ROWS)
            .find(|&row| is_playing(row_style(screen, 120, row)))
            .and_then(|row| rows.get(row).cloned())
    }

    /// Spec *Seeking*, *Paused*.
    #[test]
    fn highlight_follows_seeking_and_holds_while_paused() -> Result<()> {
        let text = numbered(59);
        let (_tmp, mut app) = folder(&[("song", Some(&text))])?;
        press(&mut app, KeyCode::Enter);
        app.playback_mut().audio_mut().position = Duration::from_millis(15_500);
        let mut view = View::default();
        let screen = settle(&mut app, &mut view, 120, SHORT);
        assert_eq!(highlighted(&screen).as_deref(), Some("Line 14"));
        press(&mut app, KeyCode::Right);
        let position = app.playback().position();
        assert!(position > Duration::from_millis(15_500), "seeked");
        let expected = app
            .lyrics()
            .and_then(|l| l.current(position))
            .map(|i| format!("Line {i:02}"));
        let screen = frame(&mut app, &mut view, 120, SHORT);
        assert_eq!(highlighted(&screen), expected, "on the next frame");
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(app.playback().state(), PlayState::Paused);
        assert_eq!(app.lyrics_deadline(Instant::now()), None);
        for _ in 0..2 {
            let screen = frame(&mut app, &mut view, 120, SHORT);
            assert_eq!(highlighted(&screen), expected, "held while paused");
        }
        Ok(())
    }

    /// Spec *Long line*: every row of a wrapped current line is
    /// highlighted.
    #[test]
    fn long_current_line_wraps_highlighted() -> Result<()> {
        let long = "word ".repeat(20);
        let text = format!("[00:01.00]{long}\n[00:30.00]Next\n");
        let (_tmp, mut app) = folder(&[("song", Some(&text))])?;
        press(&mut app, KeyCode::Enter);
        app.playback_mut().audio_mut().position = Duration::from_secs(2);
        let mut view = View::default();
        let screen = settle(&mut app, &mut view, 120, SHORT);
        let rows = pane_rows(&screen, 120);
        let wrapped: Vec<usize> = (0..INNER_ROWS)
            .filter(|&row| rows[row].starts_with("word"))
            .collect();
        assert_eq!(wrapped.len(), 3, "99 columns in 36: {rows:#?}");
        for row in wrapped {
            assert!(is_playing(row_style(&screen, 120, row)), "row {row}");
        }
        Ok(())
    }

    /// Spec *Scroll with progress*.
    #[test]
    fn plain_lyrics_scroll_with_progress() -> Result<()> {
        let total = INNER_ROWS * 3;
        let text = (0..total)
            .map(|i| format!("Row {i:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (_tmp, mut app) = folder(&[("song", Some(&text))])?;
        press(&mut app, KeyCode::Enter);
        let mut view = View::default();
        let screen = settle(&mut app, &mut view, 120, SHORT);
        assert_eq!(pane_rows(&screen, 120)[0], "Row 00", "the start first");
        app.playback_mut().audio_mut().position = Duration::from_secs(100);
        let screen = frame(&mut app, &mut view, 120, SHORT);
        let rows = pane_rows(&screen, 120);
        // Half of the 26 rows that do not fit: the middle third.
        assert_eq!(rows[0], format!("Row {INNER_ROWS:02}"), "{rows:#?}");
        for row in 0..INNER_ROWS {
            let style = row_style(&screen, 120, row);
            assert!(!is_playing(style) && style.fg != theme::DIM.fg, "row {row}");
        }
        app.playback_mut().audio_mut().position = Duration::from_secs(200);
        let screen = frame(&mut app, &mut view, 120, SHORT);
        let rows = pane_rows(&screen, 120);
        assert_eq!(
            rows[INNER_ROWS - 1],
            format!("Row {:02}", total - 1),
            "the end"
        );
        Ok(())
    }

    /// Tiny and huge terminals draw without panicking, with the pane on
    /// and lyrics loaded.
    #[test]
    fn odd_sizes_never_panic() -> Result<()> {
        let text = format!("{}[00:02.00]{}\n", numbered(30), "漢字 ".repeat(40));
        let (_tmp, mut app) = folder(&[("song", Some(&text))])?;
        press(&mut app, KeyCode::Enter);
        app.playback_mut().audio_mut().position = Duration::from_secs(3);
        let mut view = View::default();
        for (width, height) in [
            (100, 1),
            (100, 4),
            (100, 6),
            (1, 1),
            (LYRICS_MIN_WIDTH, 2),
            (500, 200),
            (120, SHORT),
        ] {
            settle(&mut app, &mut view, width, height);
        }
        Ok(())
    }

    /// The wrapped rows are kept between frames and redone when the width
    /// or the lyrics change (spec *Edited while hidden*).
    #[test]
    fn wrapped_rows_follow_width_and_reloads() -> Result<()> {
        let (tmp, mut app) = folder(&[("song", Some("[00:01.00]Hello\n"))])?;
        press(&mut app, KeyCode::Enter);
        let mut view = View::default();
        settle(&mut app, &mut view, 120, SHORT);
        let columns = |view: &View| view.lyrics_rows.as_ref().map(|w| w.columns);
        assert_eq!(columns(&view), Some(36));
        settle(&mut app, &mut view, 200, SHORT);
        assert_eq!(columns(&view), Some(44));
        std::fs::write(tmp.path().join("song.lrc"), "[00:01.00]Edited\n")?;
        let screen = lines(&frame(&mut app, &mut view, 200, SHORT));
        assert!(
            screen.iter().any(|l| l.contains("Hello")),
            "not re-read while shown"
        );
        app.set_lyrics_on(false);
        frame(&mut app, &mut view, 200, SHORT);
        app.set_lyrics_on(true);
        let screen = lines(&settle(&mut app, &mut view, 200, SHORT));
        assert!(screen.iter().any(|l| l.contains("Edited")), "{screen:#?}");
        assert!(!screen.iter().any(|l| l.contains("Hello")), "{screen:#?}");
        Ok(())
    }

    #[test]
    fn wrap_breaks_at_spaces_and_long_words_at_the_column() {
        assert_eq!(wrap("hello world foo", 11), ["hello world", "foo"]);
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("ab abcdefgh c", 4), ["ab", "abcd", "efgh", "c"]);
        assert_eq!(wrap("", 10), [""]);
        assert_eq!(wrap("   ", 10), [""]);
        assert_eq!(wrap("short", 0), ["s", "h", "o", "r", "t"], "at least 1");
    }

    /// Wide (CJK) characters wrap by display width.
    #[test]
    fn wrap_counts_display_width() {
        assert_eq!(wrap("日本語の歌詞", 5), ["日本", "語の", "歌詞"]);
        assert_eq!(wrap("日本 語", 4), ["日本", "語"]);
        assert_eq!(wrap("日", 1), ["日"], "a char wider than the row");
        for text in ["日本語の歌詞です", "a 日本 bcdef 語の歌", "x"] {
            for columns in 2..12 {
                for row in wrap(text, columns) {
                    assert!(width(&row) <= columns, "{row:?} in {columns}");
                }
            }
        }
    }

    fn synced(texts: &[&str]) -> Lyrics {
        Lyrics::Synced(
            (0_u64..)
                .zip(texts)
                .map(|(i, &text)| LyricLine {
                    at: Duration::from_secs(i),
                    text: text.to_owned(),
                })
                .collect(),
        )
    }

    #[test]
    fn lyric_rows_mark_past_current_and_future() {
        let lyrics = synced(&["one", "two three four", "", "five"]);
        let rows = lyric_rows(&lyrics, 5);
        let lines: Vec<_> = rows.iter().map(|r| (r.line, r.text.as_str())).collect();
        assert_eq!(
            lines,
            [
                (0, "one"),
                (1, "two"),
                (1, "three"),
                (1, "four"),
                (2, ""),
                (3, "five")
            ]
        );
        assert_eq!(current_rows(&rows, Some(1)), Some(1..4));
        assert_eq!(current_rows(&rows, Some(3)), Some(5..6));
        assert_eq!(current_rows(&rows, None), None);
        assert_eq!(current_rows(&rows, Some(9)), None, "no such line");
        let roles: Vec<_> = rows
            .iter()
            .map(|r| row_role(&lyrics, r.line, Some(1)))
            .collect();
        assert_eq!(
            roles,
            [
                LyricRole::Past,
                LyricRole::Current,
                LyricRole::Current,
                LyricRole::Current,
                LyricRole::Future,
                LyricRole::Future,
            ]
        );
        assert!(
            rows.iter()
                .all(|r| row_role(&lyrics, r.line, None) == LyricRole::Future)
        );
        let plain = Lyrics::Plain(vec!["a".to_owned(), String::new(), "b".to_owned()]);
        let rows = lyric_rows(&plain, 20);
        assert_eq!(rows.len(), 3, "a blank line is one row");
        assert!(
            rows.iter()
                .all(|r| row_role(&plain, r.line, Some(1)) == LyricRole::Plain)
        );
    }

    #[test]
    fn synced_top_centres_the_current_line_within_bounds() {
        assert_eq!(synced_top(None, 40, 10), 0);
        assert_eq!(synced_top(Some(&(20..21)), 40, 10), 16, "4 rows above");
        assert_eq!(synced_top(Some(&(20..24)), 40, 10), 17, "3 above, 3 below");
        assert_eq!(
            synced_top(Some(&(20..35)), 40, 10),
            20,
            "taller than the pane"
        );
        assert_eq!(synced_top(Some(&(2..3)), 40, 10), 0, "near the top");
        assert_eq!(synced_top(Some(&(38..39)), 40, 10), 30, "near the end");
        assert_eq!(synced_top(Some(&(3..5)), 5, 10), 0, "all fit");
        assert_eq!(synced_top(Some(&(3..4)), 5, 0), 3, "no rows to show");
    }

    /// Clusters are never split, and rows are measured as the screen
    /// draws them.
    #[test]
    fn wrap_keeps_grapheme_clusters_whole() {
        let hearts = "❤\u{fe0f}".repeat(6);
        let rows = wrap(&hearts, 5);
        assert_eq!(
            rows,
            [
                "❤\u{fe0f}❤\u{fe0f}",
                "❤\u{fe0f}❤\u{fe0f}",
                "❤\u{fe0f}❤\u{fe0f}"
            ]
        );
        let accents = "e\u{301}".repeat(5);
        assert_eq!(
            wrap(&accents, 2),
            ["e\u{301}e\u{301}", "e\u{301}e\u{301}", "e\u{301}"]
        );
        assert_eq!(wrap("\u{200b}", 1), ["\u{200b}"], "zero width");
        assert_eq!(wrap("漢", 1), ["漢"], "wider than the row");
        for text in [
            hearts.as_str(),
            accents.as_str(),
            "a\u{200b}b\u{200b}c d",
            "漢字 x",
        ] {
            for columns in 2..8 {
                for row in wrap(text, columns) {
                    assert!(drawn_width(&row) <= columns, "{row:?} in {columns}");
                }
            }
        }
    }

    #[test]
    fn plain_top_follows_progress_and_stays_in_span() {
        let len = Some(Duration::from_secs(200));
        let half = Duration::from_secs(100);
        assert_eq!(plain_top(30, 10, half, len), 10, "half of 20");
        assert_eq!(plain_top(30, 10, Duration::ZERO, len), 0);
        assert_eq!(plain_top(30, 10, Duration::from_secs(200), len), 20);
        assert_eq!(
            plain_top(30, 10, Duration::from_secs(900), len),
            20,
            "past the end"
        );
        assert_eq!(plain_top(30, 10, half, None), 0, "unknown length");
        assert_eq!(
            plain_top(30, 10, half, Some(Duration::ZERO)),
            0,
            "zero length"
        );
        assert_eq!(plain_top(8, 10, half, len), 0, "all fit");
        for (total, position, length) in [
            (usize::MAX, Duration::MAX, Duration::MAX),
            (usize::MAX, Duration::MAX, Duration::from_millis(1)),
            (usize::MAX, Duration::from_millis(1), Duration::MAX),
            (1_000, Duration::MAX, Duration::from_millis(3)),
        ] {
            let span = total.saturating_sub(10);
            assert!(plain_top(total, 10, position, Some(length)) <= span);
        }
    }
}
