// Account API helpers — plain async functions (not Svelte-specific).
// Matches the contract of apps/web/src/accounts/api.ts, sans the
// React hook wrappers (useTimeline, useAccountBundle) which re-emerge
// inline in the Svelte components as $effect blocks.

import type {
  AccountTeamMember,
  AccountTeamRole,
  Asset,
  Invoice,
  Job,
  NextAction,
  Account,
  AccountBundle,
  AccountNote,
  Shipment,
  TimelineEntry,
} from './types';
import { href } from '../router';
import { entityHref } from '@boss/web-kit/ui/entity-href';
import { formatMoney } from '@boss/web-kit/ui/money';
import { fetchPaged, type Paged, type PagedResult } from '../data/paginated';
import {
  AccountSchema,
  AccountTeamMemberListSchema,
  AccountNoteListSchema,
  AssetSchema,
  InvoiceSchema,
  JobSchema,
  NextActionListSchema,
  ShipmentSchema,
} from './schemas';
import { fetchPagedValidated, fetchValidated, type ParseResult } from '../data/parseResponse';
import { appNow } from '@boss/web-kit/sim-clock';

/// Thrown by loadAccountBundle (and the helpers it calls) when an
/// API response is reachable + 200 but doesn't match the expected
/// schema. AccountPage catches this and renders a server-error
/// state instead of crashing on the first `.foo` access. Network
/// errors / 4xx / 5xx still fall through to empty arrays the way
/// the pre-zod code did — that part wasn't a crash class.
export class AccountSchemaError extends Error {
  constructor(public readonly url: string, message: string) {
    super(`${url}: ${message}`);
    this.name = 'AccountSchemaError';
  }
}

function unwrapPaged<T>(
  url: string,
  result: ParseResult<{ data: ReadonlyArray<T>; total: number; limit: number; offset: number }>,
): Paged<T> | null {
  if (result.kind === 'ok') return result.data as Paged<T>;
  if (result.kind === 'invalid') throw new AccountSchemaError(url, result.reason);
  return null;
}
function unwrap<T>(url: string, result: ParseResult<T>): T | null {
  if (result.kind === 'ok') return result.data;
  if (result.kind === 'invalid') throw new AccountSchemaError(url, result.reason);
  return null;
}

const DEVICE_CAP = 500;
const INVOICE_CAP = 500;
const JOB_CAP = 500;
const SHIPMENT_CAP = 200;

export async function loadAccountBundle(
  accountId: string,
): Promise<AccountBundle | 'not-found'> {
  const account = await fetchAccount(accountId);
  if (!account) return 'not-found';

  const devicesUrl = `/api/assets?account_id=${q(accountId)}&limit=${DEVICE_CAP}`;
  const invoicesUrl = `/api/commerce/invoices?account_id=${q(accountId)}&limit=${INVOICE_CAP}`;
  const jobsUrl = `/api/jobs?subject_id=${q(accountId)}&limit=${JOB_CAP}`;
  const shipmentsUrl = `/api/shipping/shipments?account_id=${q(accountId)}&limit=${SHIPMENT_CAP}`;
  const nextActionsUrl = `/api/people/accounts/${q(accountId)}/next-actions`;
  const teamUrl = `/api/people/accounts/${q(accountId)}/account-team`;
  const notesUrl = `/api/people/accounts/${q(accountId)}/notes?limit=50`;

  const [
    devicesRes,
    invoicesRes,
    jobsRes,
    shipmentsRes,
    nextActionsRes,
    teamRes,
    notesRes,
  ] = await Promise.all([
    fetchPagedValidated(devicesUrl, AssetSchema),
    fetchPagedValidated(invoicesUrl, InvoiceSchema),
    fetchPagedValidated(jobsUrl, JobSchema),
    fetchPagedValidated(shipmentsUrl, ShipmentSchema),
    fetchValidated(nextActionsUrl, NextActionListSchema),
    fetchValidated(teamUrl, AccountTeamMemberListSchema),
    fetchValidated(notesUrl, AccountNoteListSchema),
  ]);

  const devices = unwrapPaged<Asset>(devicesUrl, devicesRes);
  const invoices = unwrapPaged<Invoice>(invoicesUrl, invoicesRes);
  const jobs = unwrapPaged<Job>(jobsUrl, jobsRes);
  const shipments = unwrapPaged<Shipment>(shipmentsUrl, shipmentsRes);
  const nextActions = unwrap<NextAction[]>(nextActionsUrl, nextActionsRes) ?? [];
  const team = unwrap<AccountTeamMember[]>(teamUrl, teamRes) ?? [];
  const notes = unwrap<AccountNote[]>(notesUrl, notesRes) ?? [];

  return {
    account,
    devices: [...(devices?.data ?? [])],
    invoices: [...(invoices?.data ?? [])],
    jobs: [...(jobs?.data ?? [])],
    shipments: [...(shipments?.data ?? [])],
    nextActions,
    team,
    notes,
    caps: {
      devices: capState(devices),
      invoices: capState(invoices),
      jobs: capState(jobs),
      shipments: capState(shipments),
    },
  };
}

