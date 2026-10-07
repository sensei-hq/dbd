/* dbd website — content data, ported from the design mockup (dbd.dc.html). */

const REPO = 'https://github.com/sensei-hq/dbd';

export const brand = {
	name: 'dbd',
	full: 'Database Designer',
	blurb: 'Manage database schemas as code. Write SQL, organize it in folders, ship it anywhere.',
	repo: REPO
};

export const nav = {
	// "Who it's for" moved to the footer to make room for the viewer and the
	// changelog; it is still a section of the home page.
	links: [
		{ label: 'Overview', href: '/#overview' },
		{ label: 'Concepts', href: '/#concepts' },
		{ label: 'Targets', href: '/#targets' },
		{ label: 'Commands', href: '/#commands' },
		{ label: 'Guide', href: '/guide' },
		{ label: 'Viewer', href: '/diagram' },
		{ label: 'Changelog', href: '/changelog' },
		{ label: 'Projects', href: '/projects' }
	],
	cta: { label: 'Get started', href: '/#start' }
};

export type TerminalLine = { type: 'cmd' | 'out' | 'ok'; text: string };

export const hero = {
	eyebrow: 'Schema-as-code · built in Rust',
	title: ['Your database schema,', 'managed like source code.'],
	lede: 'dbd turns plain SQL DDL files into a versioned, deployable schema. No DSL, no ORM, no migration files written by hand — just the folder structure as the source of truth.',
	primaryCta: { label: 'Get started', href: '#start' },
	secondaryCta: { label: 'Read the concepts', href: '#concepts' },
	install: 'cargo install dbd-cli',
	terminal: {
		file: '~/my-project',
		// dbd's own output, verbatim (blank lines dropped): a released project of
		// five entities gains a column, from v1 to v2.
		lines: [
			{ type: 'cmd', text: 'dbd inspect' },
			{ type: 'out', text: 'Everything looks ok' },
			{ type: 'out', text: '5 entities — no issues' },
			{ type: 'cmd', text: 'dbd snapshot --name "add notes column"' },
			{ type: 'out', text: 'Snapshot v002 created — 0 added, 1 altered, 0 dropped.' },
			{ type: 'out', text: 'design.yaml version updated to 2' },
			{ type: 'cmd', text: 'dbd apply' },
			{ type: 'ok', text: 'Migrated v1 → v2 — 5 applied, 1 migrated, 0 created, 0 dropped.' }
		] as TerminalLine[]
	}
};

export const overview = {
	eyebrow: 'What dbd handles',
	title: 'One SQL source. Everything else automated.',
	lede: 'Point dbd at a folder of DDL files and a single manifest. It works out ordering, diffs, data loading, docs and deployment for you.',
	features: [
		{
			title: 'Dependency ordering',
			body: 'Tables with foreign keys are applied after the tables they reference. No manual sequencing.'
		},
		{
			title: 'Schema migrations',
			body: 'Versioned snapshots with auto-generated ALTER scripts. Change a file, snapshot the diff.'
		},
		{
			title: 'Data loading',
			body: 'CSV, TSV, JSON and JSONL files loaded into staging tables, then moved into place by the procedures that read them.'
		},
		{
			title: 'Documentation',
			body: 'DBML generation for dbdocs.io and dbdiagram.io, straight from your schema.'
		},
		{
			title: 'Multi-target deploy',
			body: 'PostgreSQL, Supabase, SQLite and Convex — the same parsed schema, different adapters.'
		},
		{
			title: 'Scoped deployments',
			body: 'Deploy a named subset of the schema to different databases, with dependency-gap checking.'
		},
		{
			title: 'Formatter + pre-commit',
			body: 'dbd format keeps DDL tidy (river-style); dbd format --check drops into pre-commit and CI.'
		},
		{
			title: 'Row-level security',
			body: 'Manage Postgres/Supabase RLS policies as code in policies/. dbd deploy applies them, and so do dbd policies and dbd apply --with-policies.'
		},
		{
			title: 'Reverse-engineer a database',
			body: 'Run dbd init --from-db to generate a fresh project from an existing database, or dbd merge to sync live tables into a project you already have.'
		},
		{
			title: 'Interactive schema viewer',
			body: 'dbd diagram opens your schema in the browser: an overview, an ER diagram, a page for every table, view, routine, trigger, sequence and enum, and a changelog built from your snapshots.'
		},
		{
			title: 'Deploy straight from GitHub',
			body: 'dbd deploy fetches a schema from a directory or a GitHub repo (-s owner/repo/path, GITHUB_TOKEN for a private one), then applies it, grants, imports and applies policies — no local checkout.'
		},
		{
			title: 'Environment-scoped data',
			body: 'Seed data is environment-aware: files under import/<env>/<schema>/ load only when you pass -e <env>. Ship dev fixtures that never touch production.'
		},
		{
			title: 'Materialized views',
			body: 'A first-class entity type under ddl/materialized_view/, refreshed on a pg_cron schedule declared in design.yaml or now with dbd refresh. Drift is reported; a matview is never silently dropped and rebuilt.'
		},
		{
			title: 'Lifecycle hooks',
			body: 'apply.before, apply.after and import.after run your own SQL around the DDL and data phases — publications, grants, loaders — and a scope filters them like everything else.'
		},
		{
			title: 'Drift, both ways',
			body: 'dbd diff shows how the live database differs from the design, read-only, with an exit code for CI. dbd emit goes the other way and translates the schema for MySQL, T-SQL or SQLite.'
		}
	]
};

