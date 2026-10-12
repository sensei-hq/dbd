use super::*;
use crate::adapter::{HeldObject, OwnedObject, Use};
use crate::entity::RefKind;

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
        self.declared_entities().into_iter().map(|(_, o)| o).collect()
    }

    /// Each entity the design declares, with its registry key, one per key.
    ///
    /// Every one is claimed, read or not: a file dbd cannot read still declares
    /// its entity by its path, and a reset still drops it. The key is the
    /// object Postgres makes, which the path need not spell: the first
    /// `CREATE` of the entity's kind gives the name, folded as Postgres folds an
    /// unquoted one, and the schema it states — else the search path the file
    /// runs on, its own `set search_path` or `source.search_path`. Only where
    /// no such statement can be read — a file that does not parse, an enum
    /// declared inside a `DO` block, another dialect — does the path name it.
    ///
    /// Tables, views, materialized views, sequences and enums share one
    /// namespace: Postgres gives a table a row type, so a table and an enum of
    /// one name cannot both exist. Routines are owned by name, their overloads
    /// one entity.
    pub(crate) fn declared_entities(&self) -> Vec<(&Entity, OwnedObject)> {
        let postgres = self.config.source.dialect.eq_ignore_ascii_case("postgresql")
            || self.config.source.dialect.eq_ignore_ascii_case("postgres");
        let mut seen = std::collections::HashSet::new();
        let mut declared: Vec<(&Entity, OwnedObject)> = self
            .entities
            .iter()
            .filter_map(|e| {
                let (namespace, kind) = match e.entity_type {
                    EntityType::Table => ("relation", "table"),
                    EntityType::View => ("relation", "view"),
                    EntityType::MaterializedView => ("relation", "materialized view"),
                    EntityType::Sequence => ("relation", "sequence"),
                    EntityType::Enum => ("relation", "enum"),
                    EntityType::Function => ("routine", "function"),
                    EntityType::Procedure => ("routine", "procedure"),
                    _ => return None,
                };
                let created = postgres
                    .then_some(e.file.as_ref())
                    .flatten()
                    .and_then(|file| crate::source_text::read_to_string(&self.project_dir.join(file)).ok())
                    .and_then(|sql| crate::parser::pg::created_identity(e.entity_type, &sql));
                let on_path = || e.schema_path.default_schema().map(str::to_string);
                let (schema, name) = match created {
                    Some((stated, name)) => (stated.or_else(on_path), name),
                    None => (
                        e.schema.clone().filter(|s| !s.is_empty()).or_else(on_path),
                        crate::entity::split_qualified_name(&e.name).1,
                    ),
                };
                Some((
                    e,
                    OwnedObject {
                        namespace: namespace.to_string(),
                        schema: schema.unwrap_or_else(|| crate::reconcile::DEFAULT_SCHEMA.to_string()),
                        name,
                        kind: kind.to_string(),
                    },
                ))
            })
            .collect();
        declared.sort_by(|a, b| a.1.cmp(&b.1));
        declared.retain(|(_, o)| seen.insert((o.namespace.clone(), o.schema.clone(), o.name.clone())));
        declared
    }

    /// Refuse, naming each, a design declaring an entity another owner holds
    /// in this database — the check every command that claims nothing runs
    /// before it touches the database: `refresh`, `policies`, `import`, a dry
    /// run, `diff`. Reads the ownership registry and writes nothing (#7).
    pub async fn check_ownership(&self, adapter: &dyn DatabaseAdapter) -> Result<()> {
        self.preflight(adapter, Preflight::Check).await.map(|_| ())
    }

    /// Everything the design uses without owning it (#7): the schemas its
    /// entities live in and the ones it declares, its extensions and roles —
    /// shared with any other owner — and each reference one of its entities
    /// makes to an entity it does not declare, which the registry records only
    /// where another owner holds the target. `via` names the referring entity.
    pub(crate) fn declared_uses(&self) -> Vec<Use> {
        let declared = self.declared_entities();
        let owned_names: std::collections::HashSet<String> = declared
            .iter()
            .map(|(_, o)| format!("{}.{}", o.schema, o.name))
            .collect();
        let shared = |kind: &str, target: &str| Use {
            kind: kind.to_string(),
            target: target.to_string(),
            via: String::new(),
        };
        let mut uses: std::collections::BTreeSet<Use> =
            declared.iter().map(|(_, o)| shared("schema", &o.schema)).collect();
        for e in self.entities.iter().filter(|e| e.errors.is_empty()) {
            match e.entity_type {
                EntityType::Schema => {
                    uses.insert(shared("schema", &e.name));
                }
                EntityType::Extension => {
                    uses.insert(shared("extension", &e.name));
                }
                EntityType::Role => {
                    uses.insert(shared("role", &e.name));
                }
                _ => {}
            }
        }
        for (e, o) in &declared {
            for r in e.refs.iter().filter(|r| {
                matches!(
                    r.kind,
                    RefKind::Reads | RefKind::Writes | RefKind::Calls | RefKind::Uses
                )
            }) {
                let target = match crate::entity::split_qualified_name(&r.name) {
                    (Some(schema), name) => format!("{schema}.{name}"),
                    (None, name) => format!("{}.{name}", crate::reconcile::DEFAULT_SCHEMA),
                };
                if !owned_names.contains(&target) {
                    uses.insert(Use {
                        kind: "entity".to_string(),
                        target,
                        via: format!("{} {}.{}", o.kind, o.schema, o.name),
                    });
                }
            }
        }
        uses.into_iter().collect()
    }

    /// Claim the design's entities before a run writes anything — or, for one
    /// that only reads, check them — and refuse, naming each, when another
    /// owner holds any: owners sharing a database must not declare the same
    /// entity (#7). A claim heals the bookkeeping first, so the registry is
    /// there to write; a check writes nothing.
    ///
    /// Returns what a claim newly took — for [`Self::give_back`] if the run
    /// then fails.
    pub(crate) async fn preflight(&self, adapter: &dyn DatabaseAdapter, mode: Preflight) -> Result<Vec<OwnedObject>> {
        if mode == Preflight::Claim {
            adapter.heal_bookkeeping().await?;
        }
        let claim = adapter
            .claim_objects(self.owner(), &self.declared_objects(), mode == Preflight::Claim)
            .await?;
        if !claim.held.is_empty() {
            return Err(DbdError::Config(held_refusal(&claim.held)));
        }
        if mode == Preflight::Claim
            && let Err(e) = adapter.record_uses(self.owner(), &self.declared_uses()).await
        {
            return Err(self.give_back(adapter, &claim.claimed, e).await);
        }
        Ok(claim.claimed)
    }

    /// `failure`, after giving back what this run newly `claimed`: a run that
    /// fails after its claim must not keep entities it never completed, or
    /// their real author is barred from them. A give-back that itself fails is
    /// reported with the failure, not instead of it.
    pub(crate) async fn give_back(
        &self,
        adapter: &dyn DatabaseAdapter,
        claimed: &[OwnedObject],
        failure: DbdError,
    ) -> DbdError {
        if claimed.is_empty() {
            return failure;
        }
        match adapter.release_objects(self.owner(), claimed).await {
            Ok(()) => failure,
            Err(e) => DbdError::Config(format!("{failure}; and giving back its claims failed: {e}")),
        }
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
