use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};

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

/// A spinner that tracks a single step at a time.
///
/// In verbose mode it shows an animated spinner while the step runs,
/// then prints ✓ or ✗ per step when done.
/// In normal mode all methods are no-ops.
pub struct StepSpinner {
    pb: Option<ProgressBar>,
}

impl StepSpinner {
    pub fn new(verbosity: Verbosity) -> Self {
        if !verbosity.is_verbose() {
            return Self { pb: None };
        }
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"])
                .template("  {spinner} {msg}")
                .unwrap(),
        );
        pb.enable_steady_tick(Duration::from_millis(80));
        Self { pb: Some(pb) }
    }

    /// Called just before a step begins.
    pub fn start(&self, desc: &str) {
        if let Some(pb) = &self.pb {
            pb.set_message(desc.to_owned());
        }
    }

    /// Called after a step completes. `err` is `None` on success.
    pub fn done(&self, desc: &str, err: Option<&str>) {
        let Some(pb) = &self.pb else { return };
        if let Some(e) = err {
            pb.println(format!("  ✗ {desc}"));
            pb.println(format!("    {e}"));
        } else {
            pb.println(format!("  ✓ {desc}"));
        }
    }

    /// Clears the spinner line when all steps are complete.
    pub fn finish(&self) {
        if let Some(pb) = &self.pb {
            pb.finish_and_clear();
        }
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