export type Code = { lang: string; label: string; source: string };

export const concepts = {
	eyebrow: 'Core concepts',
	title: 'Five ideas, and you know dbd.',
	items: [
		{
			id: 'ddl',
			kicker: 'Source of truth',
			title: 'DDL files are the schema',
			body: 'Your schema is standard SQL under ddl/. One entity per file — table, view, materialized view, function, procedure, enum, sequence or role. The file path determines the entity name. No DSL, no ORM.',
			code: {
				lang: 'text',
				label: 'ddl/',
				source:
					'ddl/table/config/lookups.ddl     → config.lookups (table)\n' +
					'ddl/view/config/genders.ddl      → config.genders (view)\n' +
					'ddl/procedure/staging/import.ddl → staging.import (procedure)\n' +
					'ddl/enum/config/status.sql       → config.status (enum)'
			}
		},
		{
			id: 'manifest',
			kicker: 'Project manifest',
			title: 'design.yaml declares the project',
			body: 'One YAML file declares metadata, target databases, schemas and data operations. Everything else is auto-discovered from the folder structure.',
			code: {
				lang: 'yaml',
				label: 'design.yaml',
				source:
					'project:\n  name: MyProject\n\nsource:\n  dialect: postgresql\n\ntarget:\n  postgres:\n    url: $DATABASE_URL\n    extensions: [uuid-ossp]\n\nschemas: [config, staging]\n\nimport:\n  options:\n    truncate: true'
			}
		},
		{
			id: 'snapshots',
			kicker: 'Versioning',
			title: 'Two modes of schema evolution',
			body: 'Pre-release, dbd reconcile diffs the live database against the design and alters it in place — no snapshots. When you lock in with dbd release, dbd switches to versioned snapshots with auto-generated migrations. Risky changes (renames, type changes) auto-split into safe staged migrations.',
			code: {
				lang: 'sh',
				label: 'terminal',
				source:
					'$ dbd reconcile        # pre-release: diff live DB, alter in place\n' +
					'$ dbd release          # lock in: write baseline snapshot\n' +
					'$ dbd snapshot --name "add notes column"\n' +
					'# snapshots/002.json            full schema state\n' +
					'# migrations/002/.../...sql     the ALTER TABLE statement'
			}
		},
		{
			id: 'adapters',
			kicker: 'Deployment',
			title: 'Adapters handle the target',
			body: "The same parsed schema deploys to different databases. The connection URL picks the adapter; naming the target supabase layers Supabase's rules over PostgreSQL.",
			code: {
				lang: 'sh',
				label: 'targets',
				source:
					'postgres://…        execute SQL directly via sqlx\n' +
					'target: supabase    same adapter + Supabase rules\n' +
					'sqlite::memory:     bare-name catalog, batched INSERT\n' +
					'convex:             codegen → convex/schema.ts'
			}
		},
		{
			id: 'library',
			kicker: 'Embeddable',
			title: 'Use it as a library',
			body: 'Everything the CLI does lives in the dbd-core crate. Embed schema parsing, diffing and deployment in your own Rust tooling — the guide covers the full API.',
			code: {
				lang: 'rust',
				label: 'main.rs',
				source:
					'use dbd_core::{Design, design::Progress};\n\nlet design = Design::from_config(path, "prod")?;\ndesign.apply(&adapter, None, false, None, Progress::none()).await?;'
			}
		}
	]
};

export const targets = {
	eyebrow: 'Multi-target deployment',
	title: 'Write once. Deploy to four engines.',
	lede: "One parsed schema, four adapters. Each handles the quirks of its engine so you don't have to.",
	items: [
		{
			name: 'PostgreSQL',
			scheme: 'postgres://',
			body: 'Executes SQL directly via sqlx. The reference target — full feature support.',
			notes: ['Direct DDL execution', 'Extensions & enums', 'sqlx-backed']
		},
		{
			name: 'Supabase',
			scheme: 'postgres:// + target: supabase',
			body: "The PostgreSQL adapter, plus Supabase's rules when the target is named supabase: public is exposed, reconcile --prune and reset leave the platform's schemas alone, and configured grants add PostgREST access.",
			notes: ['Same Postgres adapter', 'Platform schemas protected', 'PostgREST grants']
		},
		{
			name: 'SQLite',
			scheme: 'sqlite::memory:',
			body: 'Bare-name catalog with batched multi-row INSERT import (≤500 rows per batch). Triggers stay atomic.',
			notes: ['Batched INSERT ≤500', 'Atomic CREATE TRIGGER', 'Schemas no-op']
		},
		{
			name: 'Convex',
			scheme: 'convex:',
			body: 'Codegen target. Writes convex/schema.ts with v.* validators and v.id() foreign keys.',
			notes: ['TypeScript codegen', 'v.* validators', 'schema_entity names']
		}
	]
};

