use super::*;
use crate::adapter::ProjectMeta;

/// What a database records of the projects dbd has run against it, as the
/// guards read it: this project's record or, for a renamed project, the
/// records its old name left.
///
/// dbd keys its record on `project.name` — one row per project, and more than
/// one project may share a database. Every guard read this project's row and
/// nothing else, so a renamed `project.name` read a production database as one
/// dbd had never seen: `reset` with no `--force` dropped its tables, a dev run
/// passed the environment guard, an older revision the version guard, and
/// another scope the pin.
///
/// So when this project has no record, but the database records other
/// projects and holds this design's objects, those projects' records are the
/// marks every guard holds the run to, and a refusal names them. The objects
/// are what tell a rename from a second project: two projects that share a
/// database each read their own record, and a project the database holds
/// nothing of — a new one, or one its own reset emptied — is held to nothing
/// of another's, as on an empty database.
#[derive(Debug, Clone, Default)]
pub struct DatabaseMarks {
    /// The project the run is for (`project.name`).
    pub project: String,
    /// This project's record.
    pub own: Option<ProjectMeta>,
    /// Every other project's record, by project — filled only when `own` is
    /// `None` and the database holds this design's objects.
    pub others: Vec<ProjectMeta>,
}

impl DatabaseMarks {
    /// The marks of a database whose record of `project` is `own` and which
    /// records no other project — for a caller holding the row already.
    pub fn of_own(project: impl Into<String>, own: Option<ProjectMeta>) -> Self {
        Self {
            project: project.into(),
            own,
            others: Vec::new(),
        }
    }

    /// The records the guards hold a run to: this project's, or the other
    /// projects' standing in for it.
    pub fn rows(&self) -> impl Iterator<Item = &ProjectMeta> {
        // At most one side is filled: `others` only when `own` is `None`.
        self.own.iter().chain(self.others.iter())
    }

    /// This project's recorded version — `0` when it has no record.
    pub fn own_version(&self) -> u32 {
        self.own.as_ref().map_or(0, |m| m.version)
    }

    /// The project that set `row`, when it is another project's mark.
    pub fn recorded_by<'a>(&self, row: &'a ProjectMeta) -> Option<&'a str> {
        self.own.is_none().then_some(row.project.as_str())
    }

    /// The sentence a refusal over another project's mark carries, ending in a
    /// newline — empty over this project's own. It names what the database
    /// records instead, and the likely cause.
    pub fn rename_note(&self) -> String {
        let names: Vec<String> = self.others.iter().map(|m| format!("'{}'", m.project)).collect();
        let (recorded, back) = match self.others.as_slice() {
            [] => return String::new(),
            [one] => (
                format!("project {}", names[0]),
                format!("if this design was '{0}', set project.name back to '{0}'", one.project),
            ),
            _ => (
                format!("projects {}", names.join(", ")),
                "if this design was one of them, set project.name back to its name".to_string(),
            ),
        };
        format!(
            "This database holds this design's objects but no record of project '{}'; it records {recorded}. \
             dbd keys its record on project.name, so a renamed project.name is the likely cause — {back}.\n",
            self.project
        )
    }
}

impl Design {
    /// The [`DatabaseMarks`] of the database `adapter` reaches, for this
    /// design: read-only, and read before any heal, under either bookkeeping
    /// layout. Another project's record is read only when this project has
    /// none, and counts only when the database holds any of this design's
    /// tables, views, materialized views, enums or sequences.
    pub async fn database_marks(&self, adapter: &dyn DatabaseAdapter) -> Result<DatabaseMarks> {
        let mut marks = DatabaseMarks::of_own(&self.config.project.name, adapter.get_project_meta().await?);
        if marks.own.is_some() {
            return Ok(marks);
        }
        let others = adapter.other_projects_meta().await?;
        if !others.is_empty() && self.database_holds_objects(adapter).await? {
            marks.others = others;
        }
        Ok(marks)
    }

    /// Whether the database holds any of this design's own objects — the
    /// kinds a project alone creates, not the schemas, extensions and roles
    /// two projects may share, nor what it declares `external:`.
    async fn database_holds_objects(&self, adapter: &dyn DatabaseAdapter) -> Result<bool> {
        let objects: Vec<(EntityType, String)> = self
            .entities
            .iter()
            .filter(|e| {
                matches!(
                    e.entity_type,
                    EntityType::Table
                        | EntityType::View
                        | EntityType::MaterializedView
                        | EntityType::Enum
                        | EntityType::Sequence
                )
            })
            .map(|e| (e.entity_type, e.name.clone()))
            .collect();
        Ok(!objects.is_empty() && !adapter.existing_objects(&objects).await?.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::DatabaseMarks;
    use crate::adapter::ProjectMeta;

    fn row(project: &str) -> ProjectMeta {
        ProjectMeta {
            project: project.into(),
            env: "prod".into(),
            version: 0,
            scope: None,
            applied_at: None,
        }
    }

    #[test]
    fn a_projects_own_record_carries_no_rename_note() {
        let marks = DatabaseMarks::of_own("alpha", Some(row("alpha")));
        assert_eq!(marks.rename_note(), "");
        assert_eq!(marks.recorded_by(&row("alpha")), None);
    }

    #[test]
    fn another_projects_record_is_named_with_the_likely_rename() {
        let marks = DatabaseMarks {
            project: "alpha2".into(),
            own: None,
            others: vec![row("alpha")],
        };
        let note = marks.rename_note();
        assert!(
            note.contains("project 'alpha2'") && note.contains("records project 'alpha'"),
            "{note}"
        );
        assert!(note.contains("set project.name back to 'alpha'"), "{note}");
        assert_eq!(marks.recorded_by(&marks.others[0]), Some("alpha"));

        let marks = DatabaseMarks {
            others: vec![row("a"), row("b")],
            ..marks
        };
        assert!(
            marks.rename_note().contains("projects 'a', 'b'"),
            "{}",
            marks.rename_note()
        );
    }
}
