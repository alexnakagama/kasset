//! Terminal setup and teardown, and the event loop driving the [`App`].
//!
//! The terminal is restored (raw mode off, main screen, cursor shown) on
//! every way out: normal quit, error, termination signal, and panic. The one
//! exception is a *second* termination signal of the same kind, an emergency
//! exit for when the loop is stuck (e.g. on a stalled network mount): it ends
//! the process at once, like `SIGKILL` would, and the terminal may then need
//! `reset`. While the
//! UI is up, the process's standard error is pointed at `/dev/null` so that C
//! libraries (ALSA in particular) cannot print over the screen.

use std::io::{self, IsTerminal, Stdout};
use std::panic::{self, PanicHookInfo};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use crossterm::event::{self, Event};
use crossterm::{cursor, execute, terminal};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::app::App;
use crate::panic_guard;
use crate::player::Audio;
use crate::text::sanitize;
use crate::ui::{self, View};

/// How long to wait for input before the next tick and redraw. Four frames a
/// second keeps the elapsed time accurate to the second.
const FRAME: Duration = Duration::from_millis(250);

/// Most input events handled between two redraws, so a flood of key repeats
/// cannot starve the screen.
const MAX_EVENTS_PER_FRAME: usize = 64;

/// The UI currently owns the terminal (raw mode, alternate screen).
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// The thread that owns the terminal; only it may tear the UI down.
static MAIN_THREAD: OnceLock<ThreadId> = OnceLock::new();

/// Report of a panic on another thread while the UI was up, to print once the
/// terminal is restored.
static BACKGROUND_PANIC: Mutex<Option<String>> = Mutex::new(None);

/// Why the UI loop ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// The user quit.
    Quit,
    /// A termination signal arrived; the payload is its number.
    Signal(i32),
}

/// Installs a panic hook that restores the terminal before the panic is
/// reported. Call once, early in `main`, on the thread that will run the UI.
///
/// Panics caught by [`panic_guard::quietly`] are not reported at all. A panic
/// on another thread while the UI is up is recorded and the UI loop stops with
/// an error, instead of the report being lost or drawn over.
pub fn install_panic_hook() {
    let _already = MAIN_THREAD.set(thread::current().id());
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        if panic_guard::is_quiet() {
            return;
        }
        let on_main = MAIN_THREAD.get() == Some(&thread::current().id());
        if !on_main && ACTIVE.load(Ordering::Acquire) {
            let report = describe_panic(info);
            let mut slot = BACKGROUND_PANIC
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            slot.get_or_insert(report);
            return;
        }
        restore();
        previous(info);
    }));
}

/// One-line, sanitised description of a panic.
fn describe_panic(info: &PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    let what = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .and_then(sanitize)
        .unwrap_or_else(|| "unknown cause".to_owned());
    let thread = thread::current();
    let name = thread.name().and_then(sanitize);
    let name = name.as_deref().unwrap_or("unnamed");
    match info.location() {
        Some(loc) => format!(
            "thread '{name}' panicked at {}:{}: {what}",
            loc.file(),
            loc.line()
        ),
        None => format!("thread '{name}' panicked: {what}"),
    }
}

fn take_background_panic() -> Option<String> {
    BACKGROUND_PANIC
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
}

/// Puts the terminal back the way it was. Idempotent and safe to call from the
/// panic hook; errors are ignored because there is nothing left to do.
fn restore() {
    if ACTIVE.swap(false, Ordering::AcqRel) {
        let _ignored = terminal::disable_raw_mode();
        let _ignored = execute!(io::stdout(), terminal::LeaveAlternateScreen, cursor::Show);
    }
    stderr::restore();
}

/// Checks that the UI can be drawn: standard output must be a terminal.
///
/// # Errors
///
/// Fails, with a message saying so, when it is not.
pub fn ensure_terminal() -> Result<()> {
    if !io::stdout().is_terminal() {
        bail!("standard output is not a terminal; run kasset in an interactive terminal");
    }
    Ok(())
}

/// The terminal in UI mode. Dropping it restores the terminal.
struct Tui {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl Tui {
    fn enter() -> Result<Self> {
        ensure_terminal()?;
        // Marked active first so that a failure halfway is still undone.
        ACTIVE.store(true, Ordering::Release);
        let setup = || -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
            terminal::enable_raw_mode()?;
            execute!(io::stdout(), terminal::EnterAlternateScreen, cursor::Hide)?;
            let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
            terminal.clear()?;
            Ok(terminal)
        };
        match setup() {
            Ok(terminal) => {
                stderr::silence();
                Ok(Self { terminal })
            }
            Err(err) => {
                restore();
                Err(err).context("cannot set up the terminal")
            }
        }
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        restore();
    }
}

