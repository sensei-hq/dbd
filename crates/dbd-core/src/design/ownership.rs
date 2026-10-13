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

/// What `reconcile` drops of the live tables its design no longer declares
/// (#7). A `bool` reads as `Off` or `On`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Prune {
    /// Drop nothing; report what a prune would drop.
    #[default]
    Off,
    /// Drop this owner's own, and those nobody owns in a schema no other
    /// owner uses; keep, naming each, those nobody owns that might be
    /// another owner's.
    On,
    /// As `On`, and drop those too, naming each.
    Unowned,
}

impl From<bool> for Prune {
    fn from(prune: bool) -> Self {
        if prune { Self::On } else { Self::Off }
    }
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
    /// every one is claimed; a `DO` block's statements count. Where no
    /// statement read names the path's own entity — a file that does not
    /// parse, a name built in dynamic SQL, a dialect dbd does not read as
    /// Postgres — the path names it, folded.
    ///
    /// Everything Postgres names as a relation or a type shares one namespace:
    /// it gives a table, a view and a materialized view a row type, so none of
    /// them can share a name with an enum, a composite, a domain or a range.
    /// Routines are owned by name, their overloads one entity.
    pub(super) fn entity_keys(&self) -> Vec<(&Entity, Vec<OwnedObject>)> {
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
                let path_namespace = owned_kind(e.entity_type).map(|(namespace, _)| namespace);
                let is_path = |k: &OwnedObject| {
                    Some(k.namespace.as_str()) == path_namespace && k.name.eq_ignore_ascii_case(&path_name)
                };
                if !keys.iter().any(is_path) {
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
                // The path's entity first: it is the one a reference is made
                // through.
                if let Some(i) = keys.iter().position(is_path) {
                    keys.swap(0, i);
                }
                (e, keys)
            })
            .collect()
    }

    /// Take over, as this design's runs claim, the entities it declares that
    /// `owner` holds — moving entities between projects kept in separate
    /// repositories (`--adopt-from`, #7). Only `owner`'s: one a third owner
    /// holds still refuses the run. The giver's next run no longer owns them,
    /// so it neither declares them — it is refused if it still does — nor
    /// prunes them.
    pub fn adopting_from(mut self, owner: impl Into<String>) -> Self {
        self.adopt_from = Some(owner.into());
        self
    }

    /// Let this design run as an owner that holds entities in the database it
    /// declares none of: a new design for the project, not another project
    /// under its name (`--allow-ownership-change`, #7). What the owner held
    /// stays its own — this design's — and a prune drops it as undeclared.
    pub fn allowing_ownership_change(mut self) -> Self {
        self.allow_ownership_change = true;
        self
    }

    /// What this design would adopt in the database `adapter` reaches: what it
    /// declares that the owner it adopts from holds — none without
    /// [`Self::adopting_from`]. Reads only.
    pub async fn adoptions(&self, adapter: &dyn DatabaseAdapter) -> Result<Vec<HeldObject>> {
        let Some(from) = &self.adopt_from else {
            return Ok(Vec::new());
        };
        let held = adapter
            .claim_objects(self.owner(), &self.declared_objects(), false, None)
            .await?;
        Ok(held.into_iter().filter(|h| &h.owner == from).collect())
    }

    /// What the owner holds that still exists, when this design declares none
    /// of it — a reused name — else `None`.
    async fn held_undeclared(
        &self,
        adapter: &dyn DatabaseAdapter,
        declared: &[OwnedObject],
    ) -> Result<Option<Vec<OwnedObject>>> {
        let held = adapter.owned_objects(self.owner()).await?;
        let declares_any = held.iter().any(|h| declared.iter().any(|d| adapter.same_object(d, h)));
        Ok((!held.is_empty() && !declares_any).then_some(held))
    }

    /// Whether this design reuses an owner's name — and is allowed to: the
    /// guards then read the previous design's record. Refuses a reuse not
    /// allowed (see [`Self::check_name_reuse`]).
    pub(crate) async fn reuses_owner_name(&self, adapter: &dyn DatabaseAdapter) -> Result<bool> {
        let declared = self.declared_objects();
        self.check_name_reuse(adapter, &declared).await?;
        Ok(self.allow_ownership_change && self.held_undeclared(adapter, &declared).await?.is_some())
    }

    /// Refuse a design arriving under an owner name that holds entities here
    /// and declaring none of them — another project under a reused name, which
    /// took over the owner's version and history, and whose first prune would
    /// drop what it never declared (#7, H3). Only what still exists counts;
    /// [`Self::allowing_ownership_change`] lifts it.
    async fn check_name_reuse(&self, adapter: &dyn DatabaseAdapter, declared: &[OwnedObject]) -> Result<()> {
        if self.allow_ownership_change {
            return Ok(());
        }
        let Some(held) = self.held_undeclared(adapter, declared).await? else {
            return Ok(());
        };
        const SHOWN: usize = 5;
        let mut named: Vec<String> = held
            .iter()
            .take(SHOWN)
            .map(|o| format!("{} {}.{}", o.namespace, o.schema, o.name))
            .collect();
        if held.len() > SHOWN {
            named.push(format!("and {} more", held.len() - SHOWN));
        }
        Err(DbdError::Config(format!(
            "owner \"{}\" here owns {} {} ({}); this design declares none of them — another project under \
             the same name? If this design is that owner now, run apply, deploy, reconcile or reset with \
             --allow-ownership-change",
            self.owner(),
            held.len(),
            if held.len() == 1 { "entity" } else { "entities" },
            named.join(", ")
        )))
    }

    /// What shares the database `adapter` reaches, read from its records and
    /// its ownership registry: each project, what it owns and uses, and which
    /// record nothing of it yet; and what would refuse this design there (#7,
    /// `dbd inspect --shared`). Reads only.
    pub async fn shared_database(&self, adapter: &dyn DatabaseAdapter) -> Result<SharedDatabase> {
        let registry = adapter.registry().await?;
        let mut projects: std::collections::BTreeMap<String, SharedProject> = std::collections::BTreeMap::new();
        let meta = adapter
            .get_project_meta()
            .await?
            .into_iter()
            .chain(adapter.other_projects_meta().await?);
        for m in meta {
            let p = projects.entry(m.project.clone()).or_default();
            p.env = Some(m.env);
            p.version = Some(m.version);
        }
        for h in registry.claims {
            projects.entry(h.owner).or_default().owns.push(h.object);
        }
        for u in registry.uses {
            let p = projects.entry(u.owner).or_default();
            match u.kind.as_str() {
                "owner" => p.recording = true,
                "schema" => p.schemas.push(u.target),
                "entity" => p.uses.push(SharedUse {
                    target: u.target,
                    via: u.via,
                }),
                _ => {}
            }
        }
        let projects = projects
            .into_iter()
            .map(|(name, p)| SharedProject { name, ..p })
            .collect();

        let declared = self.declared_objects();
        let mut conflicts: Vec<String> = adapter
            .claim_objects(self.owner(), &declared, false, self.adopt_from.as_deref())
            .await?
            .iter()
            .map(|h| {
                format!(
                    "{} {}.{} is owned by \"{}\"",
                    h.object.namespace, h.object.schema, h.object.name, h.owner
                )
            })
            .collect();
        if let Err(e) = self.check_name_reuse(adapter, &declared).await {
            conflicts.push(e.to_string());
        }
        Ok(SharedDatabase { projects, conflicts })
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
    /// makes to an entity it does not declare — recorded whoever owns the
    /// target, the uses guard asking the registry when it runs. A call is of a
    /// routine, anything else of a relation. `via` names the referring entity;
    /// overloads in several files each record their own references, and a
    /// routine another entity's file creates records its body's. A name
    /// written bare that dbd could not find carries the search path Postgres
    /// looks it up along.
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
        // That this owner records its uses, even when it uses nothing.
        uses.insert(shared("owner", self.owner()));
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
        // Each reference `refs` makes, through `via`: a guessed schema that
        // resolved to nothing carries the path it is looked up along.
        let mut add = |via: &OwnedObject, refs: &[crate::entity::Ref], search_path: &[String]| {
            for r in refs.iter().filter(|r| {
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
                        search_path: if r.schema_source.is_guess() {
                            search_path.to_vec()
                        } else {
                            Vec::new()
                        },
                    });
                }
            }
        };
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
            // A reference dbd resolved is `Resolved`, no guess; one it could
            // not still carries the path.
            add(via, &e.refs, &search_path);
            // A routine its file creates beside the entity it is for — a
            // table's trigger function — uses what its own body refers to.
            if matches!(e.entity_type, EntityType::Function | EntityType::Procedure)
                || !keys.iter().any(|k| k.namespace == "routine")
            {
                continue;
            }
            let Some(sql) = e
                .file
                .as_ref()
                .and_then(|file| crate::source_text::read_to_string(&self.project_dir.join(file)).ok())
            else {
                continue;
            };
            for (_, name, refs) in crate::parser::pg::procs::routine_body_refs(&sql, &e.schema_path) {
                if let Some(routine) = keys.iter().find(|k| k.namespace == "routine" && k.name == name) {
                    add(routine, &refs, &search_path);
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
        let declared = self.declared_objects();
        let held = adapter
            .claim_objects(self.owner(), &declared, false, self.adopt_from.as_deref())
            .await?;
        if !held.is_empty() {
            return Err(self.held_error(adapter, &held, &declared).await?);
        }
        self.check_name_reuse(adapter, &declared).await
    }

    /// The refusal over what other owners hold — and, when this project has
    /// no record here while one holder does, and this design declares
    /// everything that holder owns, the likely cause: a renamed
    /// `project.name` (#7).
    async fn held_error(
        &self,
        adapter: &dyn DatabaseAdapter,
        held: &[HeldObject],
        declared: &[OwnedObject],
    ) -> Result<DbdError> {
        let mut refusal = held_refusal(held);
        if adapter.get_project_meta().await?.is_none() {
            let recorded: std::collections::BTreeSet<String> = adapter
                .other_projects_meta()
                .await?
                .into_iter()
                .map(|m| m.project)
                .collect();
            let holders: std::collections::BTreeSet<&str> = held
                .iter()
                .map(|h| h.owner.as_str())
                .filter(|o| recorded.contains(*o))
                .collect();
            if let [holder] = holders.into_iter().collect::<Vec<_>>()[..] {
                let theirs = adapter.owned_objects(holder).await?;
                if theirs
                    .iter()
                    .all(|t| declared.iter().any(|d| adapter.same_object(d, t)))
                {
                    refusal.push_str(&format!(
                        ". This project has no record here, and this design declares everything '{holder}' \
                         owns — a renamed project.name is the likely cause: if this design was '{holder}', set \
                         project.name back to '{holder}', or take its entities over with --adopt-from {holder}"
                    ));
                }
            }
        }
        Ok(DbdError::Config(refusal))
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
        self.settle(adapter).await
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
    /// refusing, naming each, those another owner holds, and a design reusing
    /// an owner's name. What it adopts it takes over in the same claim.
    pub(crate) async fn claim(&self, adapter: &dyn DatabaseAdapter) -> Result<()> {
        let declared = self.declared_objects();
        self.check_name_reuse(adapter, &declared).await?;
        let held = adapter
            .claim_objects(self.owner(), &declared, true, self.adopt_from.as_deref())
            .await?;
        if !held.is_empty() {
            return Err(self.held_error(adapter, &held, &declared).await?);
        }
        Ok(())
    }

    /// Once the work is built — in the open batch when there is one: record
    /// what the design uses, replacing what it used before, and release what
    /// it no longer declares and is gone. The uses describe what is, so a run
    /// that fails keeps the record it had (#7).
    pub(crate) async fn settle(&self, adapter: &dyn DatabaseAdapter) -> Result<()> {
        adapter.record_uses(self.owner(), &self.declared_uses()).await?;
        adapter.release_objects(self.owner(), &self.declared_objects()).await
    }

    /// Who owns each table and enum of `live`, and what a prune of unowned
    /// ones must wait for (#7).
    pub(crate) async fn live_ownership(
        &self,
        adapter: &dyn DatabaseAdapter,
        live: &crate::snapshot::Snapshot,
    ) -> Result<LiveOwnership> {
        let keys: Vec<(String, String)> = live
            .tables
            .iter()
            .map(|t| (t.schema.clone(), t.name.clone()))
            .chain(live.enums.iter().map(|e| (e.schema.clone(), e.name.clone())))
            .collect();
        let objects: Vec<OwnedObject> = keys
            .iter()
            .map(|(schema, name)| OwnedObject {
                namespace: "relation".to_string(),
                schema: schema.clone(),
                name: name.clone(),
                kind: String::new(),
            })
            .collect();
        let found = adapter.ownership(self.owner(), &objects).await?;
        let mut owned = LiveOwnership {
            schema_users: found.schema_users,
            ..LiveOwnership::default()
        };
        // What the run adopts is its own to plan against: the claim that moves
        // it comes before the run writes.
        let adopted: std::collections::HashSet<(String, String)> = match &self.adopt_from {
            Some(_) => self
                .adoptions(adapter)
                .await?
                .into_iter()
                .filter(|h| h.object.namespace == "relation")
                .map(|h| (h.object.schema, h.object.name))
                .collect(),
            None => std::collections::HashSet::new(),
        };
        for (key, owner) in keys.into_iter().zip(found.owners) {
            match owner {
                Some(owner) if owner == self.owner() || adopted.contains(&key) => {
                    owned.mine.insert(key);
                }
                Some(owner) => {
                    owned.others.insert(key, owner);
                }
                None => {}
            }
        }
        owned.not_recording = self.not_recording(adapter, &found.recording).await?;
        Ok(owned)
    }

    /// The other projects the database records that do not record their uses
    /// — not in `recording` — sorted.
    async fn not_recording(
        &self,
        adapter: &dyn DatabaseAdapter,
        recording: &std::collections::BTreeSet<String>,
    ) -> Result<Vec<String>> {
        Ok(adapter
            .other_projects_meta()
            .await?
            .into_iter()
            .map(|m| m.project)
            .filter(|p| p != self.owner() && !recording.contains(p))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect())
    }

    /// Claim what a prune is about to drop, in the transaction that drops it:
    /// read as this owner's or nobody's while the run planned, a table another
    /// project has claimed since is kept, naming it — dropped, that project's
    /// data was gone. What is claimed is released once dropped (#7).
    pub(crate) async fn claim_prune(
        &self,
        adapter: &dyn DatabaseAdapter,
        plan: &mut crate::reconcile::ReconcilePlan,
    ) -> Result<()> {
        loop {
            let candidates: Vec<OwnedObject> = plan
                .dropped
                .iter()
                .map(|stmt| {
                    let (schema, name) = crate::entity::split_qualified_name(&stmt.entity_name);
                    OwnedObject {
                        namespace: "relation".to_string(),
                        schema: schema.unwrap_or_else(|| crate::reconcile::DEFAULT_SCHEMA.to_string()),
                        name,
                        kind: "table".to_string(),
                    }
                })
                .collect();
            if candidates.is_empty() {
                return Ok(());
            }
            // A claim takes all of them or none; the held ones out, again.
            let held = adapter.claim_objects(self.owner(), &candidates, true, None).await?;
            if held.is_empty() {
                return Ok(());
            }
            for h in held {
                let name = format!("{}.{}", h.object.schema, h.object.name);
                plan.dropped.retain(|stmt| stmt.entity_name != name);
                plan.warnings.push(format!(
                    "{name} is owned by \"{}\", which claimed it while this run planned — not dropped",
                    h.owner
                ));
            }
        }
    }

    /// The uses guard (#7): what another owner depends on is part of the
    /// contract between them. A prune candidate another owner depends on is
    /// kept, named — dropped `CASCADE`, it took that owner's dependents with
    /// it. A part the plan drops — a column, a constraint, an index, an enum's
    /// value (its whole type is swapped) — that another owner depends on is
    /// refused, before any write and under a dry run, naming each use. A
    /// part's dependents are the catalog's, column by column, and the uses
    /// recorded through routine bodies, which the catalog does not track.
    pub(crate) async fn guard_uses(
        &self,
        adapter: &dyn DatabaseAdapter,
        plan: &mut crate::reconcile::ReconcilePlan,
    ) -> Result<()> {
        use crate::adapter::{Guarded, GuardedPart};
        use crate::reconcile::DroppedKind;
        let key = |entity: &str| {
            let (schema, name) = crate::entity::split_qualified_name(entity);
            OwnedObject {
                namespace: "relation".to_string(),
                schema: schema.unwrap_or_else(|| crate::reconcile::DEFAULT_SCHEMA.to_string()),
                name,
                kind: String::new(),
            }
        };
        let prunes = plan.dropped.len();
        let mut guarded: Vec<(Guarded, String)> = plan
            .dropped
            .iter()
            .map(|stmt| {
                (
                    Guarded {
                        object: key(&stmt.entity_name),
                        part: GuardedPart::Whole,
                    },
                    stmt.entity_name.clone(),
                )
            })
            .collect();
        for part in &plan.dropped_parts {
            let (part_kind, label) = match part.kind {
                DroppedKind::Column => (
                    GuardedPart::Column(part.name.clone()),
                    format!("{}.{}", part.entity, part.name),
                ),
                DroppedKind::Constraint if !part.name.is_empty() => (
                    GuardedPart::Constraint(part.name.clone()),
                    format!("{} constraint {}", part.entity, part.name),
                ),
                DroppedKind::Index => (
                    GuardedPart::Index(part.name.clone()),
                    format!("{} index {}", part.entity, part.name),
                ),
                DroppedKind::EnumValue => (GuardedPart::Whole, part.entity.clone()),
                // A foreign key of this project's own table holds nobody up.
                DroppedKind::Constraint | DroppedKind::ForeignKey => continue,
            };
            let g = Guarded {
                object: key(&part.entity),
                part: part_kind,
            };
            if !guarded.iter().any(|(seen, _)| *seen == g) {
                guarded.push((g, label));
            }
        }
        // A table nobody owns that the prune drops too is no use of another it
        // drops: what depends on it goes in the same run.
        let going: std::collections::HashSet<String> = plan
            .dropped
            .iter()
            .map(|stmt| {
                let k = key(&stmt.entity_name);
                format!("table {}.{}", k.schema, k.name)
            })
            .collect();
        let uses = self.uses_besides(adapter, &guarded, &going).await?;
        // A prune candidate depended on: kept, named.
        let mut kept = Vec::new();
        for ((_, label), uses) in guarded.iter().zip(&uses).take(prunes) {
            if let Some(uses) = uses {
                kept.push(label.clone());
                plan.warnings
                    .push(format!("{label} is used by {uses} — not pruned; remove that use first"));
            }
        }
        plan.dropped.retain(|stmt| !kept.contains(&stmt.entity_name));
        // A part depended on: refused.
        refuse_uses(guarded.iter().zip(&uses).skip(prunes))
    }

    /// The uses guard for `apply` (#7): what a pending migration the run
    /// executes drops — read from each script it runs, a `.drop.sql`, `.sql`
    /// or `.data.sql` — that another owner depends on refuses the run, before
    /// anything runs and under a dry run, naming each use. A script dbd cannot
    /// read is refused too: what it drops is unknown. A name the script leaves
    /// to the search path is read in its own `SET search_path`, else the path
    /// the migration runs on — its entity's. `plan` is the run's own, built
    /// without writing.
    pub(crate) async fn guard_migration_drops(
        &self,
        adapter: &dyn DatabaseAdapter,
        plan: &super::ExecutionPlan,
    ) -> Result<()> {
        use crate::adapter::{Guarded, GuardedPart};
        // A design not read as Postgres may keep migrations in its own dialect
        // — SQLite's — which libpg_query cannot read: such a script is passed
        // over, not refused. What it can read is guarded as any other's. An
        // unreadable script is refused only where there is someone else to
        // protect: another project recorded in the database.
        let postgres =
            crate::parser::ParserChoice::resolve(&self.config.source.dialect, self.config.source.parser.as_deref())
                .is_ok_and(|parser| parser == crate::parser::ParserChoice::PgQuery);
        let mut others: Option<bool> = None;
        let mut guarded: Vec<(Guarded, String)> = Vec::new();
        for step in &plan.steps {
            let (entity, path) = match step {
                super::ExecutionStep::DropEntity {
                    entity_name,
                    drop_sql_path,
                    ..
                } => (entity_name, drop_sql_path),
                super::ExecutionStep::MigrateEntity {
                    entity_name,
                    migration_sql_path,
                    ..
                } => (entity_name, migration_sql_path),
                super::ExecutionStep::MigrateData {
                    entity_name,
                    data_sql_path,
                    ..
                } => (entity_name, data_sql_path),
                _ => continue,
            };
            let Ok(sql) = crate::source_text::read_to_string(path) else {
                continue;
            };
            if let Err(e) = pg_query::parse(&sql) {
                if !postgres {
                    continue;
                }
                let others = match others {
                    Some(others) => others,
                    None => *others.insert(!adapter.other_projects_meta().await?.is_empty()),
                };
                if !others {
                    continue;
                }
                return Err(DbdError::Config(format!(
                    "{}: dbd cannot read this migration ({e}), so cannot tell what it drops that \
                     another project depends on — fix the script",
                    path.display()
                )));
            }
            // The path the migration runs on: its entity's — the one its file
            // states, else the project's — and for an entity the design no
            // longer has, the project's.
            let entity_schema = self
                .entities
                .iter()
                .find(|e| &e.name == entity)
                .and_then(|e| e.schema_path.default_schema().map(str::to_string))
                .or_else(|| {
                    self.config
                        .source
                        .search_path
                        .as_ref()
                        .and_then(|path| path.first().cloned())
                })
                .or_else(|| crate::entity::split_qualified_name(entity).0)
                .unwrap_or_else(|| crate::reconcile::DEFAULT_SCHEMA.to_string());
            for (namespace, schema, name, part) in crate::script::drops_in(&sql) {
                let schema = schema.unwrap_or_else(|| entity_schema.clone());
                let label = match &part {
                    GuardedPart::Whole if namespace == "schema" => format!("schema {name}"),
                    GuardedPart::Whole if namespace == "extension" => format!("extension {name}"),
                    GuardedPart::Index(i) if *i == name => format!("index {schema}.{i}"),
                    GuardedPart::Whole => format!("{schema}.{name}"),
                    GuardedPart::Column(column) => format!("{schema}.{name}.{column}"),
                    GuardedPart::Constraint(c) => format!("{schema}.{name} constraint {c}"),
                    GuardedPart::Index(i) => format!("{schema}.{name} index {i}"),
                    GuardedPart::Truncate => format!("{schema}.{name}, truncated with CASCADE,"),
                };
                let g = Guarded {
                    object: OwnedObject {
                        namespace: namespace.to_string(),
                        schema,
                        name,
                        kind: String::new(),
                    },
                    part,
                };
                if !guarded.iter().any(|(seen, _)| *seen == g) {
                    guarded.push((g, label));
                }
            }
        }
        let uses = self.uses_of(adapter, &guarded).await?;
        refuse_uses(guarded.iter().zip(&uses))
    }

    /// Refuse a reset, unless forced, while another project's record marks the
    /// database prod (#7): it is prod for every project in it. An env dbd does
    /// not know is no proof it is not, and refuses too.
    pub(crate) async fn check_reset_prod_elsewhere(&self, adapter: &dyn DatabaseAdapter) -> Result<()> {
        let prod = adapter.other_projects_meta().await?.into_iter().find(|m| {
            m.project != self.owner() && !matches!(crate::config::normalize_env(Some(&m.env)).as_deref(), Ok("dev"))
        });
        let Some(row) = prod else {
            return Ok(());
        };
        let marked = match crate::config::normalize_env(Some(&row.env)) {
            Ok(_) => "marked this database prod".to_string(),
            Err(_) => format!("recorded env '{}', which dbd does not know and reads as prod", row.env),
        };
        Err(DbdError::SafetyGuard(format!(
            "reset is blocked — project '{}' {marked}, and a prod mark blocks every project's reset. \
             Use --force to override.",
            row.project
        )))
    }

    /// What a reset of this owner's must leave, or refuse, for other owners
    /// (#7). Refuses, forced or not, when another owner depends on what the
    /// reset drops — an entity, or a schema `--schemas` drops — since dropped
    /// `CASCADE` it takes theirs with it. Keeps, naming why: a schema
    /// `--schemas` would drop that holds anything not this owner's — another
    /// owner's entity, or one nobody owns — or that another owner uses; an
    /// extension `--extensions` would drop that another owner uses or depends
    /// on; a role another owner uses. Reads only.
    pub(crate) async fn reset_ownership(
        &self,
        adapter: &dyn DatabaseAdapter,
        drop: &super::reset::ResetDrops,
    ) -> Result<ResetKeep> {
        use crate::adapter::{Guarded, GuardedPart};
        let mut keep = ResetKeep::default();

        // What the reset drops by name: each entity as its `DROP` names it —
        // `DROP TABLE "app"."orders"` — and not the rest of what its file
        // creates, which the reset leaves: an enum beside the table stays.
        let guarded: Vec<(Guarded, String)> = self
            .entities
            .iter()
            .filter(|e| drop.entities.contains(&e.name))
            .filter_map(|e| {
                let (namespace, kind) = owned_kind(e.entity_type)?;
                let schema = e.schema.clone().unwrap_or_else(|| "public".to_string());
                let (_, name) = crate::entity::split_qualified_name(&e.name);
                let label = format!("{schema}.{name}");
                let object = OwnedObject {
                    namespace: namespace.to_string(),
                    schema,
                    name,
                    kind: kind.to_string(),
                };
                Some((
                    Guarded {
                        object,
                        part: GuardedPart::Whole,
                    },
                    label,
                ))
            })
            .collect();
        let uses = self.uses_of(adapter, &guarded).await?;
        refuse_uses(guarded.iter().zip(&uses))?;

        // A schema dropped `CASCADE` takes what is in it — kept when that is
        // not all this owner's, and guarded when it is: another owner may
        // depend on an entity of this owner's the design no longer declares.
        // An extension the design installs in a schema of its own is its
        // own — unless another owner uses or depends on it.
        let mine = self.declared_objects();
        let declared_extensions = self.reset_extensions(None);
        let mut dropped_schemas = Vec::new();
        for schema in &drop.schemas {
            let mut others = Vec::new();
            for member in adapter.not_owned_in_schema(self.owner(), schema, &mine).await? {
                let own_extension = member.kind == "extension"
                    && declared_extensions.contains(&member.name)
                    && self.extension_users(adapter, &member.name).await?.is_none();
                if !own_extension {
                    others.push(member.label(schema));
                }
            }
            let users = adapter.users_of(self.owner(), "schema", schema).await?;
            let why = if !others.is_empty() {
                const SHOWN: usize = 5;
                let mut named: Vec<String> = others.iter().take(SHOWN).cloned().collect();
                if others.len() > SHOWN {
                    named.push(format!("and {} more", others.len() - SHOWN));
                }
                format!("it holds {}", named.join(", "))
            } else if !users.is_empty() {
                format!("{} uses it", quoted(&users))
            } else {
                dropped_schemas.push((
                    Guarded {
                        object: OwnedObject {
                            namespace: "schema".to_string(),
                            schema: schema.clone(),
                            name: schema.clone(),
                            kind: String::new(),
                        },
                        part: GuardedPart::Whole,
                    },
                    format!("schema {schema}"),
                ));
                continue;
            };
            keep.schemas.push(schema.clone());
            keep.notes.push(format!("schema {schema} kept: {why}"));
        }
        let uses = self.uses_of(adapter, &dropped_schemas).await?;
        refuse_uses(dropped_schemas.iter().zip(&uses))?;
        for extension in &drop.extensions {
            if let Some(why) = self.extension_users(adapter, extension).await? {
                keep.extensions.push(extension.clone());
                keep.notes.push(format!("extension {extension} kept: {why}"));
            }
        }
        for role in &drop.roles {
            let users = adapter.users_of(self.owner(), "role", role).await?;
            if !users.is_empty() {
                keep.roles.push(role.clone());
                keep.notes.push(format!("role {role} kept: {} uses it", quoted(&users)));
            }
        }
        Ok(keep)
    }

    /// Why another owner needs `extension`, if one does: it records a use of
    /// it, or something of its depends on it — a column of its type, say.
    async fn extension_users(&self, adapter: &dyn DatabaseAdapter, extension: &str) -> Result<Option<String>> {
        use crate::adapter::{Guarded, GuardedPart};
        let users = adapter.users_of(self.owner(), "extension", extension).await?;
        if !users.is_empty() {
            return Ok(Some(format!("{} uses it", quoted(&users))));
        }
        let g = (
            Guarded {
                object: OwnedObject {
                    namespace: "extension".to_string(),
                    schema: String::new(),
                    name: extension.to_string(),
                    kind: String::new(),
                },
                part: GuardedPart::Whole,
            },
            extension.to_string(),
        );
        Ok(self
            .uses_of(adapter, std::slice::from_ref(&g))
            .await?
            .into_iter()
            .next()
            .flatten()
            .map(|uses| format!("{uses} depends on it")))
    }

    /// The other owners' uses of each of `guarded`, as one phrase each —
    /// `"billing" (view billing.v_orders)` — or `None` where there are none.
    async fn uses_of(
        &self,
        adapter: &dyn DatabaseAdapter,
        guarded: &[(crate::adapter::Guarded, String)],
    ) -> Result<Vec<Option<String>>> {
        self.uses_besides(adapter, guarded, &std::collections::HashSet::new())
            .await
    }

    /// [`Self::uses_of`], passing over a dependent nobody owns that the run
    /// drops too — `going`, each as `table app.legacy_orders`.
    async fn uses_besides(
        &self,
        adapter: &dyn DatabaseAdapter,
        guarded: &[(crate::adapter::Guarded, String)],
        going: &std::collections::HashSet<String>,
    ) -> Result<Vec<Option<String>>> {
        if guarded.is_empty() {
            return Ok(Vec::new());
        }
        let objects: Vec<crate::adapter::Guarded> = guarded.iter().map(|(g, _)| g.clone()).collect();
        let found: Vec<crate::adapter::Dependence> = adapter
            .dependents(self.owner(), &objects, &self.declared_objects())
            .await?
            .into_iter()
            .filter(|d| d.owner.is_some() || !going.contains(&d.via))
            .collect();
        // What nobody owns is guarded where a project might own it, by the
        // rule `--prune` keeps such a table by: in a schema another owner
        // uses, or anywhere while a project the database records does not
        // record its uses.
        let mut could_own = None;
        if found.iter().any(|d| d.owner.is_none()) {
            let known = adapter.ownership(self.owner(), &[]).await?;
            could_own = Some((known.schema_users, self.not_recording(adapter, &known.recording).await?));
        }
        let phrase = |d: &crate::adapter::Dependence| match &d.owner {
            Some(owner) => Some(format!("\"{owner}\" ({})", d.via)),
            None => {
                let (schema_users, not_recording) = could_own.as_ref()?;
                let mut could: std::collections::BTreeSet<String> = not_recording.iter().cloned().collect();
                could.extend(schema_users.get(&d.schema).into_iter().flatten().cloned());
                let could: Vec<String> = could.into_iter().collect();
                (!could.is_empty()).then(|| format!("{} (owned by nobody; maybe by {})", d.via, quoted(&could)))
            }
        };
        Ok((0..guarded.len())
            .map(|i| {
                let uses: Vec<String> = found.iter().filter(|d| d.guarded == i).filter_map(phrase).collect();
                (!uses.is_empty()).then(|| uses.join(", "))
            })
            .collect())
    }

    /// Settle what a prune drops by ownership (#7). Another owner's entities
    /// never reach the plan — see [`LiveOwnership::others`]. This owner's
    /// own, undeclared, are dropped (rule (a)). One nobody owns is dropped
    /// when no other owner uses its schema and every owner records its uses
    /// (rule (b)); otherwise it may be another owner's, and is kept, named,
    /// unless the run says `--prune-unowned`, which drops it, named.
    pub(crate) fn prune_by_ownership(
        &self,
        plan: &mut crate::reconcile::ReconcilePlan,
        owned: &LiveOwnership,
        prune: Prune,
    ) {
        let quoted = |names: &[String]| names.iter().map(|n| format!("\"{n}\"")).collect::<Vec<_>>().join(", ");
        // What the design creates is no orphan, whatever its path calls it:
        // `report.ddl` creating `app."Report"` reads as an undeclared
        // `Report` beside a missing `report`.
        let created: std::collections::HashMap<(String, String), String> = self
            .entity_keys()
            .into_iter()
            .flat_map(|(e, keys)| {
                let file = e
                    .file
                    .as_ref()
                    .map(|f| f.display().to_string())
                    .unwrap_or_else(|| e.name.clone());
                keys.into_iter()
                    .filter(|k| k.namespace == "relation")
                    .map(move |k| ((k.schema, k.name), file.clone()))
            })
            .collect();
        let mut kept = Vec::new();
        for stmt in std::mem::take(&mut plan.dropped) {
            let (schema, name) = match crate::entity::split_qualified_name(&stmt.entity_name) {
                (Some(schema), name) => (schema, name),
                (None, name) => (crate::reconcile::DEFAULT_SCHEMA.to_string(), name),
            };
            if let Some(file) = created.get(&(schema.clone(), name.clone())) {
                kept.push(format!(
                    "{} is created by {file}, whose path names another table — not dropped",
                    stmt.entity_name
                ));
                continue;
            }
            let why = if owned.mine.contains(&(schema.clone(), name)) {
                None
            } else if !owned.not_recording.is_empty() {
                let (who, verb) = (
                    quoted(&owned.not_recording),
                    if owned.not_recording.len() == 1 { "has" } else { "have" },
                );
                Some(format!(
                    "{who} {verb} not upgraded dbd, so the schemas it uses are unknown"
                ))
            } else {
                owned
                    .schema_users
                    .get(&schema)
                    .map(|users| format!("{schema} is shared with {}", quoted(users)))
            };
            match why {
                None => plan.dropped.push(stmt),
                Some(why) if prune == Prune::Unowned => {
                    plan.warnings.push(format!(
                        "unowned: {} — dropped by --prune-unowned ({why})",
                        stmt.entity_name
                    ));
                    plan.dropped.push(stmt);
                }
                Some(why) => kept.push(format!(
                    "unowned: {} — not dropped {} (--prune-unowned drops it)",
                    stmt.entity_name,
                    if why.contains(" is shared with ") {
                        format!("because {why}")
                    } else {
                        format!("while {why}")
                    }
                )),
            }
        }
        plan.warnings.extend(kept);
    }
}

/// What shares a database (#7) — see [`Design::shared_database`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SharedDatabase {
    /// Every project the database records or holds entities for, by name.
    pub projects: Vec<SharedProject>,
    /// What would refuse this design here, one line each.
    pub conflicts: Vec<String>,
}

