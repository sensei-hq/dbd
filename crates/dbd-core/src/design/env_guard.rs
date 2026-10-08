use super::*;

impl Design {
    /// The environment guard: refuse a run under any env other than prod
    /// against a database marked prod. `meta` is the database's recorded meta
    /// (`None` for one dbd has never written to, which passes) and `env` is the
    /// run's `-e`.
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
    /// the way out there is a database built with `-e dev`, not a flag that
    /// would also unguard a real production database.
    pub fn check_env_guard(meta: Option<&crate::adapter::ProjectMeta>, env: &str) -> Result<()> {
        let Some(meta) = meta else { return Ok(()) };
        // Through the alias table, as the reset guard reads it: rows written
        // before the CLI normalised `-e` can say `production`.
        let is_prod = |value: &str| crate::config::normalize_env(Some(value)).is_ok_and(|e| e == "prod");
        if is_prod(&meta.env) && !is_prod(env) {
            return Err(DbdError::SafetyGuard(format!(
                "environment guard: this database is marked prod, but this run is -e {env}.\n\
                 A {env} run would re-mark it {env}, which lifts the prod guard on `dbd reset`, \
                 and would load import/{env}/ data into it.\n\
                 → re-run with -e prod (the default). dbd never re-marks a prod database: if this \
                 is not production, a run without -e marked it prod — use a database built with -e {env}."
            )));
        }
        Ok(())
    }

    /// [`Self::check_env_guard`] against the database `adapter` reaches, for
    /// this design's env. Read-only, and read before any heal: the meta read
    /// resolves a legacy layout as well as the current one.
    pub(crate) async fn guard_env(&self, adapter: &dyn DatabaseAdapter) -> Result<()> {
        Self::check_env_guard(adapter.get_project_meta().await?.as_ref(), &self.env)
    }
}

#[cfg(test)]
mod tests {
    use crate::Design;
    use crate::adapter::ProjectMeta;

    fn marked(env: &str) -> ProjectMeta {
        ProjectMeta {
            project: "p".into(),
            env: env.into(),
            version: 0,
            scope: None,
            applied_at: None,
        }
    }

    #[test]
    fn a_dev_run_is_refused_on_a_database_marked_prod_or_production() {
        for recorded in ["prod", "production"] {
            let err = Design::check_env_guard(Some(&marked(recorded)), "dev").unwrap_err();
            assert!(err.to_string().contains("marked prod"), "{err}");
        }
    }

    #[test]
    fn a_prod_run_on_a_prod_database_and_any_run_on_a_dev_or_fresh_one_passes() {
        assert!(Design::check_env_guard(Some(&marked("prod")), "prod").is_ok());
        assert!(Design::check_env_guard(Some(&marked("dev")), "dev").is_ok());
        assert!(Design::check_env_guard(Some(&marked("dev")), "prod").is_ok());
        assert!(Design::check_env_guard(None, "dev").is_ok());
    }
}
