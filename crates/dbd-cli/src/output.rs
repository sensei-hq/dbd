/// Output verbosity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verbosity {
    /// Show all details (entity names, SQL, full JSON)
    Verbose,
    /// Normal output (errors, warnings, progress)
    Normal,
    /// Only show success/failure counts
    Silent,
}

impl Verbosity {
    pub fn from_flags(verbose: bool, silent: bool) -> Self {
        if silent {
            Self::Silent
        } else if verbose {
            Self::Verbose
        } else {
            Self::Normal
        }
    }

    pub fn is_silent(self) -> bool {
        self == Self::Silent
    }

    pub fn is_verbose(self) -> bool {
        self == Self::Verbose
    }
}

/// Print a line unless in silent mode.
pub fn info(verbosity: Verbosity, msg: &str) {
    if !verbosity.is_silent() {
        println!("{msg}");
    }
}

/// Print detail only in verbose mode.
pub fn detail(verbosity: Verbosity, msg: &str) {
    if verbosity.is_verbose() {
        println!("{msg}");
    }
}

/// Always print (errors, final counts).
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
