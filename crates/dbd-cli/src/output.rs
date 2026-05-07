/// Output verbosity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verbosity {
    /// Show all details (entity names, SQL, full JSON)
    Verbose,
    /// Normal output (errors, warnings, progress, summary)
    Normal,
}

impl Verbosity {
    pub fn from_flag(verbose: bool) -> Self {
        if verbose {
            Self::Verbose
        } else {
            Self::Normal
        }
    }

    pub fn is_verbose(self) -> bool {
        self == Self::Verbose
    }
}

/// Print a line in normal + verbose mode.
pub fn info(verbosity: Verbosity, msg: &str) {
    let _ = verbosity;
    println!("{msg}");
}

/// Print only in verbose mode.
pub fn detail(verbosity: Verbosity, msg: &str) {
    if verbosity.is_verbose() {
        println!("{msg}");
    }
}

/// Always print (errors, JSON output, final counts).
pub fn always(msg: &str) {
    println!("{msg}");
}

/// Print a summary line with counts.
pub fn summary(errors: usize, warnings: usize, entities: usize) {
    if errors == 0 && warnings == 0 {
        println!("{entities} entities — no issues");
    } else {
        println!(
            "{entities} entities — {errors} error{}, {warnings} warning{}",
            if errors != 1 { "s" } else { "" },
            if warnings != 1 { "s" } else { "" },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbosity_from_flag() {
        assert_eq!(Verbosity::from_flag(true), Verbosity::Verbose);
        assert_eq!(Verbosity::from_flag(false), Verbosity::Normal);
    }

    #[test]
    fn verbose_is_verbose() {
        assert!(Verbosity::Verbose.is_verbose());
        assert!(!Verbosity::Normal.is_verbose());
    }

    #[test]
    fn detail_only_runs_in_verbose() {
        assert!(!Verbosity::Normal.is_verbose());
        assert!(Verbosity::Verbose.is_verbose());
    }
}
