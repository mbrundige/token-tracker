//! Tiny ANSI helpers (no external deps).
//!
//! Port of `scripts/ansi.js`.
//!
//! Colors are enabled when stdout is a TTY, or when `FORCE_COLOR` is set.
//! Disabled when `NO_COLOR` is set (<https://no-color.org/>).
//!
//! Ports ansi.js's complete color helper surface even where no current call
//! site in this binary exercises every color (e.g. `blue`/`magenta`) or the
//! explicit-enabled constructor (`create_ansi_with`).
#![allow(dead_code)]

use std::io::IsTerminal;

const RESET: &str = "\x1b[0m";

/// Core color-want decision given whether the target stream is a TTY.
/// Mirrors Node's precedence: `FORCE_COLOR` wins over `NO_COLOR`.
fn wants_color_given_tty(is_tty: bool) -> bool {
    // Match Node's precedence: FORCE_COLOR wins over NO_COLOR.
    match std::env::var("FORCE_COLOR") {
        Ok(v) if v == "0" => return false,
        Ok(v) if !v.is_empty() => return true,
        _ => {}
    }
    if let Ok(v) = std::env::var("NO_COLOR") {
        if !v.is_empty() {
            return false;
        }
    }
    is_tty
}

/// Whether ANSI color output should be used for stdout: TTY detection plus
/// the `FORCE_COLOR` / `NO_COLOR` env var overrides.
///
/// Equivalent to JS `wantsColor(stream = process.stdout)` called with no
/// argument (the only way it's invoked anywhere in the JS codebase).
pub fn wants_color() -> bool {
    wants_color_given_tty(std::io::stdout().is_terminal())
}

fn wrap(enabled: bool, code: &str, text: &str) -> String {
    if !enabled || text.is_empty() {
        return text.to_string();
    }
    format!("\x1b[{code}m{text}{RESET}")
}

/// A small set of ANSI styling helpers.
///
/// Equivalent to the object returned by JS `createAnsi(enabled)`.
pub struct Ansi {
    pub enabled: bool,
}

impl Ansi {
    pub fn new(enabled: bool) -> Self {
        Ansi { enabled }
    }

    pub fn bold(&self, text: impl AsRef<str>) -> String {
        wrap(self.enabled, "1", text.as_ref())
    }

    pub fn dim(&self, text: impl AsRef<str>) -> String {
        wrap(self.enabled, "2", text.as_ref())
    }

    pub fn cyan(&self, text: impl AsRef<str>) -> String {
        wrap(self.enabled, "36", text.as_ref())
    }

    pub fn green(&self, text: impl AsRef<str>) -> String {
        wrap(self.enabled, "32", text.as_ref())
    }

    pub fn yellow(&self, text: impl AsRef<str>) -> String {
        wrap(self.enabled, "33", text.as_ref())
    }

    pub fn blue(&self, text: impl AsRef<str>) -> String {
        wrap(self.enabled, "34", text.as_ref())
    }

    pub fn magenta(&self, text: impl AsRef<str>) -> String {
        wrap(self.enabled, "35", text.as_ref())
    }

    /// Heat intensity: grey -> amber -> bright yellow.
    pub fn heat(&self, level: i64, ch: impl AsRef<str>) -> String {
        let ch = ch.as_ref();
        if !self.enabled {
            return ch.to_string();
        }
        // dim white, yellow, bold yellow, bright yellow, bold bright yellow
        const CODES: [&str; 5] = ["2;37", "33", "33;1", "93", "93;1"];
        let idx = level.max(0).min(CODES.len() as i64 - 1) as usize;
        format!("\x1b[{}m{}{}", CODES[idx], ch, RESET)
    }
}

/// Create an `Ansi` helper using the default color-detection logic.
///
/// Equivalent to JS `createAnsi()` called with no argument (defaults to
/// `wantsColor()`).
pub fn create_ansi() -> Ansi {
    Ansi::new(wants_color())
}

/// Create an `Ansi` helper with an explicit enabled/disabled state.
///
/// Equivalent to JS `createAnsi(enabled)` called with an explicit boolean
/// argument. Rust has no default-parameter overloading, so the
/// no-argument and explicit-argument JS call forms are split into
/// [`create_ansi`] and `create_ansi_with` respectively.
pub fn create_ansi_with(enabled: bool) -> Ansi {
    Ansi::new(enabled)
}
