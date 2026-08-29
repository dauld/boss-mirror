// Runtime schemas for the accounts API surfaces consumed by
// AccountPage + loadAccountBundle. Mirrors `apps/web/src/accounts/types.ts`.
//
// See `apps/web/src/data/parseResponse.ts` for the helper.

import { z } from '../data/parseResponse';

/// Account tier. DB CHECK constraint pins this to platinum/gold/silver
/// today; matching the TS type's closed enum so a tenant inserting an
/// out-of-set tier surfaces the bug instead of silently mis-rendering
/// the TierChip. Widen later if the DB constraint widens.
export const AccountSchema = z.object({
  id: z.string(),
  // Identity-first: only `id` is guaranteed; descriptive fields are
  // nullable until the account is enriched.
  name: z.string().nullable(),
  director: z.string().nullable(),
  city: z.string().nullable(),
  state: z.string().nullable(),
  tier: z.enum(['platinum', 'gold', 'silver']).nullable(),
  customer_since: z.string().nullable(),
  territory_rep_id: z.string().nullable(),
});
export const AccountListSchema = z.array(AccountSchema);

/// `GET /api/assets?account_id=…`. `account_id`,
/// `warranty_through`, `oem_serial` are all legitimately nullable on
/// the wire — unassigned devices and OEMs that don't issue
/// serials. Modeling them as `.nullable()` here prevents the same
/// class of crash that bit catalog (extras: null).
export const AssetSchema = z.object({
  asset_id: z.string(),
  sku: z.string(),
  phase: z.string(),
  account_id: z.string().nullable(),
  warranty_through: z.string().nullable(),
  open_ticket_count: z.number(),
  first_seen: z.string(),
  last_event_at: z.string(),
  oem_serial: z.string().nullable(),
});

export const InvoiceSchema = z.object({
  id: z.string(),
  account_id: z.string(),
  status: z.string(),
  amount_cents: z.number(),
  currency: z.string(),
  issued_on: z.string(),
  due_on: z.string(),
  paid_on: z.string().nullable(),
});

/// Matches `apps/web/src/jobs/types.ts::Subject`. The wire payload
/// uses `subject_kind` (not `kind`) alongside a plain `id`. Keeping the
/// schema permissive (passthrough) since this is a hot-path shape we
/// don't want to gate the bundle on.
const SubjectSchema = z.object({
  subject_kind: z.string(),
  id: z.string(),
}).passthrough();

export const JobSchema = z.object({
  id: z.string(),
  kind: z.string(),
  title: z.string(),
  status: z.string(),
  priority: z.string(),
  opened_on: z.string(),
  closed_on: z.string().nullable(),
  subject: SubjectSchema,
  metadata: z.record(z.string(), z.unknown()).default({}),
});

export const ShipmentSchema = z.object({
  id: z.string(),
  status: z.string(),
  origin: z.string(),
  destination: z.string(),
  expected_delivery: z.string().nullable(),
  delivered_on: z.string().nullable(),
});

export const AccountTeamMemberSchema = z.object({
  id: z.string(),
  account_id: z.string(),
  employee_id: z.string(),
  role: z.string(),
  assigned_on: z.string(),
  notes: z.string().nullable(),
  created_at: z.string(),
});
export const AccountTeamMemberListSchema = z.array(AccountTeamMemberSchema);

export const AccountNoteSchema = z.object({
  id: z.string(),
  account_id: z.string(),
  actor_id: z.string(),
  body: z.string(),
  kind: z.string(),
  created_at: z.string(),
  deleted_at: z.string().nullable(),
});
export const AccountNoteListSchema = z.array(AccountNoteSchema);

/// `GET /api/people/accounts/{id}/next-actions` returns the server's
/// `Action` shape (boss-accounts `account_next_actions.rs`): kebab-case
/// `severity` + `headline` / `context` / `deep_link` (plus `id` / `rule`
/// / `due_on`). The action card renders the UI vocabulary
/// (`title` / `detail` / `severity` / `link`), so validate the wire
/// honestly and transform it to that shape — the component stays in UI
/// terms and a real wire drift still surfaces. Empty `context` /
/// `deep_link` collapse to `undefined` so the card hides them.
export const NextActionSchema = z
  .object({
    id: z.string(),
    rule: z.string(),
    severity: z.enum(['critical', 'warning', 'info']),
    headline: z.string(),
    context: z.string(),
    deep_link: z.string(),
    due_on: z.string().nullish(),
  })
  .transform((a) => ({
    title: a.headline,
    detail: a.context.length > 0 ? a.context : undefined,
    severity: a.severity,
    link: a.deep_link.length > 0 ? a.deep_link : undefined,
  }));
export const NextActionListSchema = z.array(NextActionSchema);

/// The watchlist's risk scores (`GET /api/accounts/risk-scores`).
///
/// WHY THIS EXISTS. WatchlistPage cast the payload —
/// `(await r.json()) as { accounts: RiskScore[] }` — and then read
/// `.length` off the result. A response without an `accounts` key made
/// `scores` undefined and the page died with "Cannot read properties of
/// undefined (reading 'length')", which is precisely the failure
/// parseResponse.ts was written about (feedback 2fe1c8c1). The page's
/// own old comment had predicted it.
///
/// `factors` is permissive on purpose: the page reads a handful of keys
/// off it and tolerates absent ones, so pinning every field here would
/// turn a harmless backend addition into a hard parse failure. The
/// shape that MATTERS is `accounts` being an array of objects each
/// carrying an id and a numeric score — that is what the table indexes,
/// sorts and counts.
export const RiskScoreSchema = z.object({
  account_id: z.string(),
  account_name: z.string(),
  score: z.number(),
  top_factor: z.string(),
  factors: z.record(z.string(), z.unknown()).optional(),
});

export const RiskScoreListSchema = z.object({
  accounts: z.array(RiskScoreSchema),
});
