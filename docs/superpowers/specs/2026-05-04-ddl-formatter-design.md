# DDL Formatter Design

**Date:** 2026-05-04  |  **Status:** Draft
**Scope:** `dbd format` command, `dbd inspect --fix`, `FormatConfig` in design.yaml

---

## Overview

Add a DDL formatter that parses SQL with sqlparser-rs and re-emits it in a configurable canonical style. The formatter enforces the project's DDL conventions: lowercase keywords, leading commas, aligned types, and consistent indentation.

## Data Flow

```
dbd format:  scan_ddl() -> for each file: format_ddl(sql, config) -> write back (or diff for --check)
dbd inspect --fix:  inspect pass -> format_ddl() on files with formatting warnings -> write back
```

## Configuration (`design.yaml`)

```yaml
format:
  keyword_case: lower        # lower | upper | preserve
  comma_style: leading       # leading | trailing
  type_alignment: 27         # column for type start, 0 = off
  indent: 2                  # spaces per level
```

```rust
pub struct FormatConfig {
    pub keyword_case: KeywordCase,   // Lower (default) | Upper | Preserve
    pub comma_style: CommaStyle,     // Leading (default) | Trailing
    pub type_alignment: usize,       // default: 27
    pub indent: usize,               // default: 2
}
```

## Pure Function

```rust
/// Parse DDL with sqlparser, re-emit in canonical style.
/// Handles: CREATE TABLE/INDEX/VIEW/FUNCTION/PROCEDURE/TYPE, COMMENT ON, SET search_path.
/// Unparseable blocks preserved verbatim. Function $$ bodies preserved verbatim.
pub fn format_ddl(sql: &str, config: &FormatConfig) -> String
```

Logic: split into statement blocks, parse each with `PostgreSqlDialect`, re-emit with configured style. Unparseable blocks pass through unchanged.

## CLI

```rust
Format { #[arg(long)] check: bool }   // new command
Inspect { name: Option<String>, #[arg(long)] fix: bool }  // add --fix flag
```

`dbd format` -- reformat all DDL in-place. `dbd format --check` -- exit 1 if any file would change (CI).
`dbd inspect --fix` -- auto-fix formatting issues found during inspect.

## Test Scenarios

| ID | Scenario | Assert |
|----|----------|--------|
| F1 | Uppercase keywords | `CREATE TABLE` -> `create table` with `keyword_case: lower` |
| F2 | Trailing to leading commas | `, col` lines with `comma_style: leading` |
| F3 | Type alignment | Types start at column 27 with `type_alignment: 27` |
| F4 | Unparseable preserved | Non-standard SQL returned verbatim |
| F5 | Idempotent | `format(format(x)) == format(x)` |
| F6 | --check exit 1 | Unformatted file: exit 1, file unchanged |
| F7 | --check exit 0 | Already formatted: exit 0 |
| F8 | Function body | `$$` body preserved, header reformatted |
| F9 | SET search_path | Keywords lowercased |
| F10 | COMMENT ON | Keywords lowercased, string preserved |
| F11 | Defaults | `FormatConfig::default()` matches project style |
| F12 | Upper mode | `keyword_case: upper` produces `CREATE TABLE` |

## Files

| File | Action |
|------|--------|
| `crates/dbd-core/src/formatter.rs` | Create -- pure `format_ddl()` |
| `crates/dbd-core/src/config.rs` | Modify -- add `FormatConfig` types |
| `crates/dbd-core/src/lib.rs` | Modify -- export `formatter` |
| `crates/dbd-cli/src/cli.rs` | Modify -- `Format` command, `--fix` on `Inspect` |
| `crates/dbd-cli/src/commands.rs` | Modify -- `cmd_format`, update `cmd_inspect` |

## Future Work

- Per-file format overrides via frontmatter
- SQL comment preservation during reformat
- Pre-commit hook / editor integration