export type Command = { cmd: string; body: string };

export const commands = {
	eyebrow: 'The full toolbelt',
	title: 'More where that came from.',
	lede: 'Beyond the everyday flow, dbd ships focused commands for validation, data movement and project hygiene.',
	items: [
		{
			cmd: 'dbd inspect',
			body: 'Validate the design offline and report errors, warnings and scope gaps — exits 1 on a blocking error, so it gates CI.'
		},
		{
			cmd: 'dbd diff',
			body: 'Read-only: how the live database differs from the design, as a report, as JSON, or as an exit code.'
		},
		{
			cmd: 'dbd release',
			body: 'Cut v1: write a baseline snapshot and move from reconcile to versioned migrations.'
		},
		{
			cmd: 'dbd import',
			body: 'Load CSV, TSV, JSON and JSON Lines from import/ into staging tables, then call the procedures that read them.'
		},
		{ cmd: 'dbd export', body: 'Dump table data — and the views export: lists — back out to CSV, TSV or JSONL.' },
		{ cmd: 'dbd refresh', body: 'Refresh materialized views now, outside their pg_cron schedule.' },
		{
			cmd: 'dbd emit',
			body: 'Translate a PostgreSQL schema into MySQL, T-SQL or SQLite DDL, reporting every downgrade.'
		},
		{ cmd: 'dbd dbml', body: 'Generate DBML for dbdocs.io and dbdiagram.io, one file per configured document.' },
		{
			cmd: 'dbd policies',
			body: 'Apply the RLS policies in policies/<schema>/<table>.sql, skipping any the scope excludes.'
		},
		{
			cmd: 'dbd doctor',
			body: 'Audit the project: old config keys, stale files, plural type folders, misfiled view DDL.'
		},
		{ cmd: 'dbd reset', body: "Drop the project's managed objects, with safety guards." },
		{ cmd: 'dbd combine', body: 'Combine all DDL into a single SQL file.' },
		{ cmd: 'dbd graph', body: 'Output the dependency graph as JSON.' },
		{ cmd: 'dbd migrate', body: 'Show migration status.' },
		{ cmd: 'dbd diagram', body: 'Open the hosted schema viewer, with the schema encoded in the URL fragment.' },
		{
			cmd: 'dbd install',
			body: "Install dbd's Claude Code skill and review agent, so an AI assistant follows the conventions."
		}
	] as Command[]
};

export const audience = {
	eyebrow: "Who it's for",
	title: 'Built for people who think in SQL.',
	items: [
		{ title: 'Application developers', body: 'Who want schema-as-code without reaching for an ORM.' },
		{ title: 'DevOps & platform teams', body: 'Automating database deployments straight from Git.' },
		{ title: 'Rust developers', body: 'Who want to embed schema management directly in their apps.' },
		{ title: 'Multi-database teams', body: 'Driving several databases from a single SQL source.' }
	]
};

export const start = {
	eyebrow: 'Get started',
	title: 'Install dbd and snapshot your first schema.',
	lede: "It's a single Rust binary. Point it at a folder of SQL and a manifest — that's the whole setup.",
	steps: [
		{ n: '1', text: 'Install the binary', code: 'cargo install dbd-cli' },
		{ n: '2', text: 'Scaffold a project', code: 'dbd init --name my-project' },
		{ n: '3', text: 'Apply your schema', code: 'dbd apply' }
	],
	cta: { label: 'View on GitHub', href: REPO }
};

export const changelog = {
	eyebrow: 'Release notes',
	title: 'Changelog',
	lede: 'What changed in the last five releases of dbd. Older releases are in the full changelog.',
	description: 'Release notes for the last five versions of dbd, the schema-as-code CLI and Rust library.',
	full: `${REPO}/blob/main/CHANGELOG.md`,
	fullLabel: 'Read the full changelog on GitHub'
};

export const footer = {
	tagline: 'Database schemas as code, built in Rust.',
	columns: [
		{
			title: 'Product',
			links: [
				{ label: 'Overview', href: '/#overview' },
				{ label: 'Concepts', href: '/#concepts' },
				{ label: 'Targets', href: '/#targets' },
				{ label: 'Commands', href: '/#commands' },
				{ label: "Who it's for", href: '/#audience' }
			]
		},
		{
			title: 'Resources',
			links: [
				{ label: 'Guide', href: '/guide' },
				{ label: 'Schema viewer', href: '/diagram' },
				{ label: 'Changelog', href: '/changelog' },
				{ label: 'GitHub', href: REPO },
				{ label: 'llms.txt', href: '/llms.txt' },
				{ label: 'llms-full.txt', href: '/llms-full.txt' }
			]
		}
	],
	legal: 'An open-source project. SQL is the source of truth.'
};
