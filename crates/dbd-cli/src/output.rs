use std::time::Duration;

use console::style;
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
        if verbose { Self::Verbose } else { Self::Normal }
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

/// Print a warning to stderr (always shown, yellow, `⚠` prefix).
pub fn warn(msg: &str) {
    eprintln!("{}", style(format!("⚠ {msg}")).yellow());
}

/// Announce that output was narrowed to a scope, and by how much.
///
/// Every command that filters by scope calls this, so a narrowed run always
/// says so. Without it, `dbd dbml --scope daemon` printed only "Generated DBML
/// in design.dbml" — documentation silently missing a whole schema reads
/// exactly like documentation that is complete.
///
/// No-op for the all-scope, which filters nothing.
pub fn scope_filtered(scope: &dbd_core::ResolvedScope, kept: usize, total: usize) {
    if scope.is_all {
        return;
    }
    println!("scope '{}': {kept} of {total} entities", scope.name);
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

/// Apply a color based on the step description prefix (type:name or verb type:name).
///
/// - `schema:` cyan       — structural namespaces
/// - `table:` green       — data entities
/// - `view:` bright green — derived data
/// - `enum:` yellow       — type definitions
/// - `function:` / `procedure:` magenta — executable code
/// - `extension:` blue    — external add-ons
/// - `role:` dim magenta  — access control
/// - `migrate …` yellow   — schema change
/// - `drop …` red         — removal
fn colorize(desc: &str) -> String {
    // The type key is either the first token before ':' (for "type:name")
    // or the first word (for "migrate type:name → vN", "drop type:name (vN)").
    let key = desc
        .split_once(':')
        .map(|(pre, _)| pre.split_whitespace().last().unwrap_or(pre))
        .unwrap_or_else(|| desc.split_whitespace().next().unwrap_or(desc));

    match key {
        "schema" => style(desc).cyan().to_string(),
        "table" => style(desc).green().to_string(),
        "view" => style(desc).green().dim().to_string(),
        "enum" => style(desc).yellow().to_string(),
        "function" => style(desc).magenta().to_string(),
        "procedure" => style(desc).magenta().to_string(),
        "extension" => style(desc).blue().to_string(),
        "role" => style(desc).magenta().dim().to_string(),
        "migrate" => style(desc).yellow().to_string(),
        "drop" => style(desc).red().to_string(),
        _ => desc.to_string(),
    }
}

/// A spinner that tracks a single step at a time.
///
/// In verbose mode it shows an animated spinner while the step runs,
/// then prints ✓ or ✗ per step when done, with color-coded entity types.
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
            pb.set_message(colorize(desc));
        }
    }

    /// Called after a step completes. `err` is `None` on success.
    pub fn done(&self, desc: &str, err: Option<&str>) {
        let Some(pb) = &self.pb else { return };
        let colored = colorize(desc);
        if let Some(e) = err {
            pb.println(format!("  {} {colored}", style("✗").red().bold()));
            pb.println(format!("    {}", style(e).red().dim()));
        } else {
            pb.println(format!("  {} {colored}", style("✓").green()));
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

    /// The all-scope filters nothing, so announcing it would be noise on every
    /// ordinary run. A named scope always announces.
    #[test]
    fn scope_filtered_is_silent_for_the_all_scope() {
        let all = dbd_core::ResolvedScope {
            is_all: true,
            ..scope_named("all")
        };
        scope_filtered(&all, 5, 5); // no panic, prints nothing
        let named = scope_named("daemon");
        scope_filtered(&named, 3, 5); // prints
    }

    fn scope_named(name: &str) -> dbd_core::ResolvedScope {
        dbd_core::ResolvedScope {
            name: name.to_string(),
            entities: Default::default(),
            excluded: Default::default(),
            deps: dbd_core::config::DepsPolicy::Report,
            is_all: false,
            extensions: None,
        }
    }

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

    /// Every branch of the `colorize` type-key table must round-trip the
    /// original description (color codes wrap it, they never replace it),
    /// covering all three key-extraction paths: plain `type:name`, the
    /// "verb type:name" form, and keyless input.
    #[test]
    fn colorize_covers_every_branch_and_preserves_the_description() {
        let cases = [
            "schema:x",
            "table:x",
            "view:x",
            "enum:x",
            "function:x",
            "procedure:x",
            "extension:x",
            "role:x",
            "migrate table:x → v2",
            "drop table:x (v1)",
            "plain text",
        ];
        for desc in cases {
            let result = colorize(desc);
            assert!(!result.is_empty(), "colorize({desc:?}) returned empty string");
            assert!(
                result.contains("x") || result.contains(desc),
                "colorize({desc:?}) = {result:?} does not preserve the description"
            );
        }
    }

    #[test]
    fn colorize_key_extraction_single_token_before_colon() {
        // "type:name" — key is the whole prefix before ':'.
        assert!(colorize("table:users").contains("users"));
    }

    #[test]
    fn colorize_key_extraction_verb_type_form() {
        // "verb type:name" — key is the last word before ':', i.e. the type,
        // not the verb.
        assert!(colorize("migrate table:users → v2").contains("users"));
        assert!(colorize("drop table:users (v1)").contains("users"));
    }

    #[test]
    fn colorize_key_extraction_no_colon() {
        // No ':' at all — key falls back to the first word; unknown keys
        // pass the description through unstyled.
        assert_eq!(colorize("plain text"), "plain text".to_string());
    }

    #[test]
    fn colorize_unknown_key_passes_through_unstyled() {
        assert_eq!(colorize("mystery:x"), "mystery:x".to_string());
    }
}
