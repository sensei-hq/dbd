/**
 * dbd — all site copy and data. No markup, no styling.
 * Loaded as a global (window.DBD_CONTENT) so every section component can read
 * its own slice synchronously and paint immediately.
 * In the app repo: `export default {…}` and import it instead.
 */
(function () {
  var CONTENT = {
    brand: { name: "dbd", full: "Database Designer" },

    nav: {
      links: [
        { label: "Overview", href: "#overview" },
        { label: "Concepts", href: "#concepts" },
        { label: "Targets", href: "#targets" },
        { label: "Commands", href: "#commands" },
        { label: "Who it's for", href: "#audience" },
      ],
      cta: { label: "Get started", href: "#start" },
    },

    hero: {
      eyebrow: "Schema-as-code · built in Rust",
      titleA: "Your database schema,",
      titleB: "managed like source code.",
      lede: "dbd turns plain SQL DDL files into a versioned, deployable schema. No DSL, no ORM, no migration files written by hand — just the folder structure as the source of truth.",
      primaryCta: { label: "Get started", href: "#start" },
      secondaryCta: { label: "Read the concepts", href: "#concepts" },
      install: "cargo install dbd-cli",
      terminal: {
        file: "~/my-project",
        lines: [
          { type: "cmd", text: "dbd snapshot --name \"add notes column\"" },
          { type: "out", text: "\u2713 wrote snapshots/002.json" },
          { type: "out", text: "\u2713 wrote migrations/002/config/lookup_values.sql" },
          { type: "cmd", text: "dbd apply" },
          { type: "out", text: "\u2192 config.lookups        created" },
          { type: "out", text: "\u2192 config.genders        created" },
          { type: "out", text: "\u2192 migration 002         applied" },
          { type: "ok", text: "\u2713 schema in sync" },
        ],
      },
    },

    overview: {
      eyebrow: "What dbd handles",
      title: "One SQL source. Everything else automated.",
      lede: "Point dbd at a folder of DDL files and a single manifest. It works out ordering, diffs, data loading, docs and deployment for you.",
      features: [
        { title: "Dependency ordering", body: "Tables with foreign keys are applied after the tables they reference. No manual sequencing." },
        { title: "Schema migrations", body: "Versioned snapshots with auto-generated ALTER scripts. Change a file, snapshot the diff." },
        { title: "Data loading", body: "CSV, TSV and JSONL files loaded into staging tables with automatic procedure calls." },
        { title: "Documentation", body: "DBML generation for dbdocs.io and dbdiagram.io, straight from your schema." },
        { title: "Multi-target deploy", body: "PostgreSQL, Supabase, SQLite and Convex — the same parsed schema, different adapters." },
        { title: "Scoped deployments", body: "Deploy a named subset of the schema to different databases, with dependency-gap checking." },
        { title: "Formatter + pre-commit", body: "dbd format keeps DDL tidy (river-style); dbd format --check drops into pre-commit and CI." },
        { title: "Row-level security", body: "Manage Postgres/Supabase RLS policies as code in policies/ and apply them with dbd policies." },
        { title: "Reverse-engineer a database", body: "Run dbd init --from-db to generate a fresh project from an existing database, or dbd merge to sync live tables into a project you already have." },
        { title: "Interactive schema diagram", body: "dbd diagram opens a live, explorable view of your schema — tables, columns and foreign-key relationships." },
        { title: "Deploy straight from GitHub", body: "dbd deploy fetches a schema from a directory or GitHub repo (-s owner/repo/path), then applies and imports it — no local checkout." },
        { title: "Environment-scoped data", body: "Seed data is environment-aware: files under import/<env>/<schema>/ load only when you pass -e <env>. Ship dev fixtures that never touch production." },
      ],
    },

    concepts: {
      eyebrow: "Core concepts",
      title: "Five ideas, and you know dbd.",
      items: [
        {
          kicker: "Source of truth",
          title: "DDL files are the schema",
          body: "Your schema is standard SQL under ddl/. One entity per file — table, view, function, procedure or enum. The file path determines the entity name. No DSL, no ORM.",
          code: { lang: "text", label: "ddl/", source: "ddl/table/config/lookups.ddl     \u2192 config.lookups (table)\nddl/view/config/genders.ddl      \u2192 config.genders (view)\nddl/procedure/staging/import.ddl \u2192 staging.import (procedure)\nddl/enum/config/status.sql       \u2192 config.status (enum)" },
        },
        {
          kicker: "Project manifest",
          title: "design.yaml declares the project",
          body: "One YAML file declares metadata, target databases, schemas and data operations. Everything else is auto-discovered from the folder structure.",
          code: { lang: "yaml", label: "design.yaml", source: "project:\n  name: MyProject\n\nsource:\n  dialect: postgresql\n\ntarget:\n  postgres:\n    url: $DATABASE_URL\n    extensions: [uuid-ossp]\n\nschemas: [config, staging]\n\nimport:\n  staging: [staging]\n  options:\n    truncate: true" },
        },
        {
          kicker: "Versioning",
          title: "Two modes of schema evolution",
          body: "Pre-release, dbd reconcile diffs the live database against the design and alters it in place — no snapshots. When you lock in with dbd release, dbd switches to versioned snapshots with auto-generated migrations. Risky changes (renames, type changes) auto-split into safe staged migrations.",
          code: { lang: "sh", label: "terminal", source: "$ dbd reconcile        # pre-release: diff live DB, alter in place\n$ dbd release          # lock in: write baseline snapshot\n$ dbd snapshot --name \"add notes column\"\n# snapshots/002.json            full schema state\n# migrations/002/.../...sql     the ALTER TABLE statement" },
        },
        {
          kicker: "Deployment",
          title: "Adapters handle the target",
          body: "The same parsed schema deploys to different databases. Pick a target by its connection string and dbd does the right thing for that engine.",
          code: { lang: "sh", label: "targets", source: "postgres://\u2026        execute SQL directly via sqlx\ntarget: supabase    managed-infra filtering + grants\nsqlite::memory:     bare-name catalog, batched INSERT\nconvex:             codegen \u2192 convex/schema.ts" },
        },
        {
          kicker: "Embeddable",
          title: "Use it as a library",
          body: "Everything the CLI does lives in the dbd-core crate. Embed schema parsing, diffing and deployment in your own Rust tooling — the guide covers the full API.",
          code: { lang: "rust", label: "main.rs", source: "use dbd_core::Design;\n\nlet design = Design::from_config(path, \"prod\")?;\ndesign.apply(&adapter, None, false).await?;" },
        },
      ],
    },

    targets: {
      eyebrow: "Multi-target deployment",
      title: "Write once. Deploy to four engines.",
      lede: "One parsed schema, four adapters. Each handles the quirks of its engine so you don't have to.",
      items: [
        { name: "PostgreSQL", scheme: "postgres://", body: "Executes SQL directly via sqlx. The reference target — full feature support.", notes: ["Direct DDL execution", "Extensions & enums", "sqlx-backed"] },
        { name: "Supabase", scheme: "postgres:// + target: supabase", body: "PostgreSQL with managed-infrastructure filtering and automatic grant handling. Same connection string — the target flag in design.yaml switches the adapter.", notes: ["Managed-infra filtering", "Grant handling", "Postgres-compatible"] },
        { name: "SQLite", scheme: "sqlite::memory:", body: "Bare-name catalog with batched multi-row INSERT import (\u2264500 rows per batch). Triggers stay atomic.", notes: ["Batched INSERT \u2264500", "Atomic CREATE TRIGGER", "Schemas no-op"] },
        { name: "Convex", scheme: "convex:", body: "Codegen target. Writes convex/schema.ts with v.* validators and v.id() foreign keys.", notes: ["TypeScript codegen", "v.* validators", "schema_entity names"] },
      ],
    },

    commands: {
      eyebrow: "The full toolbelt",
      title: "More where that came from.",
      lede: "Beyond the everyday flow, dbd ships focused commands for validation, data movement and project hygiene.",
      items: [
        { cmd: "dbd inspect", body: "Validate config and report unresolved references — works offline via a local refcache." },
        { cmd: "dbd export", body: "Dump table data back out to CSV, TSV or JSONL." },
        { cmd: "dbd doctor", body: "Audit design.yaml for stale entries." },
        { cmd: "dbd reset", body: "Drop the project's managed objects, with safety guards." },
        { cmd: "dbd combine", body: "Combine all DDL into a single SQL file." },
        { cmd: "dbd graph", body: "Output the dependency graph as JSON." },
        { cmd: "dbd migrate", body: "Show migration status." },
        { cmd: "dbd diagram", body: "Open the hosted schema viewer, with the schema encoded in the URL fragment." },
      ],
    },

    audience: {
      eyebrow: "Who it's for",
      title: "Built for people who think in SQL.",
      items: [
        { title: "Application developers", body: "Who want schema-as-code without reaching for an ORM." },
        { title: "DevOps & platform teams", body: "Automating database deployments straight from Git." },
        { title: "Rust developers", body: "Who want to embed schema management directly in their apps." },
        { title: "Multi-database teams", body: "Driving several databases from a single SQL source." },
      ],
    },

    start: {
      eyebrow: "Get started",
      title: "Install dbd and snapshot your first schema.",
      lede: "It's a single Rust binary. Point it at a folder of SQL and a manifest — that's the whole setup.",
      steps: [
        { n: "1", text: "Install the binary", code: "cargo install dbd-cli" },
        { n: "2", text: "Scaffold a project", code: "dbd init --name my-project" },
        { n: "3", text: "Apply your schema", code: "dbd apply" },
      ],
      cta: { label: "View on GitHub", href: "https://github.com" },
    },

    footer: {
      tagline: "Database schemas as code, built in Rust.",
      columns: [
        { title: "Product", links: [{ label: "Overview", href: "#overview" }, { label: "Concepts", href: "#concepts" }, { label: "Targets", href: "#targets" }] },
        { title: "Resources", links: [{ label: "Get started", href: "#start" }, { label: "GitHub", href: "https://github.com" }, { label: "DBML docs", href: "#" }] },
      ],
      legal: "An open-source project. SQL is the source of truth.",
    },
  };

  if (typeof window !== 'undefined') window.DBD_CONTENT = CONTENT;
  if (typeof module !== 'undefined' && module.exports) module.exports = CONTENT;
})();