function capState<T>(p: Paged<T> | null): { total: number; capped: boolean } {
  if (!p) return { total: 0, capped: false };
  return { total: p.total, capped: p.total > p.data.length };
}

export async function loadTimeline(
  accountId: string,
  windowDays: number,
): Promise<
  | { kind: 'ready'; entries: TimelineEntry[] }
  | { kind: 'failed'; error: string }
> {
  const cutoff = cutoffDate(windowDays);
  const [invoicesPage, jobsPage, shipmentsPage] = await Promise.all([
    fetchPaged<Invoice>(
      `/api/commerce/invoices?account_id=${q(accountId)}&limit=${INVOICE_CAP}`,
    ),
    fetchPaged<Job>(
      `/api/jobs?subject_id=${q(accountId)}&limit=${JOB_CAP}`,
    ),
    fetchPaged<Shipment>(
      `/api/shipping/shipments?account_id=${q(accountId)}&limit=${SHIPMENT_CAP}`,
    ),
  ]);
  // Any failed leg fails the timeline — merging the legs that
  // happened to arrive would render a partial history as if it were
  // the whole one, which is the same false-empty class one level up
  // (packet 3fba9c35).
  for (const page of [invoicesPage, jobsPage, shipmentsPage]) {
    if (page.kind === 'failed') return page;
  }
  const invoices = invoicesPage.kind === 'ready' ? invoicesPage.page.data : [];
  const jobs = jobsPage.kind === 'ready' ? jobsPage.page.data : [];
  const shipments = shipmentsPage.kind === 'ready' ? shipmentsPage.page.data : [];

  const merged: TimelineEntry[] = [];
  for (const inv of invoices) {
    if (inv.issued_on >= cutoff) {
      merged.push({
        id: `inv-issued-${inv.id}`,
        date: inv.issued_on,
        icon: '💰',
        title: `Invoice ${inv.id} issued — ${formatMoney({ amount_cents: inv.amount_cents, currency: inv.currency })}`,
        detail: inv.status,
        link: entityHref('invoice', inv.id),
      });
    }
    if (inv.paid_on && inv.paid_on >= cutoff) {
      merged.push({
        id: `inv-paid-${inv.id}`,
        date: inv.paid_on,
        icon: '✅',
        title: `Invoice ${inv.id} paid — ${formatMoney({ amount_cents: inv.amount_cents, currency: inv.currency })}`,
        link: entityHref('invoice', inv.id),
      });
    }
  }
  for (const job of jobs) {
    if (job.opened_on >= cutoff) {
      merged.push({
        id: `job-open-${job.id}`,
        date: job.opened_on,
        icon: job.kind === 'field-service' ? '🔧' : job.kind === 'sale' ? '🤝' : '🗂',
        title: `${kindLabel(job.kind)} opened — ${job.title}`,
        detail: job.status,
        link: entityHref('job', job.id),
      });
    }
    if (job.closed_on && job.closed_on >= cutoff) {
      merged.push({
        id: `job-close-${job.id}`,
        date: job.closed_on,
        icon: '✅',
        title: `${kindLabel(job.kind)} closed — ${job.title}`,
        link: entityHref('job', job.id),
      });
    }
  }
  for (const s of shipments) {
    if (s.delivered_on && s.delivered_on >= cutoff) {
      merged.push({
        id: `ship-${s.id}`,
        date: s.delivered_on,
        icon: '📦',
        title: `Shipment ${s.id} delivered`,
        detail: `${s.origin} → ${s.destination}`,
      });
    }
  }

  merged.sort((a, b) => b.date.localeCompare(a.date));
  return { kind: 'ready', entries: merged };
}

