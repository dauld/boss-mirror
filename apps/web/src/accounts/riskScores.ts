// The churn watchlist's data edge: the shapes, the pure classifier that
// turns a response into a render state, and the two thin fetches.
//
// This exists because WatchlistPage read `body.accounts` straight off an
// unvalidated `await res.json()` and stored it as its ready-state rows.
// When the field was missing — a backend answering a bare `[]`, an error
// envelope, a version that renamed the key — `scores` became `undefined`
// and the FIRST thing the ready branch renders is `${rows.length}`, so
// the page died with "Cannot read properties of undefined (reading
// 'length')". A `try` around the fetch could never catch it: nothing
// throws while parsing, only later, during render.
//
// So the shape check happens once, here, at the boundary, and the
// discriminated union the page already had becomes worth what it costs:
// `{ kind: 'ready' }` is now a proof that `scores` is an array of rows
// the table can actually dereference.

import type { Account } from './types';

export type RiskFactors = Readonly<{
  days_since_last_invoice: number | null;
  open_ticket_count: number;
  has_active_contract: boolean;
  days_since_last_note: number | null;
}>;

export type RiskScore = Readonly<{
  account_id: string;
  account_name: string;
  score: number;
  top_factor: string;
  factors: RiskFactors;
}>;

/// What the page knows about its rows. `ready` guarantees an array.
export type RiskScoresState =
  | { kind: 'loading' }
  | { kind: 'error'; message: string }
  | { kind: 'ready'; scores: ReadonlyArray<RiskScore> };

const UNREADABLE =
  'The risk-score service answered in a shape this page cannot read.';

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v);
}

function nullableNumber(v: unknown): number | null | undefined {
  if (v === null) return null;
  return typeof v === 'number' ? v : undefined;
}

/// One row, or null when it is not the row the table renders.
function toRiskScore(v: unknown): RiskScore | null {
  if (!isRecord(v) || !isRecord(v['factors'])) return null;
  const f = v['factors'];
  const invoice = nullableNumber(f['days_since_last_invoice']);
  const note = nullableNumber(f['days_since_last_note']);
  if (
    typeof v['account_id'] !== 'string' ||
    typeof v['account_name'] !== 'string' ||
    typeof v['score'] !== 'number' ||
    typeof v['top_factor'] !== 'string' ||
    typeof f['open_ticket_count'] !== 'number' ||
    typeof f['has_active_contract'] !== 'boolean' ||
    invoice === undefined ||
    note === undefined
  ) {
    return null;
  }
  return {
    account_id: v['account_id'],
    account_name: v['account_name'],
    score: v['score'],
    top_factor: v['top_factor'],
    factors: {
      days_since_last_invoice: invoice,
      open_ticket_count: f['open_ticket_count'],
      has_active_contract: f['has_active_contract'],
      days_since_last_note: note,
    },
  };
}

/// Pure classification of the risk-score endpoint's answer.
///
/// An unreadable ROW fails the whole payload rather than being dropped.
/// Dropping would render a shorter list than the service sent and say
/// nothing about it — a count in the subtitle that quietly lies — and
/// admitting the row only moves the same TypeError down to
/// `s.factors.open_ticket_count`. "I could not read this" is the one
/// answer that is true in every case.
export function riskScoresFromResponse(status: number, body: unknown): RiskScoresState {
  if (status < 200 || status >= 300) {
    return { kind: 'error', message: `The risk-score service answered ${status}.` };
  }
  if (!isRecord(body) || !Array.isArray(body['accounts'])) {
    return { kind: 'error', message: UNREADABLE };
  }
  const rows = body['accounts'].map(toRiskScore);
  if (rows.some((r) => r === null)) return { kind: 'error', message: UNREADABLE };
  return { kind: 'ready', scores: rows as ReadonlyArray<RiskScore> };
}

/// The account directory that decorates rows with tier + city. It is
/// enrichment: an unreadable answer costs the tier filter and nothing
/// else, so it degrades to "no directory" instead of failing the page.
/// Both shapes the endpoint has served are accepted — a bare array and
/// a `{data}` envelope.
export function accountsFromResponse(body: unknown): ReadonlyArray<Account> {
  const rows = Array.isArray(body)
    ? body
    : isRecord(body) && Array.isArray(body['data'])
      ? body['data']
      : [];
  return rows.filter(
    (a): a is Account => isRecord(a) && typeof a['id'] === 'string',
  );
}

// ---------------------------------------------------------------------------
// I/O edge — thin fetches over the pure mappers above.
// ---------------------------------------------------------------------------

export async function fetchRiskScores(): Promise<RiskScoresState> {
  try {
    const r = await fetch('/api/people/accounts/risk-scores?limit=200&min_score=0');
    const body: unknown = await r.json().catch(() => null);
    return riskScoresFromResponse(r.status, body);
  } catch {
    return { kind: 'error', message: 'Could not reach the risk-score service.' };
  }
}

export async function fetchAccountDirectory(): Promise<ReadonlyArray<Account>> {
  try {
    const r = await fetch('/api/people/accounts');
    if (!r.ok) return [];
    return accountsFromResponse(await r.json().catch(() => null));
  } catch {
    return [];
  }
}
