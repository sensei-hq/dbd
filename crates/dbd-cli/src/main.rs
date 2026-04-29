mod cli;
mod commands;

use clap::Parser;

#[tokio::main]
async fn main() {
    let args = cli::Cli::parse();

    if let Err(e) = commands::run(&args.command, &args.config, &args.environment).await {
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}
