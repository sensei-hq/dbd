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
    #[arg(short, long, env = "DATABASE_URL", global = true)]
    pub database: Option<String>,

    /// Environment (dev or prod)
    #[arg(short, long, default_value = "prod", global = true)]
    pub environment: String,

    /// Source directory or GitHub repo (owner/repo/path)
    #[arg(short, long, default_value = ".", global = true)]
    pub source: String,

    /// Target name from design.yaml
    #[arg(short, long, global = true)]
    pub target: Option<String>,

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
        /// Resolve "Unresolved reference" warnings against the live database catalog
        #[arg(long)]
        database: bool,
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
        /// Print what would be imported
        #[arg(long)]
        dry_run: bool,
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
    /// Deploy from source: fetch + apply + import
    Deploy {
        /// Preview what would be executed
        #[arg(long)]
        dry_run: bool,
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
    /// Drop all schemas (with safety guards)
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
    },
    /// Initialize a new dbd project
    Init {
        /// Project name (defaults to current directory name)
        #[arg(short, long)]
        name: Option<String>,
        /// Target platform
        #[arg(short, long, default_value = "postgres")]
        target: String,
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
}