/// Runs the UI until the user quits or a termination signal arrives.
///
/// # Errors
///
/// Fails when the terminal cannot be set up, drawn to or read from, or when a
/// background thread panicked.
pub fn run<A: Audio>(app: &mut App<A>) -> Result<Exit> {
    let signals = Signals::register()?;
    let mut tui = Tui::enter()?;
    let mut view = View::default();
    let exit = event_loop(&mut tui, app, &mut view, &signals);
    // Stop the audio before the screen goes back, so quitting is immediate.
    app.shutdown();
    drop(tui);
    // Checked after the restore: a background panic that lands while the UI
    // is still up (ACTIVE) is recorded and reported here; one that lands after
    // `drop(tui)` cleared ACTIVE goes to the regular hook, which prints it to
    // the restored stderr. Either way it is not lost.
    match take_background_panic() {
        Some(report) => bail!("internal error: {report}"),
        None => exit,
    }
}

fn event_loop<A: Audio>(
    tui: &mut Tui,
    app: &mut App<A>,
    view: &mut View,
    signals: &Signals,
) -> Result<Exit> {
    loop {
        app.tick(Instant::now());
        if app.should_quit() {
            return Ok(Exit::Quit);
        }
        if let Some(signal) = signals.received() {
            return Ok(Exit::Signal(signal));
        }
        if let Some(report) = take_background_panic() {
            bail!("internal error: {report}");
        }

        let now = Instant::now();
        tui.terminal
            .draw(|frame| ui::render(frame, app, view, now))
            .context("cannot draw to the terminal")?;
        app.set_page_size(view.page_size());

        // A resize needs no handling here: `draw` adapts to the new size on
        // the next frame, which follows immediately.
        if event::poll(FRAME).context("cannot read terminal input")? {
            for _ in 0..MAX_EVENTS_PER_FRAME {
                if let Event::Key(key) = event::read().context("cannot read terminal input")? {
                    app.handle_key(key);
                }
                if app.should_quit()
                    || !event::poll(Duration::ZERO).context("cannot read terminal input")?
                {
                    break;
                }
            }
        }
    }
}

/// Termination signals turned into flags polled by the event loop, so the
/// terminal is restored instead of being left in raw mode. A second signal of
/// the same kind terminates immediately, in case the loop is stuck; that exit
/// skips the restore (nothing else is async-signal-safe), see the module docs.
struct Signals {
    #[cfg(unix)]
    flags: Vec<(i32, std::sync::Arc<AtomicBool>)>,
}

impl Signals {
    #[cfg(unix)]
    fn register() -> Result<Self> {
        use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
        use signal_hook::flag;
        use std::sync::Arc;

        let mut flags = Vec::new();
        for signal in [SIGINT, SIGTERM, SIGHUP] {
            let raised = Arc::new(AtomicBool::new(false));
            // Order matters: the shutdown check must see the flag set by an
            // earlier delivery, not this one.
            flag::register_conditional_shutdown(signal, 128 + signal, Arc::clone(&raised))
                .and_then(|_| flag::register(signal, Arc::clone(&raised)))
                .context("cannot install signal handlers")?;
            flags.push((signal, raised));
        }
        Ok(Self { flags })
    }

    #[cfg(not(unix))]
    #[expect(clippy::unnecessary_wraps, reason = "same signature as on unix")]
    fn register() -> Result<Self> {
        Ok(Self {})
    }

    fn received(&self) -> Option<i32> {
        #[cfg(unix)]
        {
            self.flags
                .iter()
                .find(|(_, raised)| raised.load(Ordering::Acquire))
                .map(|&(signal, _)| signal)
        }
        #[cfg(not(unix))]
        {
            None
        }
    }
}

/// Redirecting standard error while the UI is up.
mod stderr {
    #[cfg(unix)]
    mod imp {
        use std::fs::File;
        use std::io::{self, IsTerminal};
        use std::os::fd::OwnedFd;
        use std::sync::{Mutex, PoisonError};

        /// The original standard error, while it is redirected.
        static SAVED: Mutex<Option<OwnedFd>> = Mutex::new(None);

        /// Points fd 2 at `/dev/null`, keeping the original to put back.
        ///
        /// Only done when standard error is a terminal: when it is redirected
        /// to a file, library messages cannot hurt the UI and are kept.
        /// Best effort: on failure standard error is left as it was.
        pub fn silence() {
            if !io::stderr().is_terminal() {
                return;
            }
            let mut saved = SAVED.lock().unwrap_or_else(PoisonError::into_inner);
            if saved.is_some() {
                return;
            }
            let Ok(null) = File::options().write(true).open("/dev/null") else {
                return;
            };
            // Close-on-exec, above the standard descriptors.
            let Ok(original) = rustix::io::fcntl_dupfd_cloexec(io::stderr(), 3) else {
                return;
            };
            if rustix::stdio::dup2_stderr(&null).is_ok() {
                *saved = Some(original);
            }
        }

        /// Puts the original standard error back, if it was redirected.
        pub fn restore() {
            let original = SAVED.lock().unwrap_or_else(PoisonError::into_inner).take();
            if let Some(original) = original {
                let _ignored = rustix::stdio::dup2_stderr(&original);
            }
        }
    }

    #[cfg(not(unix))]
    mod imp {
        pub fn silence() {}
        pub fn restore() {}
    }

    pub use imp::{restore, silence};
}
