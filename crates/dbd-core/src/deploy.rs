use std::path::{Path, PathBuf};

#[cfg(feature = "deploy")]
use async_trait::async_trait;

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
    download_and_extract(gh, cache, &HttpFetcher).await
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

/// Fetches raw bytes for a GitHub source over the network.
///
/// Injected so `download_and_extract`'s error-handling and extraction logic
/// can be unit-tested with a fake transport — only `HttpFetcher::fetch`
/// itself (the real `reqwest` GET) needs a live network call to exercise.
#[cfg(feature = "deploy")]
#[async_trait]
trait SourceFetcher: Send + Sync {
    /// Fetch raw bytes from `url`. `label` is used only to build error messages.
    async fn fetch(&self, url: &str, label: &str) -> Result<Vec<u8>>;
}

/// Production fetcher: downloads the tarball from GitHub over HTTP.
#[cfg(feature = "deploy")]
struct HttpFetcher;

#[cfg(feature = "deploy")]
#[async_trait]
impl SourceFetcher for HttpFetcher {
    async fn fetch(&self, url: &str, label: &str) -> Result<Vec<u8>> {
        let client = reqwest::Client::builder()
            .user_agent("dbd-rs")
            .build()
            .map_err(|e| DbdError::GitHubSource(format!("HTTP client error: {e}")))?;

        let response = client
            .get(url)
            .send()
            .await
            .map_err(|e| DbdError::GitHubSource(format!("Failed to fetch {label}: {e}")))?;

        if !response.status().is_success() {
            return Err(DbdError::GitHubSource(format!(
                "GitHub returned {} for {label}",
                response.status()
            )));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|e| DbdError::GitHubSource(format!("Failed to read response: {e}")))?;

        Ok(bytes.to_vec())
    }
}

#[cfg(feature = "deploy")]
async fn download_and_extract(
    gh: &github::GitHubSource,
    cache_dir: &Path,
    fetcher: &dyn SourceFetcher,
) -> Result<()> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/tarball/{}",
        gh.owner, gh.repo, gh.git_ref
    );

    let bytes = fetcher.fetch(&url, &gh.label()).await?;
    extract_tarball(&bytes, cache_dir)
}

