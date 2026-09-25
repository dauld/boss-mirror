// Account-detail shapes. Server types; match the JSON shapes of
// the endpoints used by AccountPage.svelte and AccountsList.svelte.

import type { Subject } from '../jobs/types';

export type Account = {
  id: string;
  // Identity-first: only `id` is guaranteed. Every descriptive field is
  // enriched after the account exists, so each is nullable until set.
  name: string | null;
  director: string | null;
  city: string | null;
  state: string | null;
  // An (account, tier) Class code — a tenant adds a tier as one Class
  // row, so no closed union can name them (backlog d2c9e79f). `null`
  // is untiered, until classified.
  tier: string | null;
  customer_since: string | null;
  territory_rep_id: string | null;
};

export type Asset = {
  asset_id: string;
  sku: string;
  phase: string;
  account_id: string | null;
  warranty_through: string | null;
  open_ticket_count: number;
  first_seen: string;
  last_event_at: string;
  oem_serial: string | null;
};

export type Invoice = {
  id: string;
  account_id: string;
  status: string;
  amount_cents: number;
  currency: string;
  issued_on: string;
  due_on: string;
  paid_on: string | null;
};

/// One row of `GET /api/commerce/open-ar` (boss-commerce AccountOpenAr):
/// an account's open receivables, summed by the service over every
/// invoice it still owes. Accounts that owe nothing have no row.
export type AccountOpenAr = {
  account_id: string;
  open_ar_cents: number;
  open_count: number;
};

export type Job = {
  id: string;
  kind: string;
  title: string;
  status: string;
  priority: string;
  opened_on: string;
  closed_on: string | null;
  // Wave 6 collapse: shared Subject type from jobs/types.ts. The
  // old inline shape was an early loose-kind sketch that predates
  // the refactor; the canonical shape now lives in one place.
  subject: Subject;
  metadata: Record<string, unknown>;
};

export type Shipment = {
  id: string;
  status: string;
  origin: string;
  destination: string;
  expected_delivery: string | null;
  delivered_on: string | null;
};

export type AccountTeamMember = {
  /// Stable row id from `account_team_members.id`. Lets the UI
  /// key off a natural primary key instead of fabricating
  /// `${employee_id}-${role}` on every render.
  id: string;
  account_id: string;
  employee_id: string;
  role: string;
  /// ISO date. Wire field is `assigned_on` (see
  /// `boss-people/src/account_team_members.rs::AccountTeamMemberAssignment`).
  /// Matching name here prevents a latent mismatch that would
  /// render `undefined` in an "assigned on" column.
  assigned_on: string;
  notes: string | null;
  created_at: string;
};

export type AccountNote = {
  id: string;
  account_id: string;
  actor_id: string;
  body: string;
  kind: string;
  created_at: string;
  deleted_at: string | null;
};

export type NextAction = {
  title: string;
  detail?: string;
  severity?: 'critical' | 'warning' | 'info';
  link?: string;
};

export type AccountTeamRole =
  | 'territory-rep'
  | 'customer-success'
  | 'service-manager'
  | 'finance-contact'
  | 'executive-sponsor';

/// Canonical order for rendering and for the role-picker dropdown.
/// `territory-rep` first because it's the sales owner relationship
/// that anchors every other role. Mirrored from the DB CHECK
/// constraint in `infra/postgres/schema.sql`.
export const ACCOUNT_TEAM_ROLES: ReadonlyArray<AccountTeamRole> = [
  'territory-rep',
  'customer-success',
  'service-manager',
  'finance-contact',
  'executive-sponsor',
];

export const ACCOUNT_TEAM_ROLE_LABEL: Record<AccountTeamRole, string> = {
  'territory-rep': 'Territory rep',
  'customer-success': 'Customer success',
  'service-manager': 'Service manager',
  'finance-contact': 'Finance contact',
  'executive-sponsor': 'Executive sponsor',
};

export type AccountBundle = {
  account: Account;
  devices: Asset[];
  invoices: Invoice[];
  jobs: Job[];
  shipments: Shipment[];
  nextActions: NextAction[];
  team: AccountTeamMember[];
  notes: AccountNote[];
  /// Per-fetch totals for the cross-entity lists above. When the
  /// listed array is shorter than the matching total the SPA should
  /// surface "showing X of Y" so the drill-down doesn't lie about
  /// how complete the rollup is.
  caps: AccountBundleCaps;
};

export type AccountBundleCaps = {
  devices: { total: number; capped: boolean };
  invoices: { total: number; capped: boolean };
  jobs: { total: number; capped: boolean };
  shipments: { total: number; capped: boolean };
};

export type TimelineEntry = {
  id: string;
  date: string;
  icon: string;
  title: string;
  detail?: string;
  link?: string;
};
