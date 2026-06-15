/* ============================================================
   dbd designs — MOCKUP sample data + component prop types

   Each component is driven purely by its own typed prop; this file
   provides one self-contained sample dataset so the gallery can render
   every component independently. Reuses the viewer's SchemaModel shape
   so the diagram can reuse `toLayoutData` / `layout.ts`.
   ============================================================ */
import type { SchemaModel } from '$lib/design/model';

// ---- prop types ----
export type Crumb = { label: string; href?: string; current?: boolean };
export type User = { name: string; email: string; initials: string };

export type HeaderData = {
  brand: { name: string; badge: string };
  crumbs: Crumb[];
  user: User;
};

export type EnumDef = { schema: string; name: string; values: string[] };

export type SidebarData = {
  project: { name: string };
  model: SchemaModel;
  enums: EnumDef[];
};

export type Stat = { value: number; label: string };
export type Tab = { id: string; label: string; icon: string };

export type ContentHeaderData = {
  // version / via / updated come from app metadata (not the schema model), so
  // they're optional — omitted for the shareable-link flow which has no storage.
  project: { name: string; db: string; note?: string; version?: string; via?: string; updated?: string };
  stats: Stat[];
  tabs: Tab[];
  activeTab: string;
};

export const BRAND = { name: 'dbd', badge: 'designs' };

const ROOT_TABS: Tab[] = [
  { id: 'diagram', label: 'Diagram', icon: 'grid' },
  { id: 'entities', label: 'Entities', icon: 'rows' },
];

/** Header data for a project, with an optional drilled-into entity crumb. */
export function buildHeaderData(projectName: string, entityName?: string): HeaderData {
  return {
    brand: BRAND,
    crumbs: [
      { label: 'Designs', href: '/projects' },
      { label: projectName, current: !entityName },
      ...(entityName ? [{ label: entityName, current: true }] : []),
    ],
    user: sampleUser,
  };
}

/** Content-header data derived from a SchemaModel (no version/push metadata). */
export function buildContentHeaderData(model: SchemaModel, activeTab = 'diagram'): ContentHeaderData {
  const enums = model.schemas.reduce((a, s) => a + s.enums, 0);
  return {
    project: { name: model.project.name, db: model.project.db, note: model.project.note },
    stats: [
      { value: model.tables.length, label: 'tables' },
      { value: enums, label: 'enums' },
      { value: model.refs.length, label: 'refs' },
    ],
    tabs: ROOT_TABS,
    activeTab,
  };
}

