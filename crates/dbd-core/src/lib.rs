pub mod adapter;
pub mod config;
pub mod dbml;
pub mod dbml_parse;
pub mod dependency;
pub mod diagram;
pub mod diff;
pub mod doctor;
pub mod design;
pub mod emit;
pub mod entity;
pub mod error;
pub mod deploy;
pub mod formatter;
pub mod github;
pub mod init;
pub mod parser;
pub mod reconcile;
pub mod references;
pub mod refcache;
pub mod scanner;
pub mod schema_diff;
pub mod schema_model;
pub mod scope;
pub mod reverse;
pub mod script;
pub mod snapshot;

pub use adapter::DatabaseAdapter;
pub use design::{ApplyComplete, ApplyStrategy, DeployComplete, Design, ImportComplete};
pub use entity::{Entity, EntityType};
pub use reconcile::{ReconcileComplete, ReconcilePlan};
pub use schema_diff::SchemaDiff;
pub use error::{DbdError, Result};
pub use schema_model::SchemaModel;
pub use scope::{ResolvedScope, ScopeGap};
pub use snapshot::DataSqlTodo;

/// Connect to a database by URL and return an adapter.
///
/// The URL scheme determines the adapter:
/// - `postgres://` / `postgresql://` → PostgreSQL adapter
/// - `sqlite://` / `sqlite::memory:` / `file:` → SQLite adapter
/// - `convex:` / `convex://<dir>` → Convex codegen adapter (writes `<dir>/schema.ts`)
///
/// ```no_run
/// # async fn example() -> dbd_core::Result<()> {
/// let adapter = dbd_core::connect("postgres://localhost/mydb", "myproject").await?;
/// # Ok(())
/// # }
/// ```
pub async fn connect(url: &str, project: &str) -> Result<Box<dyn DatabaseAdapter>> {
    if url.starts_with("convex:") {
        let adapter = adapter::convex::ConvexAdapter::from_url(url, project)?;
        return Ok(Box::new(adapter));
    }
    #[cfg(feature = "sqlite")]
    if url.starts_with("sqlite:") || url.starts_with("file:") {
        let adapter = adapter::sqlite::SqliteAdapter::new(url, project).await?;
        return Ok(Box::new(adapter));
    }
    #[cfg(feature = "postgres")]
    {
        let adapter = adapter::postgres::PostgresAdapter::new(url, project).await?;
        Ok(Box::new(adapter))
    }
    #[cfg(not(feature = "postgres"))]
    Err(DbdError::Config(format!(
        "No adapter compiled in for URL: {url}"
    )))
}

#[cfg(test)]
mod tests {
    /// The `convex:` scheme routes to the codegen adapter (no server needed).
    #[tokio::test]
    async fn connect_convex_scheme() {
        let tmp = tempfile::tempdir().unwrap();
        let url = format!("convex://{}", tmp.path().display());
        assert!(super::connect(&url, "proj").await.is_ok());
    }

    /// The `sqlite:` scheme routes to the in-memory SQLite adapter.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn connect_sqlite_in_memory() {
        assert!(super::connect("sqlite::memory:", "proj").await.is_ok());
    }
}
