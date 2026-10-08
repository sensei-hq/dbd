use super::*;

impl Design {
    /// The environment guard: refuse a run under any env other than prod
    /// against a database marked prod. `marks` are what the database records
    /// (one dbd has never written to passes) and `env` is the run's `-e`.
    ///
    /// A run records its env in the bookkeeping row, and that row is what the
    /// prod guard on `reset` reads — so `apply -e dev` against a production
    /// database re-marked it dev and `reset` went ahead. The dev run is itself
    /// the mistake: it would also load `import/dev/` seed data into production.
    /// So it is refused, before anything is written, by every command that
    /// records the env (apply, deploy, reconcile) and by import, which picks
    /// its data by it.
    ///
    /// One-way on purpose: a dev database may be promoted to prod, which only
    /// tightens what `reset` allows. Nothing re-marks a prod database dev.
    /// There is no override flag either: the default `-e` is prod, so a dev
    /// database a run without `-e` marked prod is the one case it bites, and
    /// the way out there is to rebuild it — `reset --force`, which forgets the
    /// mark with everything else, then a run with `-e dev` — not a flag that
    /// would also unguard a real production database.
    ///
    /// The mark is this project's record or, after a rename, the one its old
    /// `project.name` left (see [`DatabaseMarks`]).
    pub fn check_env_guard(marks: &DatabaseMarks, env: &str) -> Result<()> {
        // Through the alias table, as the reset guard reads it: rows written
        // before the CLI normalised `-e` can say `production`.
        let is_prod = |value: &str| crate::config::normalize_env(Some(value)).is_ok_and(|e| e == "prod");
        if is_prod(env) {
            return Ok(());
        }
        let Some(row) = marks.rows().find(|m| is_prod(&m.env)) else {
            return Ok(());
        };
        // The rebuild is no way out from another name's mark: a forced reset
        // forgets only this project's record, and the old name's stays.
        let (by, way_out) = match marks.recorded_by(row) {
            None => (
                String::new(),
                format!(
                    "→ re-run with -e prod (the default). dbd never re-marks a prod database: if this \
                     is not production, a run without -e marked it prod — rebuild it: \
                     `dbd reset --force`, then run with -e {env}."
                ),
            ),
            Some(other) => (
                format!(" by project '{other}'"),
                "→ set project.name back, or re-run with -e prod (the default).".to_string(),
            ),
        };
        Err(DbdError::SafetyGuard(format!(
            "environment guard: this database is marked prod{by}, but this run is -e {env}.\n{note}\
             A {env} run would mark it {env}, which lifts the prod guard on `dbd reset`, \
             and would load import/{env}/ data into it.\n{way_out}",
            note = marks.rename_note()
        )))
    }

    /// [`Self::check_env_guard`] against the database `adapter` reaches, for
    /// this design's env. Read-only, and read before any heal: the meta read
    /// resolves a legacy layout as well as the current one.
    ///
    /// Public for a caller that loads data outside [`Self::import_data`] — the
    /// CLI's ad-hoc `import -n -f` — and for a preview of either, which must
    /// refuse where the run does.
    pub async fn guard_env(&self, adapter: &dyn DatabaseAdapter) -> Result<()> {
        Self::check_env_guard(&self.database_marks(adapter).await?, &self.env)
    }

    /// The guards [`Self::apply`] checks against the database before it runs
    /// anything: the environment and the version. One function, so a preview
    /// with a database to ask refuses exactly where the run does — `apply` and
    /// `deploy --dry-run -e dev` previewed a run over a prod database that the
    /// run then refused.
    pub fn check_apply_guards(&self, marks: &DatabaseMarks) -> Result<()> {
        Self::check_env_guard(marks, &self.env)?;
        Self::check_version_guard(marks, self.config.project.version.unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use crate::Design;
    use crate::adapter::ProjectMeta;
    use crate::design::DatabaseMarks;

    fn marked(env: &str) -> DatabaseMarks {
        DatabaseMarks::of_own("p", Some(row("p", env)))
    }

    fn row(project: &str, env: &str) -> ProjectMeta {
        ProjectMeta {
            project: project.into(),
            env: env.into(),
            version: 0,
            scope: None,
            applied_at: None,
        }
    }

    #[test]
    fn a_dev_run_is_refused_on_a_database_marked_prod_or_production() {
        for recorded in ["prod", "production"] {
            let err = Design::check_env_guard(&marked(recorded), "dev").unwrap_err();
            assert!(err.to_string().contains("marked prod"), "{err}");
        }
    }

    #[test]
    fn a_prod_run_on_a_prod_database_and_any_run_on_a_dev_or_fresh_one_passes() {
        assert!(Design::check_env_guard(&marked("prod"), "prod").is_ok());
        assert!(Design::check_env_guard(&marked("dev"), "dev").is_ok());
        assert!(Design::check_env_guard(&marked("dev"), "prod").is_ok());
        assert!(Design::check_env_guard(&DatabaseMarks::of_own("p", None), "dev").is_ok());
    }

    /// Another project's prod mark refuses as this project's would, naming it
    /// — and the way out is the name, since a forced reset forgets only this
    /// project's record.
    #[test]
    fn another_projects_prod_mark_refuses_a_dev_run_naming_it() {
        let marks = DatabaseMarks {
            project: "p2".into(),
            own: None,
            others: vec![row("p", "prod")],
        };
        let err = Design::check_env_guard(&marks, "dev").unwrap_err().to_string();
        assert!(err.contains("marked prod by project 'p'"), "{err}");
        assert!(err.contains("project.name") && !err.contains("reset --force"), "{err}");
    }
}
