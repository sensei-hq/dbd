/* dbd website — content data, ported from the design mockup (data.js). */

const REPO = 'https://github.com/sensei-hq/dbd';

export const brand = {
	name: 'dbd',
	full: 'Database Designer',
	blurb: 'Manage database schemas as code. Write SQL, organize it in folders, ship it anywhere.',
	repo: REPO
};

export const nav = {
	links: [
		{ label: 'Overview', href: '/#overview' },
		{ label: 'Concepts', href: '/#concepts' },
		{ label: 'Targets', href: '/#targets' },
		{ label: "Who it's for", href: '/#audience' },
		{ label: 'Guide', href: '/guide' }
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
	install: 'cargo install dbd',
	terminal: {
		file: '~/my-project',
		lines: [
			{ type: 'cmd', text: 'dbd snapshot --name "add notes column"' },
			{ type: 'out', text: '✓ wrote snapshots/002.json' },
			{ type: 'out', text: '✓ wrote migrations/002/config/lookup_values.sql' },
			{ type: 'cmd', text: 'dbd apply' },
			{ type: 'out', text: '→ config.lookups        created' },
			{ type: 'out', text: '→ config.genders        created' },
			{ type: 'out', text: '→ migration 002         applied' },
			{ type: 'ok', text: '✓ schema in sync' }
		] as TerminalLine[]
	}
};

export const overview = {
	eyebrow: 'What dbd handles',
	title: 'One SQL source. Everything else automated.',
	lede: 'Point dbd at a folder of DDL files and a single manifest. It works out ordering, diffs, data loading, docs and deployment for you.',
	features: [
		{
			tag: '01',
			title: 'Dependency ordering',
			body: 'Tables with foreign keys are applied after the tables they reference. No manual sequencing.'
		},
		{
			tag: '02',
			title: 'Schema migrations',
			body: 'Versioned snapshots with auto-generated ALTER scripts. Change a file, snapshot the diff.'
		},
		{
			tag: '03',
			title: 'Data loading',
			body: 'CSV, TSV and JSONL files loaded into staging tables with automatic procedure calls.'
		},
		{
			tag: '04',
			title: 'Documentation',
			body: 'DBML generation for dbdocs.io and dbdiagram.io, straight from your schema.'
		},
		{
			tag: '05',
			title: 'Multi-target deploy',
			body: 'PostgreSQL, Supabase, SQLite and Convex — the same parsed schema, different adapters.'
		},
		{
			tag: '06',
			title: 'Scoped deployments',
			body: 'Deploy a named subset of the schema to different databases, with dependency-gap checking.'
		}
	]
};

export type Code = { lang: string; label: string; source: string };

export const concepts = {
	eyebrow: 'Core concepts',
	title: 'Four ideas, and you know dbd.',
	items: [
		{
			id: 'ddl',
			kicker: 'Source of truth',
			title: 'DDL files are the schema',
			body: 'Your schema is standard SQL under ddl/. One entity per file — table, view, function, procedure or enum. The file path determines the entity name. No DSL, no ORM.',
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
					'project:\n  name: MyProject\n\nsource:\n  dialect: postgresql\n\ntarget:\n  postgres:\n    url: $DATABASE_URL\n    extensions: [uuid-ossp]\n\nschemas: [config, staging]\n\nimport:\n  staging: [staging]\n  options:\n    truncate: true'
			}
		},
		{
			id: 'snapshots',
			kicker: 'Versioning',
			title: 'Snapshots track schema evolution',
			body: 'Change a DDL file, then snapshot the diff. dbd writes a full schema state plus the exact ALTER statement. Next dbd apply runs the migration automatically.',
			code: {
				lang: 'sh',
				label: 'terminal',
				source:
					'$ dbd snapshot --name "add notes column"\n# snapshots/002.json            full schema state\n# migrations/002/.../...sql     the ALTER TABLE statement'
			}
		},
		{
			id: 'adapters',
			kicker: 'Deployment',
			title: 'Adapters handle the target',
			body: 'The same parsed schema deploys to different databases. Pick a target by its connection string and dbd does the right thing for that engine.',
			code: {
				lang: 'sh',
				label: 'targets',
				source:
					'postgres://…     execute SQL directly via sqlx\nsupabase         managed-infra filtering + grants\nsqlite::memory:  bare-name catalog, batched INSERT\nconvex:          codegen → convex/schema.ts'
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
			scheme: 'supabase',
			body: 'PostgreSQL with managed-infrastructure filtering and automatic grant handling.',
			notes: ['Managed-infra filtering', 'Grant handling', 'Postgres-compatible']
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
		{ n: '1', text: 'Install the binary', code: 'cargo install dbd' },
		{ n: '2', text: 'Scaffold a project', code: 'dbd init my-project' },
		{ n: '3', text: 'Apply your schema', code: 'dbd apply' }
	],
	cta: { label: 'View on GitHub', href: REPO }
};

export const footer = {
	tagline: 'Database schemas as code, built in Rust.',
	columns: [
		{
			title: 'Product',
			links: [
				{ label: 'Overview', href: '/#overview' },
				{ label: 'Concepts', href: '/#concepts' },
				{ label: 'Targets', href: '/#targets' }
			]
		},
		{
			title: 'Resources',
			links: [
				{ label: 'Guide', href: '/guide' },
				{ label: 'GitHub', href: REPO },
				{ label: 'llms.txt', href: '/llms.txt' }
			]
		}
	],
	legal: 'An open-source project. SQL is the source of truth.'
};
