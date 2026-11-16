//! Catching panics from code that parses untrusted files, quietly.
//!
//! Decoders and tag readers run on attacker-controllable input and may panic.
//! Those panics are caught and turned into errors, but `catch_unwind` alone
//! still runs the global panic hook first, which would print a report over the
//! terminal UI (or tear the UI down). [`quietly`] marks the current thread so
//! the hook installed by the terminal module can recognise such a panic as
//! handled and stay silent.

use std::cell::Cell;
use std::panic::{UnwindSafe, catch_unwind};

thread_local! {
    /// Set while the current thread runs inside [`quietly`].
    static QUIET: Cell<bool> = const { Cell::new(false) };
}

/// Runs `f`, returning `Err` with the panic payload if it panics. The panic
/// hook is told (via [`is_quiet`]) not to report the panic.
///
/// Requires `panic = "unwind"` (see `Cargo.toml`).
pub fn quietly<R>(f: impl FnOnce() -> R + UnwindSafe) -> std::thread::Result<R> {
    let outer = QUIET.with(|q| q.replace(true));
    let result = catch_unwind(f);
    QUIET.with(|q| q.set(outer));
    result
}

/// Whether the current thread is inside [`quietly`], i.e. a panic on it is
/// about to be caught and handled.
#[must_use]
pub fn is_quiet() -> bool {
    // `try_with` fails only while the thread's locals are being destroyed;
    // nothing is guarded then.
    QUIET.try_with(Cell::get).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_the_thread_only_inside() {
        assert!(!is_quiet());
        let inside = quietly(is_quiet);
        assert!(matches!(inside, Ok(true)));
        assert!(!is_quiet());
    }

    #[test]
    fn catches_panics_and_restores_state() {
        let nested = quietly(|| {
            let inner = quietly(|| -> () { panic!("boom") });
            (inner.is_err(), is_quiet())
        });
        assert!(matches!(nested, Ok((true, true))), "still quiet in outer");
        assert!(!is_quiet());
    }
}
