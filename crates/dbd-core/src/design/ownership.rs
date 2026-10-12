use super::*;
use crate::adapter::{HeldObject, OwnedObject};

/// What a run does with the database's ownership registry before it runs
/// anything (#7): a run that writes claims, one that only reads checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Preflight {
    /// `apply`, `deploy`, `reconcile`, `reset`: heal the bookkeeping, then
    /// claim every entity the design declares.
    Claim,
    /// A dry run, `diff`: the same refusal, nothing written.
    Check,
}

impl Design {
    /// The owner this design's runs claim as: its project's name.
    pub(crate) fn owner(&self) -> &str {
        &self.config.project.name
    }

    /// Every entity the whole design declares, keyed as the registry keys it —
    /// the whole design, never a scope's subset: a scope narrows what a run
    /// applies, not what the project owns (#40). What every owner shares —
    /// schemas, extensions, roles — is not an entity anyone owns.
    pub(crate) fn declared_objects(&self) -> Vec<OwnedObject> {
        let declared: std::collections::BTreeSet<OwnedObject> = self
            .entities
            .iter()
            .filter(|e| e.errors.is_empty())
            .filter_map(|e| {
                let (namespace, kind) = match e.entity_type {
                    EntityType::Table => ("relation", "table"),
                    EntityType::View => ("relation", "view"),
                    EntityType::MaterializedView => ("relation", "materialized view"),
                    EntityType::Sequence => ("relation", "sequence"),
                    EntityType::Enum => ("type", "enum"),
                    EntityType::Function => ("routine", "function"),
                    EntityType::Procedure => ("routine", "procedure"),
                    _ => return None,
                };
                let (_, name) = crate::entity::split_qualified_name(&e.name);
                Some(OwnedObject {
                    namespace: namespace.to_string(),
                    schema: e
                        .schema
                        .clone()
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| crate::reconcile::DEFAULT_SCHEMA.to_string()),
                    name,
                    kind: kind.to_string(),
                })
            })
            .collect();
        // A routine is owned by name, so its overloads are one entity.
        let mut seen = std::collections::HashSet::new();
        declared
            .into_iter()
            .filter(|o| seen.insert((o.namespace.clone(), o.schema.clone(), o.name.clone())))
            .collect()
    }

    /// Refuse, naming each, a design declaring an entity another owner holds
    /// in this database — the check every command that claims nothing runs
    /// before it touches the database: `refresh`, `policies`, `import`, a dry
    /// run, `diff`. Reads the ownership registry and writes nothing (#7).
    pub async fn check_ownership(&self, adapter: &dyn DatabaseAdapter) -> Result<()> {
        self.preflight(adapter, Preflight::Check).await
    }

    /// Claim the design's entities before a run writes anything — or, for one
    /// that only reads, check them — and refuse, naming each, when another
    /// owner holds any: owners sharing a database must not declare the same
    /// entity (#7). A claim heals the bookkeeping first, so the registry is
    /// there to write; a check writes nothing.
    pub(crate) async fn preflight(&self, adapter: &dyn DatabaseAdapter, mode: Preflight) -> Result<()> {
        if mode == Preflight::Claim {
            adapter.heal_bookkeeping().await?;
        }
        let held = adapter
            .claim_objects(self.owner(), &self.declared_objects(), mode == Preflight::Claim)
            .await?;
        if held.is_empty() {
            return Ok(());
        }
        Err(DbdError::Config(held_refusal(&held)))
    }
}

/// `relation app.orders is owned by "shop"; …; owners sharing a database must
/// not declare the same entity` — the first ten, and how many more.
fn held_refusal(held: &[HeldObject]) -> String {
    const SHOWN: usize = 10;
    let mut parts: Vec<String> = held
        .iter()
        .take(SHOWN)
        .map(|h| {
            format!(
                "{} {}.{} is owned by \"{}\"",
                h.object.namespace, h.object.schema, h.object.name, h.owner
            )
        })
        .collect();
    if held.len() > SHOWN {
        parts.push(format!("and {} more", held.len() - SHOWN));
    }
    format!(
        "{}; owners sharing a database must not declare the same entity",
        parts.join("; ")
    )
}
