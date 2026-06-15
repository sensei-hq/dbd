//! Emit canonical `CREATE …` DDL text from an `Entity`/`TableDef`. The inverse of
//! the parser; used by the reverse-engineer engine. Output is intended to be
//! re-parseable (round-trip stable).

use crate::entity::Entity;

/// Quote a SQL identifier.
fn q(ident: &str) -> String {
    format!("\"{ident}\"")
}

/// Bare (unqualified) name from a possibly-qualified entity name (`schema.name` → `name`).
fn bare(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

/// `CREATE TYPE "schema"."name" AS ENUM ('a', 'b');`
pub fn emit_enum(entity: &Entity) -> String {
    let schema = entity.schema.as_deref().unwrap_or("public");
    let name = bare(&entity.name);
    let values = entity
        .enum_values
        .iter()
        .map(|v| format!("'{}'", v.name.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(", ");
    format!("CREATE TYPE {}.{} AS ENUM ({});", q(schema), q(name), values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::{EntityType, EnumValue};

    #[test]
    fn emits_enum() {
        let mut e = Entity::new(EntityType::Enum, "shop.order_status");
        e.enum_values = vec![
            EnumValue { name: "pending".into(), note: None },
            EnumValue { name: "paid".into(), note: None },
        ];
        assert_eq!(
            emit_enum(&e),
            "CREATE TYPE \"shop\".\"order_status\" AS ENUM ('pending', 'paid');"
        );
    }
}