/// Extract a GitHub tarball (`.tar.gz` bytes) into `cache_dir`.
///
/// Pure function — no network or global state — so it's unit-testable with
/// an in-memory tarball built via the `tar`/`flate2` crates.
///
/// GitHub tarballs have a top-level directory like "owner-repo-sha/"; that
/// prefix is stripped when extracting.
#[cfg(feature = "deploy")]
fn extract_tarball(bytes: &[u8], cache_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(cache_dir)?;
    let decoder = flate2::read::GzDecoder::new(bytes);
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

    #[test]
    fn resolve_subpath_with_empty_string_is_base() {
        // An empty subpath should behave like `None` — joining "" onto a path
        // must not change which directory it refers to.
        let base = PathBuf::from("/cache/owner-repo-HEAD");
        assert_eq!(resolve_subpath(&base, Some("")), base);
    }

    #[test]
    fn resolve_subpath_with_nested_segments() {
        let base = PathBuf::from("/cache/owner-repo-HEAD");
        assert_eq!(
            resolve_subpath(&base, Some("src/db/schema")),
            PathBuf::from("/cache/owner-repo-HEAD/src/db/schema")
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
    async fn resolve_local_path_ignores_no_cache() {
        // Doc contract: "Local sources ignore no_cache." Passing `true` must
        // not change behavior for a local directory — it should still
        // resolve directly without attempting any cache/download logic.
        let tmp = TempDir::new().unwrap();
        let result = resolve_source(tmp.path().to_str().unwrap(), true).await.unwrap();
        assert_eq!(result, tmp.path());
    }

    #[tokio::test]
    async fn resolve_local_path_current_dir_shorthand() {
        // "." is explicitly documented as a local-path indicator and always
        // exists, so it should resolve as-is rather than being treated as a
        // (invalid, single-segment) GitHub shorthand.
        let result = resolve_source(".", false).await.unwrap();
        assert_eq!(result, PathBuf::from("."));
    }

    #[tokio::test]
    async fn resolve_local_path_that_is_a_regular_file() {
        // resolve_source only checks existence, not directory-ness — a path
        // to an existing file is returned as-is, matching the documented
        // "local path ... exists on disk: return as-is" contract.
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("design.yaml");
        std::fs::write(&file_path, "project:\n  name: test\n").unwrap();

        let result = resolve_source(file_path.to_str().unwrap(), false).await.unwrap();
        assert_eq!(result, file_path);
    }

    #[tokio::test]
    async fn resolve_source_invalid_github_shorthand_propagates_parse_error() {
        // A two-segment string that isn't a local path is treated as GitHub
        // shorthand; if the repo segment has unsafe characters,
        // parse_github_source's error must propagate through resolve_source.
        let result = resolve_source("owner/repo;rm", false).await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("Invalid repo"),
            "expected an 'Invalid repo' error, got: {msg}"
        );
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

    // ── Tarball extraction (pure) + fetch injection ──────────────────────
    //
    // These tests exercise `extract_tarball` and `download_and_extract`
    // entirely in-memory / on a temp dir, without any network call. Only
    // `HttpFetcher::fetch` (the real `reqwest` GET) is left uncovered.

    /// Build a well-formed `.tar.gz` with a single top-level directory
    /// (as real GitHub tarballs have) containing `files`.
    #[cfg(feature = "deploy")]
    fn build_tarball(top_level_dir: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
        let enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut builder = tar::Builder::new(enc);

        for (rel_path, contents) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            let full_path = format!("{top_level_dir}/{rel_path}");
            builder.append_data(&mut header, &full_path, *contents).unwrap();
        }

        let enc = builder.into_inner().expect("write tarball to Vec");
        enc.finish().expect("gzip finish to Vec")
    }

    /// Build a `.tar.gz` with a single entry whose raw path bytes are
    /// written directly (bypassing `tar::Header::set_path`'s own `".."`
    /// rejection), simulating a hand-crafted/malicious tarball rather than
    /// one produced by this crate's own `tar::Builder` usage.
    #[cfg(feature = "deploy")]
    fn build_malicious_tarball(raw_entry_path: &str, contents: &[u8]) -> Vec<u8> {
        let enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut builder = tar::Builder::new(enc);

        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        let name_bytes = raw_entry_path.as_bytes();
        header.as_old_mut().name[..name_bytes.len()].copy_from_slice(name_bytes);
        header.set_cksum();
        builder.append(&header, contents).unwrap();

        let enc = builder.into_inner().expect("write tarball to Vec");
        enc.finish().expect("gzip finish to Vec")
    }

    #[cfg(feature = "deploy")]
    mod extraction {
        use super::*;

        #[test]
        fn extract_tarball_strips_top_level_dir() {
            let tmp = TempDir::new().unwrap();
            let dest = tmp.path().join("dest");
            let bytes = build_tarball(
                "owner-repo-abc123",
                &[("design.yaml", b"project:\n  name: test\n")],
            );

            extract_tarball(&bytes, &dest).unwrap();

            assert_eq!(
                std::fs::read_to_string(dest.join("design.yaml")).unwrap(),
                "project:\n  name: test\n"
            );
            // The tarball's own top-level directory must not appear in the output.
            assert!(!dest.join("owner-repo-abc123").exists());
        }

        #[test]
        fn extract_tarball_creates_nested_subpath_directories() {
            let tmp = TempDir::new().unwrap();
            let dest = tmp.path().join("dest");
            let bytes = build_tarball(
                "owner-repo-abc123",
                &[("database/design.yaml", b"project:\n  name: nested\n")],
            );

            extract_tarball(&bytes, &dest).unwrap();

            assert_eq!(
                std::fs::read_to_string(dest.join("database/design.yaml")).unwrap(),
                "project:\n  name: nested\n"
            );
        }

        #[test]
        fn extract_tarball_skips_top_level_dir_entry_and_creates_nested_dirs() {
            // Real GitHub tarballs include an explicit directory entry for
            // the top-level dir itself (e.g. "owner-repo-abc123/"), which
            // strips down to an empty path and must be skipped rather than
            // erroring. They also contain explicit directory entries for
            // subdirectories (distinct from a file path that merely
            // contains a slash), which must be created via `create_dir_all`.
            let tmp = TempDir::new().unwrap();
            let dest = tmp.path().join("dest");

            let enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            let mut builder = tar::Builder::new(enc);

            let mut top_header = tar::Header::new_gnu();
            top_header.set_entry_type(tar::EntryType::Directory);
            top_header.set_size(0);
            top_header.set_mode(0o755);
            top_header.set_path("owner-repo-abc123/").unwrap();
            top_header.set_cksum();
            builder.append(&top_header, std::io::empty()).unwrap();

            let mut dir_header = tar::Header::new_gnu();
            dir_header.set_entry_type(tar::EntryType::Directory);
            dir_header.set_size(0);
            dir_header.set_mode(0o755);
            dir_header.set_path("owner-repo-abc123/empty_dir/").unwrap();
            dir_header.set_cksum();
            builder.append(&dir_header, std::io::empty()).unwrap();

            let enc = builder.into_inner().expect("write tarball to Vec");
            let bytes = enc.finish().expect("gzip finish to Vec");

            extract_tarball(&bytes, &dest).unwrap();

            assert!(dest.exists(), "cache dir itself should still be created");
            assert!(
                dest.join("empty_dir").is_dir(),
                "explicit directory entries should be created"
            );
        }

        #[test]
        fn extract_tarball_traversal_guard_matches_current_behavior() {
            // KNOWN GAP, pre-existing and NOT introduced by this refactor:
            // `dest = cache_dir.join(stripped)` followed by
            // `dest.starts_with(cache_dir)` can never reject a relative
            // ".." escape on Unix. `Path::join(x)` only re-roots onto `x`
            // when `x` itself is absolute; otherwise it *always* appends
            // `x`'s components after `cache_dir`'s, so the result trivially
            // starts_with `cache_dir` regardless of ".." components inside
            // `x` — `Path::starts_with` is a lexical component-prefix
            // check, it does not resolve "..". And since `stripped` is
            // built by `.skip(1)`-ing a `Components` iterator, it can never
            // itself become absolute (`RootDir` can only ever be the first
            // component). So the `continue` guard branch is unreachable
            // dead code as currently written: this check cannot fire for
            // ANY input on Unix. (Independently confirmed by this session's
            // semgrep "Path Traversal" finding on this exact line.)
            //
            // This test is a characterization test: it locks in *today's*
            // observed behavior unchanged by the refactor, per the task's
            // instruction to keep the guard's logic verbatim. It is not an
            // assertion that the behavior is safe — a follow-up fix should
            // reject any `stripped` path containing a `Component::ParentDir`
            // (or canonicalize `dest`/`cache_dir` before comparing) instead
            // of relying on `starts_with` alone.
            let tmp = TempDir::new().unwrap();
            let dest = tmp.path().join("dest");
            let bytes = build_malicious_tarball("repo-main/../escaped.txt", b"pwned");

            extract_tarball(&bytes, &dest).unwrap();

            let escaped = tmp.path().join("escaped.txt");
            assert!(
                escaped.exists(),
                "documents current guard behavior: this entry is not rejected"
            );
            assert!(!dest.join("escaped.txt").exists());
        }
    }

    // ── Fetch injection (FakeFetcher, no network) ─────────────────────────

    #[cfg(feature = "deploy")]
    mod fetch_injection {
        use super::*;

        /// A fetcher stub that returns fixed bytes without any network call.
        struct FakeFetcher(Vec<u8>);

        #[async_trait]
        impl SourceFetcher for FakeFetcher {
            async fn fetch(&self, _url: &str, _label: &str) -> Result<Vec<u8>> {
                Ok(self.0.clone())
            }
        }

        /// A fetcher stub that always fails, to exercise error propagation.
        struct FailingFetcher;

        #[async_trait]
        impl SourceFetcher for FailingFetcher {
            async fn fetch(&self, _url: &str, _label: &str) -> Result<Vec<u8>> {
                Err(DbdError::GitHubSource("network is down".to_string()))
            }
        }

        #[tokio::test]
        async fn download_and_extract_with_fake_fetcher_extracts_files() {
            let tmp = TempDir::new().unwrap();
            let cache_dir = tmp.path().join("cache");
            let bytes = build_tarball(
                "sensei-hq-daemon-abc123",
                &[("design.yaml", b"project:\n  name: fake-fetch\n")],
            );
            let gh = github::parse_github_source("sensei-hq/daemon").unwrap();
            let fetcher = FakeFetcher(bytes);

            download_and_extract(&gh, &cache_dir, &fetcher).await.unwrap();

            assert_eq!(
                std::fs::read_to_string(cache_dir.join("design.yaml")).unwrap(),
                "project:\n  name: fake-fetch\n"
            );
        }

        #[tokio::test]
        async fn download_and_extract_propagates_fetcher_error() {
            let tmp = TempDir::new().unwrap();
            let cache_dir = tmp.path().join("cache");
            let gh = github::parse_github_source("sensei-hq/daemon").unwrap();

            let result = download_and_extract(&gh, &cache_dir, &FailingFetcher).await;

            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("network is down"));
        }
    }
}
