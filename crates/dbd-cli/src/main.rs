mod cli;
mod commands;
mod output;

use clap::Parser;
use output::Verbosity;
use std::path::PathBuf;

#[tokio::main]
async fn main() {
    let args = cli::Cli::parse();
    let verbosity = Verbosity::from_flag(args.verbose);

    // --source defines the project root. Everything is relative to it.
    let project_dir = PathBuf::from(&args.source);

    // Config file is always inside the project directory.
    let config = project_dir.join(&args.config);

    if let Err(e) = commands::run(
        &args.command,
        &config,
        &args.environment,
        args.database.as_deref(),
        &project_dir,
        verbosity,
    )
    .await
    {
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}
