use super::*;
use crate::adapter::{HeldObject, OwnedObject, Use};
use crate::entity::RefKind;
use crate::parser::pg::Created;

/// What a run does with the database's ownership registry before it does
/// anything else (#7). Both refuse a design declaring an entity another owner
/// holds; neither claims — see [`Design::claim_built`] and
/// [`Design::claim_first`] for when a run does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Preflight {
    /// `apply` and `deploy`: heal the bookkeeping, then check.
    Write,
    /// A dry run, `diff`, `import`, `refresh`, `policies`, and `reconcile`
    /// before it plans: check, writing nothing.
    Read,
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
        let mut seen = std::collections::HashSet::new();
        let mut objects: Vec<OwnedObject> = self.entity_keys().into_iter().flat_map(|(_, keys)| keys).collect();
        objects.sort();
        objects.retain(|o| seen.insert((o.namespace.clone(), o.schema.clone(), o.name.clone())));
        objects
    }

    /// Each entity the design declares, with the registry keys of what it
    /// creates — the path's entity first.
    ///
    /// Every one is claimed, read or not: a file dbd cannot read still declares
    /// its entity by its path, and a reset still drops it. A key is the object
    /// Postgres makes, which the path need not spell: each `CREATE` in the
    /// entity's file gives a name, folded as Postgres folds an unquoted one, and
    /// the schema it states — else the search path the file runs on, its own
    /// `set search_path` or `source.search_path`. A file may create more than
    /// its path names — another table, the type a function returns — and
    /// every one is claimed. Only where no statement can be read — a file that
    /// does not parse, an enum declared inside a `DO` block, a dialect dbd does
    /// not read as Postgres — does the path name the entity, folded.
    ///
    /// Everything Postgres names as a relation or a type shares one namespace:
    /// it gives a table, a view and a materialized view a row type, so none of
    /// them can share a name with an enum, a composite, a domain or a range.
    /// Routines are owned by name, their overloads one entity.
    fn entity_keys(&self) -> Vec<(&Entity, Vec<OwnedObject>)> {
        // Read as the loader reads the design: `supabase`, and anything else
        // libpg_query parses, is Postgres.
        let postgres =
            crate::parser::ParserChoice::resolve(&self.config.source.dialect, self.config.source.parser.as_deref())
                .is_ok_and(|parser| parser == crate::parser::ParserChoice::PgQuery);
        self.entities
            .iter()
            .filter(|e| owned_kind(e.entity_type).is_some())
            .map(|e| {
                let on_path = || e.schema_path.default_schema().map(str::to_string);
                let key = |created: Created, schema: Option<String>, name: String| {
                    let (namespace, kind) = match created {
                        Created::Entity(entity_type) => owned_kind(entity_type)?,
                        Created::Type(kind) => ("relation", kind),
                    };
                    Some(OwnedObject {
                        namespace: namespace.to_string(),
                        schema: schema.unwrap_or_else(|| crate::reconcile::DEFAULT_SCHEMA.to_string()),
                        name,
                        kind: kind.to_string(),
                    })
                };
                let path_name = crate::entity::split_qualified_name(&e.name).1;
                let created: Vec<(Created, Option<String>, String)> = postgres
                    .then_some(e.file.as_ref())
                    .flatten()
                    .and_then(|file| crate::source_text::read_to_string(&self.project_dir.join(file)).ok())
                    .map(|sql| crate::parser::pg::created_identities(&sql))
                    .unwrap_or_default();
                let mut keys: Vec<OwnedObject> = created
                    .into_iter()
                    .filter_map(|(created, stated, name)| key(created, stated.or_else(on_path), name))
                    .collect();
                if keys.is_empty() {
                    // A path cannot quote a name, so it names what Postgres
                    // makes of an unquoted one: folded.
                    keys.extend(key(
                        Created::Entity(e.entity_type),
                        e.schema
                            .clone()
                            .filter(|s| !s.is_empty())
                            .map(|s| s.to_ascii_lowercase())
                            .or_else(on_path),
                        path_name.to_ascii_lowercase(),
                    ));
                }
                // The path's entity first, by namespace and name: it is the one
                // a reference is made through.
                let path_namespace = owned_kind(e.entity_type).map(|(namespace, _)| namespace);
                if let Some(i) = keys.iter().position(|k| {
                    Some(k.namespace.as_str()) == path_namespace && k.name.eq_ignore_ascii_case(&path_name)
                }) {
                    keys.swap(0, i);
                }
                (e, keys)
            })
            .collect()
    }

    /// Refuse, naming each, a design declaring an entity another owner holds
    /// in this database — the check every command that claims nothing runs
    /// before it touches the database: `refresh`, `policies`, `import`, a dry
    /// run, `diff`. Reads the ownership registry and writes nothing (#7).
    pub async fn check_ownership(&self, adapter: &dyn DatabaseAdapter) -> Result<()> {
        self.preflight(adapter, Preflight::Read).await
    }

    /// Everything the design uses without owning it (#7): the schemas its
    /// entities live in and the ones it declares, its extensions and roles —
    /// shared with any other owner — and each reference any of its entities
    /// makes to an entity it does not declare, which the registry records only
    /// where another owner holds the target. A call is of a routine, anything
    /// else of a relation. `via` names the referring entity; overloads in
    /// several files each record their own references. A name written bare
    /// that dbd could not find carries the search path Postgres looks it up
    /// along.
    pub(crate) fn declared_uses(&self) -> Vec<Use> {
        let keyed = self.entity_keys();
        let owned: std::collections::HashSet<String> = keyed
            .iter()
            .flat_map(|(_, keys)| keys.iter().map(|o| format!("{}:{}.{}", o.namespace, o.schema, o.name)))
            .collect();
        let shared = |kind: &str, target: &str| Use {
            kind: kind.to_string(),
            target: target.to_string(),
            via: String::new(),
            search_path: Vec::new(),
        };
        let mut uses: std::collections::BTreeSet<Use> = keyed
            .iter()
            .flat_map(|(_, keys)| keys.iter().map(|o| shared("schema", &o.schema)))
            .collect();
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
        for (e, keys) in &keyed {
            let Some(via) = keys.first() else {
                continue;
            };
            // As the resolver walks it: the named schemas, then `public`, which
            // dbd keeps on every path it applies.
            let mut search_path: Vec<String> = e.schema_path.schemas().map(str::to_string).collect();
            if !search_path.iter().any(|s| s == crate::reconcile::DEFAULT_SCHEMA) {
                search_path.push(crate::reconcile::DEFAULT_SCHEMA.to_string());
            }
            for r in e.refs.iter().filter(|r| {
                matches!(
                    r.kind,
                    RefKind::Reads | RefKind::Writes | RefKind::Calls | RefKind::Uses
                )
            }) {
                let namespace = if r.kind == RefKind::Calls {
                    "routine"
                } else {
                    "relation"
                };
                let target = match crate::entity::split_qualified_name(&r.name) {
                    (Some(schema), name) => format!("{namespace}:{schema}.{name}"),
                    (None, name) => format!("{namespace}:{}.{name}", crate::reconcile::DEFAULT_SCHEMA),
                };
                if !owned.contains(&target) {
                    uses.insert(Use {
                        kind: "entity".to_string(),
                        target,
                        via: format!("{} {}.{}", via.kind, via.schema, via.name),
                        search_path: if r.unresolved && r.schema_source.is_guess() {
                            search_path.clone()
                        } else {
                            Vec::new()
                        },
                    });
                }
            }
        }
        uses.into_iter().collect()
    }

    /// Before a run does anything: refuse, naming each, a design declaring an
    /// entity another owner holds — owners sharing a database must not declare
    /// the same entity (#7). A run that writes heals the bookkeeping first.
    /// Nothing is claimed here: see [`Self::claim_built`].
    pub(crate) async fn preflight(&self, adapter: &dyn DatabaseAdapter, mode: Preflight) -> Result<()> {
        if mode == Preflight::Write {
            adapter.heal_bookkeeping().await?;
        }
        let held = adapter
            .claim_objects(self.owner(), &self.declared_objects(), false)
            .await?;
        if !held.is_empty() {
            return Err(DbdError::Config(held_refusal(&held)));
        }
        Ok(())
    }

    /// Claim the design's entities and record what it uses, in the run's own
    /// transaction, as the last of its work — `apply`'s batch, before it
    /// commits — so the claim commits with the work or not at all. A run that
    /// fails, or is killed, before this claims nothing; two runs of one
    /// project at once claim the same rows. A project that claimed an entity
    /// since [`Self::preflight`] checked is refused here, and the run's work
    /// rolls back with the claim (#7).
    pub(crate) async fn claim_built(&self, adapter: &dyn DatabaseAdapter) -> Result<()> {
        self.claim(adapter).await?;
        self.record_declared_uses(adapter).await
    }

    /// Heal the bookkeeping and claim the design's entities, committed, before
    /// a run's first write — for a run whose writes commit as they go:
    /// `reconcile`, `reset`, an `apply` outside one transaction. Claimed after
    /// its writes, such a run found an entity unowned, waited on a project
    /// building it, and wrote over — or dropped — what that project had just
    /// claimed. Claimed first, whichever run claims first owns it, and the
    /// other is refused before it writes. A run that then fails keeps its
    /// claims: what its design declares is its own, built yet or not (#7).
    pub(crate) async fn claim_first(&self, adapter: &dyn DatabaseAdapter) -> Result<()> {
        adapter.heal_bookkeeping().await?;
        self.claim(adapter).await
    }

    /// Claim the design's entities — in the open batch when there is one —
    /// refusing, naming each, those another owner holds.
    pub(crate) async fn claim(&self, adapter: &dyn DatabaseAdapter) -> Result<()> {
        let held = adapter
            .claim_objects(self.owner(), &self.declared_objects(), true)
            .await?;
        if !held.is_empty() {
            return Err(DbdError::Config(held_refusal(&held)));
        }
        Ok(())
    }

    /// Record what the design uses, replacing what it used before — in the
    /// open batch when there is one. Written once the work is built: the uses
    /// describe what is, so a run that fails keeps the record it had.
    pub(crate) async fn record_declared_uses(&self, adapter: &dyn DatabaseAdapter) -> Result<()> {
        adapter.record_uses(self.owner(), &self.declared_uses()).await
    }
}

/// The registry's namespace and kind for an entity type, or `None` for one
/// nobody owns.
fn owned_kind(entity_type: EntityType) -> Option<(&'static str, &'static str)> {
    Some(match entity_type {
        EntityType::Table => ("relation", "table"),
        EntityType::View => ("relation", "view"),
        EntityType::MaterializedView => ("relation", "materialized view"),
        EntityType::Sequence => ("relation", "sequence"),
        EntityType::Enum => ("relation", "enum"),
        EntityType::Function => ("routine", "function"),
        EntityType::Procedure => ("routine", "procedure"),
        _ => return None,
    })
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