/// One project in a shared database (#7).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SharedProject {
    pub name: String,
    /// Its record — `None` for an owner with claims and no record.
    pub env: Option<String>,
    pub version: Option<u32>,
    /// Whether it records what it owns and uses — a dbd that keeps the
    /// registry has run for it. One that does not owns nothing known.
    pub recording: bool,
    /// What it holds that still exists.
    pub owns: Vec<OwnedObject>,
    /// The schemas it uses.
    pub schemas: Vec<String>,
    /// What of others' it uses, and through what of its own.
    pub uses: Vec<SharedUse>,
}

/// A use one project makes of another's entity (#7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedUse {
    /// `relation:app.orders`.
    pub target: String,
    /// `view billing.v_orders`.
    pub via: String,
}

/// `entities` less what another owner than `owner` holds in the database
/// `adapter` reaches, and what was left out, with its owner — for reverse
/// engineering beside other projects (#7): `init --from-db` and `merge` took
/// them as this project's. What is kept and references something left out —
/// a foreign key's table, a view's — gains an `External` entity for it,
/// commented with its owner, so the design declares it `external:` and the
/// reference resolves. Refused while a
/// project the database records does not record what it owns and uses: its
/// entities cannot be told from nobody's. A database keeping no registry
/// leaves nothing out.
pub async fn leave_out_other_owners(
    adapter: &dyn DatabaseAdapter,
    owner: &str,
    entities: Vec<Entity>,
) -> Result<(Vec<Entity>, Vec<HeldObject>)> {
    if !adapter.keeps_registry() {
        return Ok((entities, Vec::new()));
    }
    let registry = adapter.registry().await?;
    let recording: std::collections::BTreeSet<&str> = registry
        .uses
        .iter()
        .filter(|u| u.kind == "owner")
        .map(|u| u.owner.as_str())
        .collect();
    let silent: Vec<String> = adapter
        .other_projects_meta()
        .await?
        .into_iter()
        .map(|m| m.project)
        .filter(|p| p != owner && !recording.contains(p.as_str()))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    if !silent.is_empty() {
        let one = silent.len() == 1;
        return Err(DbdError::Config(format!(
            "{} here {} not run a dbd that records what {} owns and uses — {} entities cannot be told from \
             nobody's. Run {} apply or reconcile with this dbd first",
            quoted(&silent),
            if one { "has" } else { "have" },
            if one { "it" } else { "they" },
            if one { "its" } else { "their" },
            if one { "its" } else { "their" },
        )));
    }
    let held: Vec<&HeldObject> = registry.claims.iter().filter(|h| h.owner != owner).collect();
    let mut kept = Vec::new();
    let mut left = Vec::new();
    for e in entities {
        let namespace = match e.entity_type {
            EntityType::Function | EntityType::Procedure => "routine",
            EntityType::Schema | EntityType::Extension | EntityType::Role => {
                kept.push(e);
                continue;
            }
            _ => "relation",
        };
        let (schema, name) = crate::entity::split_qualified_name(&e.name);
        let schema = schema
            .or_else(|| e.schema.clone())
            .unwrap_or_else(|| crate::reconcile::DEFAULT_SCHEMA.to_string());
        let key = OwnedObject {
            namespace: namespace.to_string(),
            schema,
            name,
            kind: String::new(),
        };
        match held.iter().find(|h| adapter.same_object(&key, &h.object)) {
            Some(h) => left.push((*h).clone()),
            None => kept.push(e),
        }
    }
    if left.is_empty() {
        return Ok((kept, left));
    }
    // What is kept and depends on something left out — a foreign key's
    // table, what a view reads — as a drop's `CASCADE` would reach it.
    let qualified = |e: &Entity| {
        let (schema, name) = crate::entity::split_qualified_name(&e.name);
        let schema = schema
            .or_else(|| e.schema.clone())
            .unwrap_or_else(|| crate::reconcile::DEFAULT_SCHEMA.to_string());
        format!("{schema}.{name}")
    };
    let kept_names: std::collections::HashSet<String> = kept.iter().map(qualified).collect();
    let relations: Vec<&HeldObject> = left.iter().filter(|h| h.object.namespace == "relation").collect();
    let guarded: Vec<crate::adapter::Guarded> = relations
        .iter()
        .map(|h| crate::adapter::Guarded {
            object: h.object.clone(),
            part: crate::adapter::GuardedPart::Whole,
        })
        .collect();
    let referenced: std::collections::BTreeSet<String> = adapter
        .dependents(owner, &guarded, &[])
        .await?
        .into_iter()
        .filter(|d| {
            d.via
                .rsplit_once(' ')
                .is_some_and(|(_, dependent)| kept_names.contains(dependent))
        })
        .map(|d| {
            let h = relations[d.guarded];
            format!("{}.{}", h.object.schema, h.object.name)
        })
        .collect();
    let owners: std::collections::HashMap<String, &str> = left
        .iter()
        .map(|h| (format!("{}.{}", h.object.schema, h.object.name), h.owner.as_str()))
        .collect();
    kept.extend(referenced.iter().map(|name| {
        let mut external = Entity::new(EntityType::External, name);
        external.comment = owners.get(name).map(|owner| format!("another project's (\"{owner}\")"));
        external
    }));
    Ok((kept, left))
}

