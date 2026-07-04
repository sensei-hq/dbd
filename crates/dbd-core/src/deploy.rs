use std::path::{Path, PathBuf};

use crate::error::{DbdError, Result};
use crate::github;

/// Remove the entire GitHub download cache directory.
///
/// No-op if the cache does not exist yet. Only affects downloaded GitHub
/// sources; local sources are never cached.
pub fn clear_cache() -> Result<()> {
    clear_cache_dir(&github::cache_root())
}

fn clear_cache_dir(root: &Path) -> Result<()> {
    if root.exists() {
        std::fs::remove_dir_all(root)?;
    }
    Ok(())
}

/// Resolve a source string to a local project directory.
///
/// - Local path (starts with `.`, `/`, or exists on disk): return as-is
/// - GitHub source: download tarball, extract to cache, return cache path
///
/// When `no_cache` is true, any cached copy of the source is discarded and the
/// tarball is re-downloaded fresh. Local sources ignore `no_cache`.
pub async fn resolve_source(source: &str, no_cache: bool) -> Result<PathBuf> {
    if !github::is_github_source(source) {
        // Local path
        let path = PathBuf::from(source);
        if !path.exists() {
            return Err(DbdError::Config(format!(
                "Source directory not found: {source}"
            )));
        }
        return Ok(path);
    }

    // GitHub source
    let gh = github::parse_github_source(source)?;
    let cache = github::cache_dir(&gh.owner, &gh.repo, &gh.git_ref);

    if no_cache {
        // Drop any stale copy so the fresh download can't be shadowed by it.
        std::fs::remove_dir_all(&cache).ok();
    } else {
        // Check if already cached — resolve subpath first so subpath sources hit correctly
        let resolved = resolve_subpath(&cache, gh.subpath.as_deref());
        if resolved.join("design.yaml").exists() {
            return Ok(resolved);
        }
    }

    // Download and extract
    download_github_source(&gh, &cache).await?;
    Ok(resolve_subpath(&cache, gh.subpath.as_deref()))
}

#[cfg(feature = "deploy")]
async fn download_github_source(gh: &github::GitHubSource, cache: &std::path::Path) -> Result<()> {
    download_and_extract(gh, cache).await
}

#[cfg(not(feature = "deploy"))]
async fn download_github_source(_gh: &github::GitHubSource, _cache: &std::path::Path) -> Result<()> {
    Err(DbdError::Config(
        "GitHub deploy requires the 'deploy' feature (reqwest/flate2/tar)".to_string(),
    ))
}

fn resolve_subpath(base: &Path, subpath: Option<&str>) -> PathBuf {
    match subpath {
        Some(sp) => base.join(sp),
        None => base.to_path_buf(),
    }
}

#[cfg(feature = "deploy")]
async fn download_and_extract(
    gh: &github::GitHubSource,
    cache_dir: &Path,
) -> Result<()> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/tarball/{}",
        gh.owner, gh.repo, gh.git_ref
    );

    let client = reqwest::Client::builder()
        .user_agent("dbd-rs")
        .build()
        .map_err(|e| DbdError::GitHubSource(format!("HTTP client error: {e}")))?;

    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| DbdError::GitHubSource(format!("Failed to fetch {}: {e}", gh.label())))?;

    if !response.status().is_success() {
        return Err(DbdError::GitHubSource(format!(
            "GitHub returned {} for {}",
            response.status(),
            gh.label()
        )));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| DbdError::GitHubSource(format!("Failed to read response: {e}")))?;

    // Extract tarball
    std::fs::create_dir_all(cache_dir)?;
    let decoder = flate2::read::GzDecoder::new(&bytes[..]);
    let mut archive = tar::Archive::new(decoder);

    // GitHub tarballs have a top-level directory like "owner-repo-sha/"
    // We need to strip that prefix when extracting
    for entry in archive
        .entries()
        .map_err(|e| DbdError::GitHubSource(format!("Failed to read tarball: {e}")))?
    {
        let mut entry =
            entry.map_err(|e| DbdError::GitHubSource(format!("Tarball entry error: {e}")))?;

        let path = entry
            .path()
            .map_err(|e| DbdError::GitHubSource(format!("Invalid path in tarball: {e}")))?;

        // Strip the first component (github prefix directory)
        let stripped: PathBuf = path.components().skip(1).collect();
        if stripped.as_os_str().is_empty() {
            continue;
        }

        // Validate no path traversal
        let dest = cache_dir.join(&stripped);
        if !dest.starts_with(cache_dir) {
            continue; // Skip paths that would escape cache dir
        }

        if entry.header().entry_type().is_dir() {
            std::fs::create_dir_all(&dest)?;
        } else {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut file = std::fs::File::create(&dest)?;
            std::io::copy(&mut entry, &mut file)
                .map_err(|e| DbdError::GitHubSource(format!("Failed to extract file: {e}")))?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn resolve_subpath_without_sub() {
        let base = PathBuf::from("/cache/owner-repo-HEAD");
        assert_eq!(resolve_subpath(&base, None), base);
    }

    #[test]
    fn resolve_subpath_with_sub() {
        let base = PathBuf::from("/cache/owner-repo-HEAD");
        assert_eq!(
            resolve_subpath(&base, Some("database")),
            PathBuf::from("/cache/owner-repo-HEAD/database")
        );
    }

    #[tokio::test]
    async fn resolve_local_path() {
        let tmp = TempDir::new().unwrap();
        let result = resolve_source(tmp.path().to_str().unwrap(), false).await.unwrap();
        assert_eq!(result, tmp.path());
    }

    #[tokio::test]
    async fn resolve_local_path_not_found() {
        let result = resolve_source("/nonexistent/path/to/project", false).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[tokio::test]
    async fn resolve_source_cache_hit_with_subpath() {
        use crate::github::cache_dir;

        // Simulate a pre-populated cache for sensei-hq/daemon/database@test-cache-hit-v1
        let cache = cache_dir("sensei-hq", "daemon", "test-cache-hit-v1");
        let database_dir = cache.join("database");
        std::fs::create_dir_all(&database_dir).unwrap();
        std::fs::write(database_dir.join("design.yaml"), "project:\n  name: test\n").unwrap();

        // Should return the database/ subpath without attempting a network download
        let result = resolve_source("sensei-hq/daemon/database@test-cache-hit-v1", false).await;

        // Cleanup before assertions so a failure doesn't leave files behind
        std::fs::remove_dir_all(&cache).ok();

        let path = result.expect("should return cached path without downloading");
        assert!(
            path.ends_with("database"),
            "should resolve to database/ subpath, got: {}",
            path.display()
        );
    }

    #[test]
    fn clear_cache_dir_removes_everything() {
        // Operate on an isolated temp root so the shared real cache (and any
        // parallel test using it) is never touched.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("dbd");
        let source_a = root.join("owner-repo-a");
        let source_b = root.join("owner-repo-b");
        std::fs::create_dir_all(&source_a).unwrap();
        std::fs::create_dir_all(&source_b).unwrap();
        std::fs::write(source_a.join("design.yaml"), "project:\n  name: a\n").unwrap();

        clear_cache_dir(&root).expect("clear should succeed");
        assert!(!root.exists(), "entire cache root should be removed");

        // Idempotent: clearing an already-absent cache is a no-op, not an error.
        clear_cache_dir(&root).expect("clear on missing cache should be a no-op");
    }
}