// ---- sample schema (2 schemas, cross-schema refs, enums, notes) ----
export const sampleModel: SchemaModel = {
  project: { name: 'shopdb', db: 'postgresql', note: 'Storefront catalog, customers and orders.' },
  schemas: [
    { name: 'auth', tables: 2, enums: 0 },
    { name: 'shop', tables: 4, enums: 1 },
  ],
  tables: [
    {
      schema: 'auth', name: 'users', kind: 'table',
      noteMd: 'Account records. One row per signed-up person.\nEmail is unique and used for login.',
      columns: [
        { name: 'id', type: 'uuid', pk: true, nn: true, note: 'Primary key.' },
        { name: 'email', type: 'text', nn: true, note: 'Login email — unique.' },
        { name: 'name', type: 'text', note: 'Display name; optional.' },
        { name: 'created_at', type: 'timestamptz', nn: true, def: 'now()', note: 'Account signup timestamp.' },
      ],
      indexes: [
        { def: '(email)', unique: true, name: 'users_email_key' },
        { def: '(created_at)', name: 'users_created_at_idx' },
      ],
    },
    {
      schema: 'auth', name: 'sessions', kind: 'table',
      noteMd: 'Active login sessions, expired rows are pruned nightly.',
      columns: [
        { name: 'id', type: 'uuid', pk: true, nn: true, note: 'Session primary key.' },
        { name: 'user_id', type: 'uuid', nn: true, note: 'Owner of the session → auth.users.' },
        { name: 'token', type: 'varchar(128)', nn: true, note: 'Opaque bearer token (hashed at rest).' },
        { name: 'expires_at', type: 'timestamptz', nn: true, note: 'Absolute expiry; rows past this are pruned.' },
      ],
      indexes: [
        { def: '(token)', unique: true, name: 'sessions_token_key' },
        { def: '(user_id)', name: 'sessions_user_id_idx' },
        { def: '(expires_at)', name: 'sessions_expires_at_idx' },
      ],
    },
    {
      schema: 'shop', name: 'customers', kind: 'table',
      noteMd: 'A customer profile, linked to an `auth.users` account.',
      columns: [
        { name: 'id', type: 'uuid', pk: true, nn: true, note: 'Customer primary key.' },
        { name: 'user_id', type: 'uuid', nn: true, note: 'The account this profile belongs to → auth.users.' },
        { name: 'billing_address', type: 'text', note: 'Free-form billing address; null until checkout.' },
        { name: 'created_at', type: 'timestamptz', nn: true, def: 'now()', note: 'When the profile was created.' },
      ],
    },
    {
      schema: 'shop', name: 'products', kind: 'table',
      noteMd: 'Sellable catalog items.',
      columns: [
        { name: 'id', type: 'uuid', pk: true, nn: true, note: 'Product primary key.' },
        { name: 'sku', type: 'varchar(32)', nn: true, note: 'Stock-keeping unit — unique, human-facing.' },
        { name: 'name', type: 'text', nn: true, note: 'Display name shown in the catalog.' },
        { name: 'price_cents', type: 'integer', nn: true, def: '0', note: 'Unit price in cents to avoid float rounding.' },
      ],
    },
    {
      schema: 'shop', name: 'orders', kind: 'table',
      noteMd: 'A placed order.\n- belongs to one customer\n- has many `order_items`',
      columns: [
        { name: 'id', type: 'uuid', pk: true, nn: true, note: 'Order primary key.' },
        { name: 'customer_id', type: 'uuid', nn: true, note: 'Who placed the order → shop.customers.' },
        { name: 'status', type: 'order_status', nn: true, en: true, def: 'pending', note: 'Lifecycle state; see the order_status enum.' },
        { name: 'total_cents', type: 'integer', nn: true, def: '0', note: 'Order total in cents, summed from line items.' },
        { name: 'placed_at', type: 'timestamptz', nn: true, def: 'now()', note: 'When the order was submitted.' },
      ],
      indexes: [
        { def: '(customer_id)', name: 'orders_customer_id_idx' },
        { def: '(status, placed_at)', name: 'orders_status_placed_idx' },
      ],
    },
    {
      schema: 'shop', name: 'order_items', kind: 'table',
      noteMd: 'Line items for an order (order × product).',
      columns: [
        { name: 'id', type: 'bigserial', pk: true, nn: true, note: 'Line-item primary key.' },
        { name: 'order_id', type: 'uuid', nn: true, note: 'Parent order → shop.orders.' },
        { name: 'product_id', type: 'uuid', nn: true, note: 'Product purchased → shop.products.' },
        { name: 'qty', type: 'integer', nn: true, def: '1', note: 'Units ordered for this line.' },
      ],
    },
  ],
  refs: [
    { from: { s: 'auth', t: 'sessions', c: 'user_id' }, to: { s: 'auth', t: 'users', c: 'id' } },
    { from: { s: 'shop', t: 'customers', c: 'user_id' }, to: { s: 'auth', t: 'users', c: 'id' } },
    { from: { s: 'shop', t: 'orders', c: 'customer_id' }, to: { s: 'shop', t: 'customers', c: 'id' } },
    { from: { s: 'shop', t: 'order_items', c: 'order_id' }, to: { s: 'shop', t: 'orders', c: 'id' } },
    { from: { s: 'shop', t: 'order_items', c: 'product_id' }, to: { s: 'shop', t: 'products', c: 'id' } },
  ],
};

export const sampleEnums: EnumDef[] = [
  { schema: 'shop', name: 'order_status', values: ['pending', 'paid', 'shipped', 'cancelled'] },
];

export const sampleUser: User = { name: 'Sam Reyes', email: 'sam@example.dev', initials: 'SR' };
