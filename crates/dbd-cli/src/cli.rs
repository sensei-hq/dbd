use clap::{Parser, Subcommand};
use std::path::PathBuf;

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
}

#[derive(Subcommand)]
pub enum Commands {
    /// Validate project configuration
    Inspect {
        /// Inspect a specific entity by name
        #[arg(short, long)]
        name: Option<String>,
        /// Verbose output
        #[arg(long)]
        verbose: bool,
    },
    /// Apply DDL scripts to database
    Apply {
        /// Apply a specific entity only
        #[arg(short, long)]
        name: Option<String>,
        /// Print apply order without executing
        #[arg(long)]
        dry_run: bool,
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
    /// Apply pending migrations
    Migrate {
        /// Apply pending migrations
        #[arg(long)]
        apply: bool,
        /// Show local vs database version
        #[arg(long)]
        status: bool,
        /// Apply up to this version
        #[arg(long)]
        to: Option<u32>,
        /// Print SQL without executing
        #[arg(long)]
        dry_run: bool,
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
}
