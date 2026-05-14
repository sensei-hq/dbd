pub mod adapter;
pub mod config;
pub mod dbml;
pub mod dependency;
pub mod diff;
pub mod doctor;
pub mod design;
pub mod entity;
pub mod error;
pub mod deploy;
pub mod formatter;
pub mod github;
pub mod init;
pub mod parser;
pub mod references;
pub mod scanner;
pub mod script;
pub mod snapshot;

pub use adapter::DatabaseAdapter;
pub use design::{ApplyComplete, ApplyStrategy, DeployComplete, Design, ImportComplete};
pub use entity::{Entity, EntityType};
pub use error::{DbdError, Result};
pub use snapshot::DataSqlTodo;

/// Connect to a database by URL and return an adapter.
///
/// The URL scheme determines the adapter:
/// - `postgres://` / `postgresql://` → PostgreSQL adapter
///
/// ```no_run
/// # async fn example() -> dbd_core::Result<()> {
/// let adapter = dbd_core::connect("postgres://localhost/mydb", "myproject").await?;
/// # Ok(())
/// # }
/// ```
pub async fn connect(url: &str, project: &str) -> Result<Box<dyn DatabaseAdapter>> {
    let adapter = adapter::postgres::PostgresAdapter::new(url, project).await?;
    Ok(Box::new(adapter))
}
