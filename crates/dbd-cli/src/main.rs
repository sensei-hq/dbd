mod cli;
mod commands;
mod output;

use clap::Parser;
use output::Verbosity;

#[tokio::main]
async fn main() {
    let args = cli::Cli::parse();
    let verbosity = Verbosity::from_flags(args.verbose, args.silent);

    if let Err(e) = commands::run(&args.command, &args.config, &args.environment, verbosity).await {
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}