/// Run `work` holding the database's dbd write lock (#7), released however
/// `work` ends, a run dropped part-way included — see
/// [`DatabaseAdapter::lock_writes`].
pub(crate) async fn with_write_lock<T>(
    adapter: &dyn DatabaseAdapter,
    exclusive: bool,
    work: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    /// Gives the lock up if the run is dropped before it ends.
    struct Abandon<'a>(Option<&'a dyn DatabaseAdapter>);
    impl Drop for Abandon<'_> {
        fn drop(&mut self) {
            if let Some(adapter) = self.0 {
                adapter.abandon_writes();
            }
        }
    }
    adapter.lock_writes(exclusive).await?;
    let mut abandon = Abandon(Some(adapter));
    let done = work.await;
    abandon.0 = None;
    let released = adapter.unlock_writes().await;
    let value = done?;
    released?;
    Ok(value)
}

/// `"a", "b"`.
fn quoted(names: &[String]) -> String {
    names.iter().map(|n| format!("\"{n}\"")).collect::<Vec<_>>().join(", ")
}

/// What a reset leaves for other owners, and why — see
/// [`Design::reset_ownership`].
#[derive(Debug, Default, Clone)]
pub(crate) struct ResetKeep {
    pub(crate) schemas: Vec<String>,
    pub(crate) extensions: Vec<String>,
    /// Roles kept for another project here: neither dropped nor revoked.
    pub(crate) roles: Vec<String>,
    /// Roles kept because another database holds them: their grants here are
    /// revoked, and they are not dropped.
    pub(crate) held_elsewhere: Vec<String>,
    /// One line each: `schema app kept: it holds table app.invoices ("billing")`.
    pub(crate) notes: Vec<String>,
}

