//! Terminal styling for the report.
//!
//! Four inks — a gate, a suspicion, a command, secondary text — and
//! everything else stays uncoloured. Colouring every row conveys as much
//! as colouring none.
//!
//! Secondary text is an explicit 256-colour grey rather than SGR 2. `dim`
//! is advisory and terminals are free to ignore it. Measured on a real
//! terminal, `dim` rendered paths at full brightness, so the finding did
//! not lead and the path did not follow. A grey is a colour, and colours
//! get honoured.
//!
//! Enabled only on a tty, and never when `NO_COLOR` is set. A piped
//! consumer therefore gets clean text without asking for it, so there is
//! no `--plain` flag and no separate machine-readable renderer.

use std::io::IsTerminal;

/// Which inks a render should use. `Ink::none()` is the piped case and
/// makes every accessor return the empty string, so call sites never
/// branch on whether styling is on.
#[derive(Clone, Copy)]
pub struct Ink {
    on: bool,
}

impl Ink {
    /// Styling for stdout as it actually is right now.
    pub fn stdout() -> Ink {
        Ink {
            on: std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
        }
    }

    /// Styling suppressed. Rendering to anything but a terminal goes
    /// through `stdout()` and gets this anyway; naming it is for tests,
    /// which must assert on text rather than on escapes.
    #[cfg(test)]
    pub fn none() -> Ink {
        Ink { on: false }
    }

    fn code(self, seq: &'static str) -> &'static str {
        if self.on { seq } else { "" }
    }

    /// Section names.
    pub fn bold(self) -> &'static str {
        self.code("\x1b[1m")
    }

    /// Paths, column headings, the label on a suggested command: present,
    /// but never the thing being read first.
    pub fn faint(self) -> &'static str {
        self.code("\x1b[38;5;245m")
    }

    /// Rungs 0-2. Red because a build fails on these, not because they
    /// are the biggest numbers on screen.
    pub fn gate(self) -> &'static str {
        self.code("\x1b[31m")
    }

    /// Rungs 3-4, which never block anything.
    pub fn suspicion(self) -> &'static str {
        self.code("\x1b[33m")
    }

    /// The one span on a line worth copying.
    pub fn command(self) -> &'static str {
        self.code("\x1b[36m")
    }

    pub fn off(self) -> &'static str {
        self.code("\x1b[0m")
    }
}

#[cfg(test)]
mod tests {
    use super::Ink;

    #[test]
    fn styling_off_emits_nothing_at_all() {
        // Every accessor must vanish together: a render that half-styles
        // leaves escape codes in a file someone is diffing.
        let ink = Ink::none();
        for seq in [
            ink.bold(),
            ink.faint(),
            ink.gate(),
            ink.suspicion(),
            ink.command(),
            ink.off(),
        ] {
            assert_eq!(seq, "");
        }
    }

    #[test]
    fn secondary_text_never_uses_sgr_2() {
        // Measured on a real terminal: `dim` was ignored and paths stayed
        // at full brightness. The grey is not a preference.
        let ink = Ink { on: true };
        assert_eq!(ink.faint(), "\x1b[38;5;245m");
        assert!(!ink.faint().contains("[2m"));
    }

    #[test]
    fn the_ladder_reads_differently_at_each_rung() {
        let ink = Ink { on: true };
        assert_ne!(ink.gate(), ink.suspicion());
        assert!(!ink.gate().is_empty() && !ink.suspicion().is_empty());
    }
}
