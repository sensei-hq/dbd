//! Spawning `git` without the parent's `GIT_*` env.

use std::path::Path;

/// `git` in `dir`, without the parent's `GIT_*` env (#114): under
/// `git commit -a`'s hook `GIT_INDEX_FILE` is the real repo's
/// `index.lock`, which a child git must never use — a write lands in the
/// wrong index, and a read answers from it instead of the work tree's.
pub fn command(dir: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new("git");
    cmd.current_dir(dir);
    cmd.env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_COMMON_DIR");
    cmd
}
