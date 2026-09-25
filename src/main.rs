//! # Documentation links are checked
//!
//! `rustdoc`'s link and HTML lints are denied crate-wide, because they catch a
//! class of rot nothing else does: a doc comment that points at an item which
//! has been renamed, made private, or deleted still *reads* correctly. Only
//! rustdoc knows it no longer resolves.
//!
//! This is a `deny` in the source rather than a flag in CI so it travels with
//! the crate — a contributor running `cargo doc` locally sees the same failure
//! the pipeline would, and the rule is discoverable from the code that obeys
//! it. `cargo doc` is not run on every `cargo test`, so this costs nothing in
//! the inner loop; `.github/workflows/ci.yml` runs it once per push.
//!
//! Twenty-four of these had accumulated before the lint was turned on.
#![deny(
    rustdoc::broken_intra_doc_links,
    rustdoc::private_intra_doc_links,
    rustdoc::invalid_html_tags,
    rustdoc::bare_urls
)]

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
        &args.source,
        args.scope.as_deref(),
        args.deps.map(Into::into),
        verbosity,
    )
    .await
    {
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}
