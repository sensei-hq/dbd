use std::path::{Path, PathBuf};

use crate::error::{DbdError, Result};

/// Parsed GitHub source components.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubSource {
    pub owner: String,
    pub repo: String,
    pub subpath: Option<String>,
    pub git_ref: String,
}

impl GitHubSource {
    pub fn label(&self) -> String {
        let base = match &self.subpath {
            Some(p) => format!("{}/{}/{}", self.owner, self.repo, p),
            None => format!("{}/{}", self.owner, self.repo),
        };
        if self.git_ref != "HEAD" {
            format!("{} ({})", base, self.git_ref)
        } else {
            base
        }
    }
}

// Only allow safe characters in GitHub path segments
fn is_safe_segment(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

fn is_safe_subpath(s: &str) -> bool {
    !s.contains("..") && s.chars().all(|c| c.is_ascii_alphanumeric() || "._-/".contains(c))
}

/// Parse a GitHub source string.
///
/// Formats:
///   owner/repo
///   owner/repo/subpath
///   owner/repo@ref
///   owner/repo/subpath@ref
///   https://github.com/owner/repo
///   https://github.com/owner/repo/tree/branch/subpath
pub fn parse_github_source(source: &str) -> Result<GitHubSource> {
    // Full URL format
    if let Some(rest) = source
        .strip_prefix("https://github.com/")
        .or_else(|| source.strip_prefix("http://github.com/"))
    {
        let rest = rest.trim_end_matches(".git");
        let parts: Vec<&str> = rest.splitn(4, '/').collect();
        if parts.len() < 2 {
            return Err(DbdError::GitHubSource(format!(
                "Invalid GitHub URL: \"{source}\""
            )));
        }
        let owner = parts[0].to_string();
        let repo = parts[1].to_string();
        let (git_ref, subpath) = if parts.len() >= 3 && parts[2] == "tree" {
            let remainder = parts.get(3).unwrap_or(&"");
            let tree_parts: Vec<&str> = remainder.splitn(2, '/').collect();
            let r = tree_parts[0].to_string();
            let s = tree_parts.get(1).map(|p| p.to_string());
            (if r.is_empty() { "HEAD".to_string() } else { r }, s)
        } else {
            ("HEAD".to_string(), None)
        };

        validate_segments(&owner, &repo, &git_ref, subpath.as_deref())?;
        return Ok(GitHubSource {
            owner,
            repo,
            subpath,
            git_ref,
        });
    }

    // Shorthand format: owner/repo[/subpath][@ref]
    let mut git_ref = "HEAD".to_string();
    let mut path_str = source.to_string();

    if let Some(at_idx) = source.rfind('@') {
        if at_idx > 0 {
            git_ref = source[at_idx + 1..].to_string();
            path_str = source[..at_idx].to_string();
        }
    }

    let parts: Vec<&str> = path_str.split('/').collect();
    if parts.len() < 2 {
        return Err(DbdError::GitHubSource(format!(
            "Invalid GitHub source: \"{source}\""
        )));
    }

    let owner = parts[0].to_string();
    let repo = parts[1].to_string();
    let subpath = if parts.len() > 2 {
        Some(parts[2..].join("/"))
    } else {
        None
    };

    validate_segments(&owner, &repo, &git_ref, subpath.as_deref())?;

    Ok(GitHubSource {
        owner,
        repo,
        subpath,
        git_ref,
    })
}

fn validate_segments(owner: &str, repo: &str, git_ref: &str, subpath: Option<&str>) -> Result<()> {
    if !is_safe_segment(owner) {
        return Err(DbdError::GitHubSource(format!(
            "Invalid owner: \"{owner}\""
        )));
    }
    if !is_safe_segment(repo) {
        return Err(DbdError::GitHubSource(format!("Invalid repo: \"{repo}\"")));
    }
    if !is_safe_segment(git_ref) {
        return Err(DbdError::GitHubSource(format!("Invalid ref: \"{git_ref}\"")));
    }
    if let Some(sp) = subpath {
        if !is_safe_subpath(sp) {
            return Err(DbdError::GitHubSource(format!(
                "Invalid subpath: \"{sp}\" — path traversal is not allowed"
            )));
        }
    }
    Ok(())
}

/// Check if a source string looks like a GitHub reference rather than a local path.
pub fn is_github_source(source: &str) -> bool {
    if source.is_empty() || source == "." {
        return false;
    }
    if source.starts_with("https://github.com/") || source.starts_with("http://github.com/") {
        return true;
    }
    if source.starts_with('/') || source.starts_with('.') {
        return false;
    }
    if Path::new(source).exists() {
        return false;
    }
    source.split('/').count() >= 2
}

/// Cache directory for GitHub downloads.
pub fn cache_dir(owner: &str, repo: &str, git_ref: &str) -> PathBuf {
    let base = dirs::cache_dir().unwrap_or_else(|| PathBuf::from(".cache"));
    base.join("dbd").join(format!("{}-{}-{}", owner, repo, git_ref))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_owner_repo() {
        let result = parse_github_source("sensei-hq/daemon").unwrap();
        assert_eq!(
            result,
            GitHubSource {
                owner: "sensei-hq".to_string(),
                repo: "daemon".to_string(),
                subpath: None,
                git_ref: "HEAD".to_string(),
            }
        );
    }

    #[test]
    fn parse_owner_repo_subpath() {
        let result = parse_github_source("sensei-hq/daemon/database").unwrap();
        assert_eq!(result.subpath, Some("database".to_string()));
        assert_eq!(result.git_ref, "HEAD");
    }

    #[test]
    fn parse_deep_subpath() {
        let result = parse_github_source("sensei-hq/daemon/src/db/schema").unwrap();
        assert_eq!(result.subpath, Some("src/db/schema".to_string()));
    }

    #[test]
    fn parse_with_ref() {
        let result = parse_github_source("sensei-hq/daemon@v2.1").unwrap();
        assert_eq!(result.git_ref, "v2.1");
        assert_eq!(result.subpath, None);
    }

    #[test]
    fn parse_subpath_with_ref() {
        let result = parse_github_source("sensei-hq/daemon/database@main").unwrap();
        assert_eq!(result.subpath, Some("database".to_string()));
        assert_eq!(result.git_ref, "main");
    }

    #[test]
    fn parse_full_url() {
        let result =
            parse_github_source("https://github.com/sensei-hq/daemon").unwrap();
        assert_eq!(result.owner, "sensei-hq");
        assert_eq!(result.repo, "daemon");
        assert_eq!(result.git_ref, "HEAD");
    }

    #[test]
    fn parse_full_url_with_tree() {
        let result = parse_github_source(
            "https://github.com/sensei-hq/daemon/tree/main/database",
        )
        .unwrap();
        assert_eq!(result.owner, "sensei-hq");
        assert_eq!(result.repo, "daemon");
        assert_eq!(result.git_ref, "main");
        assert_eq!(result.subpath, Some("database".to_string()));
    }

    #[test]
    fn parse_url_with_git_suffix() {
        let result =
            parse_github_source("https://github.com/sensei-hq/daemon.git").unwrap();
        assert_eq!(result.repo, "daemon");
    }

    #[test]
    fn rejects_single_segment() {
        assert!(parse_github_source("daemon").is_err());
    }

    #[test]
    fn rejects_traversal_in_owner() {
        assert!(parse_github_source("../evil/repo").is_err());
    }

    #[test]
    fn rejects_traversal_in_subpath() {
        assert!(parse_github_source("owner/repo/../../etc/passwd").is_err());
    }

    #[test]
    fn rejects_special_chars_in_repo() {
        assert!(parse_github_source("owner/repo;rm/path").is_err());
    }

    #[test]
    fn is_github_source_detects_urls() {
        assert!(is_github_source("https://github.com/owner/repo"));
    }

    #[test]
    fn is_github_source_detects_shorthand() {
        assert!(is_github_source("sensei-hq/daemon/database"));
    }

    #[test]
    fn is_github_source_rejects_local() {
        assert!(!is_github_source("."));
        assert!(!is_github_source(""));
        assert!(!is_github_source("/usr/local"));
        assert!(!is_github_source("./my-project"));
        assert!(!is_github_source("daemon")); // Single segment
    }

    #[test]
    fn label_format() {
        let src = parse_github_source("sensei-hq/daemon/database@v2.1").unwrap();
        assert_eq!(src.label(), "sensei-hq/daemon/database (v2.1)");

        let src2 = parse_github_source("sensei-hq/daemon").unwrap();
        assert_eq!(src2.label(), "sensei-hq/daemon");
    }

    #[test]
    fn cache_dir_deterministic() {
        let a = cache_dir("owner", "repo", "main");
        let b = cache_dir("owner", "repo", "main");
        assert_eq!(a, b);
        assert!(a.ends_with("owner-repo-main"));
    }
}
