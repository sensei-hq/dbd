use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Copy, Clone, Debug, clap::ValueEnum)]
pub enum DepsArg {
    Report,
    Include,
}

impl From<DepsArg> for dbd_core::config::DepsPolicy {
    fn from(a: DepsArg) -> Self {
        match a {
            DepsArg::Report => dbd_core::config::DepsPolicy::Report,
            DepsArg::Include => dbd_core::config::DepsPolicy::Include,
        }
    }
}

#[derive(Parser)]
#[command(name = "dbd", version, about = "Database schema management as code")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Path to config file
    #[arg(short, long, default_value = "design.yaml", global = true)]
    pub config: PathBuf,

    /// Database connection URL
    #[arg(short, long, env = "DATABASE_URL", global = true, hide_env_values = true)]
    pub database: Option<String>,

    /// Environment (dev or prod)
    #[arg(short, long, default_value = "prod", global = true)]
    pub environment: String,

    /// Source directory or GitHub repo (owner/repo/path)
    #[arg(short, long, default_value = ".", global = true)]
    pub source: String,

    /// Verbose output (show all details)
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Scope name from design.yaml (default: full set)
    #[arg(long, global = true)]
    pub scope: Option<String>,

    /// Dependency policy override: report | include
    #[arg(long, global = true, value_enum)]
    pub deps: Option<DepsArg>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Validate project configuration
    Inspect {
        /// Inspect a specific entity by name
        #[arg(short, long)]
        name: Option<String>,
        /// Auto-fix formatting issues
        #[arg(long)]
        fix: bool,
        /// Resolve "Unresolved reference" warnings against the live database
        /// catalog (uses the -d/--database connection)
        #[arg(long = "from-db")]
        from_db: bool,
    },
    /// Apply DDL scripts to database
    Apply {
        /// Apply a specific entity only
        #[arg(short, long)]
        name: Option<String>,
        /// Print apply order without executing
        #[arg(long)]
        dry_run: bool,
        /// Also apply RLS policies after entities
        #[arg(long)]
        with_policies: bool,
        /// Allow this database to be re-pointed to a different scope
        /// (bypasses the scope guard).
        #[arg(long)]
        allow_scope_change: bool,
    },
    /// Combine all DDL into one file
    Combine {
        /// Destination SQL file
        #[arg(short, long, default_value = "init.sql")]
        file: PathBuf,
    },
    /// Load data files into database
    Import {
        /// Import a specific table only
        #[arg(short, long)]
        name: Option<String>,
        /// Load a specific file into the table named by --name (format
        /// inferred from the extension: .jsonl/.tsv/.csv). Requires --name.
        #[arg(short = 'f', long = "file")]
        file: Option<PathBuf>,
        /// Print what would be imported
        #[arg(long)]
        dry_run: bool,
    },
    /// Refresh materialized views (REFRESH MATERIALIZED VIEW [CONCURRENTLY])
    Refresh {
        /// Refresh a specific materialized view (or `schema.*` wildcard). Omit to refresh all.
        #[arg(short, long)]
        name: Option<String>,
    },
    /// Output dependency graph as JSON
    Graph {
        /// Scope to a specific entity's subgraph
        #[arg(short, long)]
        name: Option<String>,
    },
    /// Generate DBML documentation
    Dbml {
        /// Destination DBML file
        #[arg(short, long, default_value = "design.dbml")]
        file: PathBuf,
    },
    /// Open the hosted schema viewer with the schema encoded in the URL fragment
    Diagram {
        /// Emit the raw SchemaModel JSON to a file instead of opening the viewer
        #[arg(long)]
        json: bool,
        /// Destination file for --json (default: schema.json)
        #[arg(short, long, default_value = "schema.json")]
        file: PathBuf,
        /// Print the viewer URL instead of opening a browser
        #[arg(long)]
        print_url: bool,
        /// Base URL of the dbd site (default: https://dbd.sensei-hq.com)
        #[arg(long, env = "DBD_DIAGRAM_URL")]
        site: Option<String>,
    },
    /// Deploy from source: fetch + apply + import
    Deploy {
        /// Preview what would be executed
        #[arg(long)]
        dry_run: bool,
        /// Ignore any cached copy of the source and re-download it fresh
        /// (GitHub sources only)
        #[arg(long)]
        no_cache: bool,
        /// Remove the entire download cache before deploying
        #[arg(long)]
        clear_cache: bool,
        /// Allow this database to be re-pointed to a different scope
        /// (bypasses the scope guard).
        #[arg(long)]
        allow_scope_change: bool,
    },
    /// Create a versioned schema snapshot
    Snapshot {
        /// Description for this snapshot
        #[arg(short, long)]
        name: Option<String>,
        /// List existing snapshots
        #[arg(long)]
        list: bool,
    },
    /// Show migration status
    Migrate {
        /// Show local vs database version
        #[arg(long)]
        status: bool,
    },
    /// Drop the project's managed objects (with safety guards)
    Reset {
        /// Target platform
        #[arg(long, default_value = "postgres")]
        target: String,
        /// Print what would be dropped
        #[arg(long)]
        dry_run: bool,
        /// Override safety guards
        #[arg(long)]
        force: bool,
        /// Also drop the project's managed schemas (DROP SCHEMA … CASCADE),
        /// skipping protected schemas
        #[arg(long)]
        schemas: bool,
        /// Also drop the target's configured extensions (DROP EXTENSION … CASCADE)
        #[arg(long)]
        extensions: bool,
        /// Shorthand for --schemas --extensions
        #[arg(long)]
        clean: bool,
        /// Allow this database to be re-pointed to a different scope
        /// (bypasses the scope guard).
        #[arg(long)]
        allow_scope_change: bool,
    },
    /// Audit design.yaml for stale entries
    Doctor {
        /// Remove stale entries
        #[arg(long)]
        fix: bool,
    },
    /// Export table data to files
    Export {
        /// Export a specific table only
        #[arg(short, long)]
        name: Option<String>,
        /// Output format (csv, tsv, jsonl)
        #[arg(short, long, default_value = "csv")]
        format: String,
        /// Output directory; selected tables are written flat as
        /// <dir>/<name>.<fmt> (default: export/<schema>/<name>.<fmt>)
        #[arg(short = 'o', long = "output")]
        output: Option<PathBuf>,
    },
    /// Initialize a new dbd project
    Init {
        /// Project name (defaults to current directory name)
        #[arg(short, long)]
        name: Option<String>,
        /// Target platform
        #[arg(short, long, default_value = "postgres")]
        target: String,
        /// Reverse-engineer the project from a database connection string (or $DATABASE_URL)
        #[arg(long, value_name = "CONN", num_args = 0..=1, default_missing_value = "")]
        from_db: Option<String>,
        /// Reverse-engineer the project from a DBML file (mutually exclusive with --from-db)
        #[arg(long, value_name = "FILE", conflicts_with = "from_db")]
        from_dbml: Option<PathBuf>,
        /// Base project version written to design.yaml
        #[arg(long, default_value_t = 1)]
        version: u32,
        /// Limit to these schemas (repeatable)
        #[arg(long = "schema", value_name = "SCHEMA")]
        schemas: Vec<String>,
        /// Exclude these schemas (repeatable)
        #[arg(long = "exclude-schema", value_name = "SCHEMA")]
        exclude_schemas: Vec<String>,
        /// Include Supabase platform schemas (bypass the denylist)
        #[arg(long)]
        all_schemas: bool,
        /// Also reverse-engineer cluster-global roles (name + memberships; opt-in)
        #[arg(long)]
        roles: bool,
        /// Print the plan without writing
        #[arg(long)]
        dry_run: bool,
    },
    /// Sync a database into the current dbd project (reverse-engineer + merge)
    Merge {
        /// Database connection string (or $DATABASE_URL)
        conn: Option<String>,
        /// Sync from a DBML file instead of a live database (mutually exclusive with the conn argument)
        #[arg(long, value_name = "FILE", conflicts_with = "conn")]
        from_dbml: Option<PathBuf>,
        /// Limit to these schemas (repeatable)
        #[arg(long = "schema", value_name = "SCHEMA")]
        schemas: Vec<String>,
        /// Exclude these schemas (repeatable)
        #[arg(long = "exclude-schema", value_name = "SCHEMA")]
        exclude_schemas: Vec<String>,
        /// Include Supabase platform schemas (bypass the denylist)
        #[arg(long)]
        all_schemas: bool,
        /// Also reverse-engineer cluster-global roles (name + memberships; opt-in)
        #[arg(long)]
        roles: bool,
        /// Print the plan without writing
        #[arg(long)]
        dry_run: bool,
    },
    /// Format DDL files
    Format {
        /// Check formatting without modifying files (exit 1 if unformatted)
        #[arg(long)]
        check: bool,
    },
    /// Apply RLS policies from policies/ directory
    Policies {
        /// Print what would be applied without executing
        #[arg(long)]
        dry_run: bool,
    },
    /// Show the complete difference between the live database and the design
    /// (read-only). Covers columns, PK/unique, foreign keys, CHECK constraints,
    /// indexes, comments and enums — everything `reconcile --dry-run` omits.
    Diff {
        /// Emit the diff as JSON for tooling/CI
        #[arg(long)]
        json: bool,
        /// Exit 2 when differences exist, 0 when in sync (errors exit 1)
        #[arg(long)]
        exit_code: bool,
    },
    /// Reconcile the live database to the design in place (pre-release, declarative)
    ///
    /// Diffs the live schema against the design and applies ALTER/CREATE
    /// directly — no snapshots, no version bump. Disabled once the project is
    /// released; whole-table drops stay a `snapshot`/`reset` operation.
    Reconcile {
        /// Preview the plan without executing
        #[arg(long)]
        dry_run: bool,
        /// Allow destructive changes (dropping columns/constraints)
        #[arg(long)]
        allow_destructive: bool,
        /// Drop orphaned tables — those in a managed schema but no longer in the design
        #[arg(long)]
        prune: bool,
        /// Allow this database to be re-pointed to a different scope
        /// (bypasses the scope guard).
        #[arg(long)]
        allow_scope_change: bool,
    },
    /// Release the current version: write a baseline snapshot and lock in the
    /// snapshot/migration workflow (disables `reconcile`)
    #[command(visible_alias = "baseline")]
    Release {
        /// Description for the baseline snapshot
        #[arg(short, long)]
        name: Option<String>,
    },
    /// Install dbd's Claude Code skill + agent so an AI assistant understands
    /// this project's schema-as-code conventions.
    ///
    /// Global (~/.claude, or $CLAUDE_CONFIG_DIR) is personal to your machine and
    /// applies to every project you open. `--project` writes ./.claude instead,
    /// which is shared with the team only once you commit that directory. Assets
    /// are embedded in the binary — no repo checkout or network needed.
    Install {
        /// Install into ./.claude instead of ~/.claude. Commit that directory to
        /// share the skill + agent with everyone who clones the repo; gitignore
        /// it to keep the install local to your checkout.
        #[arg(long)]
        project: bool,
        /// Preview what would be written without touching the filesystem
        #[arg(long)]
        dry_run: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// clap's own definition validation (duplicate ids, bad defaults, etc.).
    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    /// Parse every subcommand with no extra args. This exercises the
    /// matches→struct downcast, which panics if a subcommand arg collides with a
    /// `global = true` arg of a different type (e.g. an `Inspect` `database: bool`
    /// vs the global `database: Option<String>`). `debug_assert` does NOT catch
    /// that — only actually parsing does.
    #[test]
    fn every_subcommand_parses() {
        let cmds = [
            "inspect", "apply", "combine", "import", "refresh", "graph", "dbml", "diagram", "deploy",
            "snapshot", "migrate", "reset", "doctor", "export", "init", "format",
            "policies", "diff", "reconcile", "release", "install",
        ];
        for c in cmds {
            Cli::try_parse_from(["dbd", c])
                .unwrap_or_else(|e| panic!("`dbd {c}` failed to parse: {e}"));
        }
        // merge requires a positional conn argument
        Cli::try_parse_from(["dbd", "merge", "postgres://x"])
            .unwrap_or_else(|e| panic!("`dbd merge postgres://x` failed to parse: {e}"));
    }

    #[test]
    fn init_from_db_and_merge_parse() {
        let init = Cli::try_parse_from(["dbd", "init", "--from-db", "postgres://x", "--version", "2"]);
        assert!(init.is_ok(), "init --from-db failed");
        let merge = Cli::try_parse_from(["dbd", "merge", "postgres://x", "--dry-run", "--all-schemas"]);
        assert!(merge.is_ok(), "merge failed");
    }

    /// `--roles` is accepted on both `init --from-db` and `merge`, defaults to false
    /// when absent, and parses to `true` when present.
    #[test]
    fn roles_flag_parses_on_init_and_merge() {
        // init: present → true
        let cli = Cli::try_parse_from(["dbd", "init", "--from-db", "postgres://x", "--roles"])
            .expect("init --from-db --roles should parse");
        assert!(
            matches!(&cli.command, Commands::Init { roles: true, .. }),
            "expected init roles == true"
        );
        // init: absent → false
        let cli = Cli::try_parse_from(["dbd", "init", "--from-db", "postgres://x"])
            .expect("init --from-db should parse");
        assert!(
            matches!(&cli.command, Commands::Init { roles: false, .. }),
            "expected init roles == false when absent"
        );
        // merge: present → true
        let cli = Cli::try_parse_from(["dbd", "merge", "postgres://x", "--roles"])
            .expect("merge --roles should parse");
        assert!(
            matches!(&cli.command, Commands::Merge { roles: true, .. }),
            "expected merge roles == true"
        );
        // merge: absent → false
        let cli = Cli::try_parse_from(["dbd", "merge", "postgres://x"])
            .expect("merge should parse");
        assert!(
            matches!(&cli.command, Commands::Merge { roles: false, .. }),
            "expected merge roles == false when absent"
        );
    }

    /// `dbd deploy` cache flags default to false and flip to true when present.
    #[test]
    fn deploy_cache_flags_parse() {
        // absent → both false
        let cli = Cli::try_parse_from(["dbd", "deploy"]).expect("deploy should parse");
        assert!(
            matches!(&cli.command, Commands::Deploy { no_cache: false, clear_cache: false, .. }),
            "expected deploy cache flags == false when absent"
        );
        // present → both true
        let cli = Cli::try_parse_from(["dbd", "deploy", "--no-cache", "--clear-cache"])
            .expect("deploy --no-cache --clear-cache should parse");
        assert!(
            matches!(&cli.command, Commands::Deploy { no_cache: true, clear_cache: true, .. }),
            "expected deploy cache flags == true when present"
        );
    }

    /// `dbd diff` flags default to false and flip to true when present.
    #[test]
    fn diff_flags_parse() {
        let cli = Cli::try_parse_from(["dbd", "diff"]).expect("diff parses");
        assert!(matches!(&cli.command, Commands::Diff { json: false, exit_code: false }));
        let cli = Cli::try_parse_from(["dbd", "diff", "--json", "--exit-code"]).expect("diff flags parse");
        assert!(matches!(&cli.command, Commands::Diff { json: true, exit_code: true }));
    }

    /// `dbd reconcile` flags default to false and flip to true when present.
    #[test]
    fn reconcile_flags_parse() {
        let cli = Cli::try_parse_from(["dbd", "reconcile"]).expect("reconcile should parse");
        assert!(
            matches!(
                &cli.command,
                Commands::Reconcile { dry_run: false, allow_destructive: false, prune: false, .. }
            ),
            "expected reconcile flags == false when absent"
        );
        let cli = Cli::try_parse_from([
            "dbd", "reconcile", "--dry-run", "--allow-destructive", "--prune",
        ])
        .expect("reconcile flags should parse");
        assert!(
            matches!(
                &cli.command,
                Commands::Reconcile { dry_run: true, allow_destructive: true, prune: true, .. }
            ),
            "expected reconcile flags == true when present"
        );
    }

    /// `--allow-scope-change` parses on every write subcommand and defaults false.
    #[test]
    fn allow_scope_change_flag_parses() {
        for c in ["apply", "deploy", "reconcile", "reset"] {
            let on = Cli::try_parse_from(["dbd", c, "--allow-scope-change"])
                .unwrap_or_else(|e| panic!("`dbd {c} --allow-scope-change` failed: {e}"));
            let flag = match &on.command {
                Commands::Apply { allow_scope_change, .. } => *allow_scope_change,
                Commands::Deploy { allow_scope_change, .. } => *allow_scope_change,
                Commands::Reconcile { allow_scope_change, .. } => *allow_scope_change,
                Commands::Reset { allow_scope_change, .. } => *allow_scope_change,
                _ => unreachable!(),
            };
            assert!(flag, "{c} --allow-scope-change should be true");

            let off = Cli::try_parse_from(["dbd", c]).unwrap();
            let flag_off = match &off.command {
                Commands::Apply { allow_scope_change, .. } => *allow_scope_change,
                Commands::Deploy { allow_scope_change, .. } => *allow_scope_change,
                Commands::Reconcile { allow_scope_change, .. } => *allow_scope_change,
                Commands::Reset { allow_scope_change, .. } => *allow_scope_change,
                _ => unreachable!(),
            };
            assert!(!flag_off, "{c} allow_scope_change should default false");
        }
    }

    /// `dbd release` parses, and `dbd baseline` is accepted as a visible alias.
    #[test]
    fn release_and_baseline_alias_parse() {
        let cli = Cli::try_parse_from(["dbd", "release"]).expect("release should parse");
        assert!(matches!(&cli.command, Commands::Release { name: None }));
        let cli = Cli::try_parse_from(["dbd", "release", "--name", "v1 GA"])
            .expect("release --name should parse");
        assert!(matches!(&cli.command, Commands::Release { name: Some(_) }));
        let cli = Cli::try_parse_from(["dbd", "baseline"]).expect("baseline alias should parse");
        assert!(
            matches!(&cli.command, Commands::Release { .. }),
            "baseline should alias to Release"
        );
    }

    /// `--force-overwrite` was removed from both `init` and `merge` when the reverse
    /// path unified on overwrite+snapshot — parsing it must now fail.
    #[test]
    fn force_overwrite_is_rejected() {
        assert!(
            Cli::try_parse_from(["dbd", "init", "--from-db", "postgres://x", "--force-overwrite"]).is_err(),
            "init --force-overwrite should be rejected (flag removed)"
        );
        assert!(
            Cli::try_parse_from(["dbd", "merge", "postgres://x", "--force-overwrite"]).is_err(),
            "merge --force-overwrite should be rejected (flag removed)"
        );
    }

    /// `dbd reset` and its flags parse; `--clean`/`--schemas`/`--extensions`
    /// default to false and flip to true when present, alongside `--force`/`--dry-run`.
    #[test]
    fn reset_flags_parse() {
        // bare reset → all opt-in flags false
        let cli = Cli::try_parse_from(["dbd", "reset"]).expect("reset should parse");
        assert!(
            matches!(
                &cli.command,
                Commands::Reset { schemas: false, extensions: false, clean: false, force: false, dry_run: false, .. }
            ),
            "expected all reset flags false by default"
        );
        // --schemas --extensions
        let cli = Cli::try_parse_from(["dbd", "reset", "--schemas", "--extensions"])
            .expect("reset --schemas --extensions should parse");
        assert!(matches!(
            &cli.command,
            Commands::Reset { schemas: true, extensions: true, clean: false, .. }
        ));
        // --clean --force --dry-run
        let cli = Cli::try_parse_from(["dbd", "reset", "--clean", "--force", "--dry-run"])
            .expect("reset --clean --force --dry-run should parse");
        assert!(matches!(
            &cli.command,
            Commands::Reset { clean: true, force: true, dry_run: true, .. }
        ));
    }

    /// `dbd init --from-db` (no value) must parse to `from_db == Some("")` (env-fallback sentinel).
    #[test]
    fn init_from_db_no_value_uses_env_fallback_sentinel() {
        let cli = Cli::try_parse_from(["dbd", "init", "--from-db"])
            .expect("init --from-db (no value) should parse");
        assert!(
            matches!(&cli.command, Commands::Init { from_db: Some(s), .. } if s.is_empty()),
            "expected from_db == Some(\"\")"
        );
    }

    /// `dbd init --from-db postgres://h/db` must parse to that URL.
    #[test]
    fn init_from_db_with_value_captures_url() {
        let cli = Cli::try_parse_from(["dbd", "init", "--from-db", "postgres://h/db"])
            .expect("init --from-db <url> should parse");
        assert!(
            matches!(&cli.command, Commands::Init { from_db: Some(s), .. } if s == "postgres://h/db"),
            "expected from_db == Some(\"postgres://h/db\")"
        );
    }

    /// `dbd init` (no --from-db flag at all) must parse to `from_db == None`.
    #[test]
    fn init_without_from_db_parses_to_none() {
        let cli = Cli::try_parse_from(["dbd", "init"])
            .expect("init with no flags should parse");
        assert!(
            matches!(&cli.command, Commands::Init { from_db: None, .. }),
            "expected from_db == None"
        );
    }

    /// `dbd init --from-dbml <file>` and `dbd merge --from-dbml <file>` parse, and
    /// the parsed value is captured on the right subcommand.
    #[test]
    fn init_and_merge_from_dbml_parse() {
        let init = Cli::try_parse_from(["dbd", "init", "--from-dbml", "schema.dbml", "--name", "demo"])
            .expect("init --from-dbml should parse");
        assert!(
            matches!(&init.command, Commands::Init { from_dbml: Some(p), .. } if p.ends_with("schema.dbml")),
            "expected init from_dbml == Some(schema.dbml)"
        );
        let merge = Cli::try_parse_from(["dbd", "merge", "--from-dbml", "schema.dbml", "--dry-run"])
            .expect("merge --from-dbml should parse");
        assert!(
            matches!(&merge.command, Commands::Merge { from_dbml: Some(p), .. } if p.ends_with("schema.dbml")),
            "expected merge from_dbml == Some(schema.dbml)"
        );
    }

    /// `--from-db` and `--from-dbml` are mutually exclusive on `init`.
    #[test]
    fn init_from_db_and_from_dbml_together_is_rejected() {
        let res = Cli::try_parse_from([
            "dbd", "init", "--from-db", "postgres://x", "--from-dbml", "schema.dbml",
        ]);
        assert!(res.is_err(), "init --from-db + --from-dbml together should be rejected");
    }

    /// On `merge`, the positional `conn` and `--from-dbml` are mutually exclusive.
    #[test]
    fn merge_conn_and_from_dbml_together_is_rejected() {
        let res = Cli::try_parse_from([
            "dbd", "merge", "postgres://x", "--from-dbml", "schema.dbml",
        ]);
        assert!(res.is_err(), "merge conn + --from-dbml together should be rejected");
    }

    /// `import -f/--file` parses and requires nothing at the clap layer (the
    /// `--file requires --name` rule is enforced in the handler, not clap), and
    /// the back-compat forms (`import`, `import -n t`) still parse.
    #[test]
    fn import_file_flag_parses_and_back_compat_holds() {
        let cli = Cli::try_parse_from(["dbd", "import", "-n", "t", "-f", "f.jsonl"])
            .expect("import -n t -f f.jsonl should parse");
        assert!(
            matches!(&cli.command, Commands::Import { name: Some(n), file: Some(p), .. }
                if n == "t" && p.ends_with("f.jsonl")),
            "expected name == t and file == f.jsonl"
        );
        // back-compat: no flags
        let cli = Cli::try_parse_from(["dbd", "import"]).expect("import should parse");
        assert!(matches!(&cli.command, Commands::Import { name: None, file: None, dry_run: false }));
        // back-compat: just -n
        let cli = Cli::try_parse_from(["dbd", "import", "-n", "t"]).expect("import -n t should parse");
        assert!(matches!(&cli.command, Commands::Import { name: Some(n), file: None, .. } if n == "t"));
    }

    /// `dbd refresh` with no `--name` parses to `name == None` (refresh all).
    #[test]
    fn parses_refresh_all() {
        let cli = Cli::try_parse_from(["dbd", "refresh"]).unwrap();
        assert!(matches!(&cli.command, Commands::Refresh { name: None }));
    }

    /// `dbd refresh -n <qualified-name>` captures the selector.
    #[test]
    fn parses_refresh_named() {
        let cli = Cli::try_parse_from(["dbd", "refresh", "-n", "analytics.daily_sales"]).unwrap();
        assert!(matches!(&cli.command, Commands::Refresh { name: Some(n) } if n == "analytics.daily_sales"));
    }

    /// `export -o/--output` parses alongside `-f/--format`, and the back-compat
    /// form (`export -n t -f jsonl`, no `-o`) still parses.
    #[test]
    fn export_output_flag_parses_and_back_compat_holds() {
        let cli = Cli::try_parse_from(["dbd", "export", "-n", "t", "-f", "jsonl", "-o", "out"])
            .expect("export -n t -f jsonl -o out should parse");
        assert!(
            matches!(&cli.command, Commands::Export { name: Some(n), format, output: Some(p) }
                if n == "t" && format == "jsonl" && p.ends_with("out")),
            "expected name == t, format == jsonl, output == out"
        );
        // back-compat: no -o → output None, format defaults preserved
        let cli = Cli::try_parse_from(["dbd", "export", "-n", "t", "-f", "jsonl"])
            .expect("export -n t -f jsonl should parse");
        assert!(matches!(&cli.command, Commands::Export { output: None, format, .. } if format == "jsonl"));
    }

    /// `--help` must never render the resolved `$DATABASE_URL` value (which can
    /// carry a password). clap binds the arg to the env var, so without
    /// `hide_env_values` it would print `[env: DATABASE_URL=<the connection string>]`.
    /// Assert the arg keeps `hide_env_values` set (introspecting the definition
    /// avoids mutating the process env, which would race parallel tests).
    #[test]
    fn database_arg_hides_env_value_in_help() {
        let cmd = Cli::command();
        let arg = cmd
            .get_arguments()
            .find(|a| a.get_id() == "database")
            .expect("global --database arg should exist");
        assert!(
            arg.is_hide_env_values_set(),
            "the --database arg must set hide_env_values so `--help` never echoes \
             the resolved $DATABASE_URL (which can carry a password)"
        );
    }

    /// `dbd install` flags default to false and flip to true when present; the
    /// global default (no `--project`) is the intended shape.
    #[test]
    fn install_flags_parse() {
        // bare install → global, not a dry run
        let cli = Cli::try_parse_from(["dbd", "install"]).expect("install should parse");
        assert!(
            matches!(&cli.command, Commands::Install { project: false, dry_run: false }),
            "expected install project == false, dry_run == false by default"
        );
        // --project --dry-run
        let cli = Cli::try_parse_from(["dbd", "install", "--project", "--dry-run"])
            .expect("install --project --dry-run should parse");
        assert!(
            matches!(&cli.command, Commands::Install { project: true, dry_run: true }),
            "expected install project == true, dry_run == true when present"
        );
    }

    /// The global `-d/--database <url>` and `inspect --from-db` must coexist.
    #[test]
    fn inspect_db_flags_coexist() {
        let cli = Cli::try_parse_from(["dbd", "inspect", "--from-db", "-d", "postgres://x"])
            .expect("inspect --from-db -d <url> should parse");
        assert!(matches!(cli.command, Commands::Inspect { from_db: true, .. }));
        assert_eq!(cli.database.as_deref(), Some("postgres://x"));
    }
}