/// Refuse, naming each, the guarded entities another owner uses —
/// `app.orders.total is used by "billing" (view billing.v_totals); remove that
/// use first`.
fn refuse_uses<'a>(
    guarded: impl Iterator<Item = (&'a (crate::adapter::Guarded, String), &'a Option<String>)>,
) -> Result<()> {
    let refused: Vec<String> = guarded
        .filter_map(|((_, label), uses)| uses.as_ref().map(|uses| format!("{label} is used by {uses}")))
        .collect();
    if refused.is_empty() {
        return Ok(());
    }
    Err(DbdError::Config(format!(
        "{}; remove that use first",
        refused.join("; ")
    )))
}

/// What the registry says of a run's live tables and enums (#7).
#[derive(Debug, Default)]
pub(crate) struct LiveOwnership {
    /// This owner's, as `(schema, name)`.
    pub(crate) mine: std::collections::HashSet<(String, String)>,
    /// Another owner's, and that owner: hidden from the run, which has no say
    /// over them — not drift, and never pruned.
    pub(crate) others: std::collections::HashMap<(String, String), String>,
    /// Each schema another owner uses, and those owners.
    pub(crate) schema_users: std::collections::BTreeMap<String, Vec<String>>,
    /// The other owners the database records that do not record their uses:
    /// their schemas are unknown, so an unowned entity might be theirs.
    pub(crate) not_recording: Vec<String>,
}

/// The registry's namespace and kind for an entity type, or `None` for one
/// nobody owns.
pub(super) fn owned_kind(entity_type: EntityType) -> Option<(&'static str, &'static str)> {
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