export async function loadContracts(
  accountId: string,
): Promise<ReadonlyArray<{ id: string; end_date: string }>> {
  const body = await fetchJson<{ data?: Array<{ id: string; end_date: string }> } | Array<{ id: string; end_date: string }>>(
    `/api/commerce/agreements?account_id=${q(accountId)}&limit=10`,
  );
  if (!body) return [];
  if (Array.isArray(body)) return body;
  return body.data ?? [];
}

export async function assignAccountTeamMember(input: {
  account_id: string;
  employee_id: string;
  role: AccountTeamRole;
  actor_id: string;
  notes?: string;
}): Promise<void> {
  const resp = await fetch(
    `/api/people/accounts/${q(input.account_id)}/account-team`,
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        employee_id: input.employee_id,
        role: input.role,
        actor_id: input.actor_id,
        notes: input.notes,
      }),
    },
  );
  if (!resp.ok) throw new Error(`${resp.status}: ${await resp.text()}`);
}

export async function unassignAccountTeamMember(input: {
  account_id: string;
  role: AccountTeamRole;
  actor_id: string;
}): Promise<void> {
  const url =
    `/api/people/accounts/${q(input.account_id)}/account-team/${q(input.role)}` +
    `?actor_id=${q(input.actor_id)}`;
  const resp = await fetch(url, { method: 'DELETE' });
  if (!resp.ok && resp.status !== 404) {
    throw new Error(`${resp.status}: ${await resp.text()}`);
  }
}

export async function createAccountNote(input: {
  account_id: string;
  actor_id: string;
  body: string;
  kind?: 'note' | 'call' | 'meeting' | 'email' | 'interaction';
}): Promise<AccountNote> {
  const resp = await fetch(
    `/api/people/accounts/${q(input.account_id)}/notes`,
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        kind: input.kind ?? 'note',
        body: input.body,
        actor_id: input.actor_id,
      }),
    },
  );
  if (!resp.ok) throw new Error(`${resp.status}: ${await resp.text()}`);
  const body = (await resp.json()) as { id: string };
  const now = appNow().toISOString();
  return {
    id: body.id,
    account_id: input.account_id,
    actor_id: input.actor_id,
    body: input.body,
    kind: input.kind ?? 'note',
    created_at: now,
    deleted_at: null,
  };
}

/// The account directory, one bounded page. `GET /api/people/accounts`
/// answered an unbounded bare array until backlog 2d1d298e
/// (2026-09-23), so seven pages each special-cased its shape and none
/// could tell a capped list from a complete one. It now answers the
/// `{data, total, limit, offset}` envelope; read it here, and render an
/// OverflowBanner on `isCapped(page)`. 1000 is the service's
/// MAX_LIST_LIMIT — a larger ask is clamped, and the envelope's own
/// `limit` and `total` stay truthful either way, so the banner does
/// not depend on this number matching the server's.
export const ACCOUNTS_LIST_URL = '/api/people/accounts?limit=1000';

export function fetchAccountsPage(): Promise<PagedResult<Account>> {
  return fetchPaged<Account>(ACCOUNTS_LIST_URL);
}

/// One account by id. This scanned the whole directory for the id
/// until 2d1d298e, which a bounded list read can no longer answer for
/// an account past the first page; the detail route is the question
/// actually being asked (it also carries `contacts`, ignored here).
async function fetchAccount(id: string): Promise<Account | null> {
  const url = `/api/people/accounts/${q(id)}`;
  const result = await fetchValidated(url, AccountSchema);
  return unwrap(url, result);
}

async function fetchJson<T>(url: string): Promise<T | null> {
  try {
    const resp = await fetch(url);
    if (!resp.ok) return null;
    return (await resp.json()) as T;
  } catch {
    return null;
  }
}

function q(s: string): string {
  return encodeURIComponent(s);
}

function cutoffDate(days: number): string {
  const d = appNow();
  d.setDate(d.getDate() - days);
  return d.toISOString().slice(0, 10);
}

function kindLabel(kind: string): string {
  return kind
    .split('-')
    .map((p) => (p.length === 0 ? p : p[0]!.toUpperCase() + p.slice(1)))
    .join(' ');
}
