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
    exit_quietly_when_stdout_closes();
    let args = cli::Cli::parse();
    let verbosity = Verbosity::from_flag(args.verbose);

    // --source defines the project root. Everything is relative to it.
    let mut project_dir = PathBuf::from(&args.source);
    let mut source = args.source.clone();
    let mut module = args.module.clone();

    // Run inside a module's folder, which holds no design.yaml of its own, the
    // workspace is found by walking up, as Cargo finds its — and the run is
    // that module's (#7). `init` and `install` set up where they are run.
    let walks_up = !matches!(args.command, cli::Commands::Init { .. } | cli::Commands::Install { .. });
    if walks_up
        && args.config.is_relative()
        && !project_dir.join(&args.config).exists()
        && !dbd_core::github::is_github_source(&args.source)
        && let Some((root, found)) = dbd_core::Workspace::locate(&project_dir, &args.config)
    {
        source = root.display().to_string();
        project_dir = root;
        if module.is_none() {
            module = found;
        }
    }

    // Config file is always inside the project directory.
    let config = project_dir.join(&args.config);

    if let Err(e) = commands::run(
        &args.command,
        &config,
        &args.environment,
        args.database.as_deref(),
        &project_dir,
        &source,
        args.scope.as_deref(),
        args.deps.map(Into::into),
        module.as_deref(),
        verbosity,
    )
    .await
    {
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}

/// End the process quietly once stdout's reader has gone away.
///
/// `println!` panics when a write fails, so `dbd inspect | head` ended with
/// `failed printing to stdout: Broken pipe` as soon as `head` had what it
/// wanted — every command prints that way, so every command did. One hook here
/// covers them all.
///
/// Restoring the default SIGPIPE action would also end it, but for every
/// descriptor: a Postgres connection over a Unix socket (which Linux writes
/// without `MSG_NOSIGNAL`) whose server went away would then kill dbd without a
/// word, where today the write fails and dbd says so. So only a failed print is
/// caught, and only when stdout itself now fails with `BrokenPipe` — any other
/// failure (a full disk behind `>`) still panics. It exits with 141, the status
/// of a process SIGPIPE ends, as `cat` and `grep` end.
fn exit_quietly_when_stdout_closes() {
    let report = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info
            .payload()
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| info.payload().downcast_ref::<&str>().copied())
            .unwrap_or_default();
        if message.starts_with("failed printing to std") && stdout_is_a_closed_pipe() {
            std::process::exit(141);
        }
        report(info);
    }));
}

/// Whether a write to stdout fails because nothing reads it any more.
fn stdout_is_a_closed_pipe() -> bool {
    use std::io::Write;
    let mut out = std::io::stdout();
    matches!(
        out.write_all(b"\n").and_then(|()| out.flush()),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe
    )
}
