import { afterEach, describe, it, expect } from 'bun:test';
import { ENTRIES_PER_ACCOUNT_CAP, loadEntriesForAccount, loadPeriods } from './ledger';

// Backlog 1a2b67c9 (page audit 3f964c57, /ux/finance, 2026-09-23):
// loadPeriods and loadEntriesForAccount folded a failed read into [],
// so a ledger outage painted "No periods yet." and "No entries for this
// account." — the page's normal empty states. Each now hands back the
// read's outcome beside its rows, and the reason with it.

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
});

function stubFetch(fn: (url: string) => Promise<Response>) {
  globalThis.fetch = ((input: RequestInfo | URL) => fn(String(input))) as unknown as typeof fetch;
}

const PERIOD = {
  id: 'per-2026-09', kind: 'month', starts_on: '2026-09-01', ends_on: '2026-09-30',
  status: 'open', locked_at: null, locked_by: null, locked_rule_version: null,
  locked_checksum: null, entry_count: 3, total_debits: 100, total_credits: 100,
};

const ENTRY = {
  id: 'ent-1', fact_id: 'fact-1', posted_on: '2026-09-02', memo: null, rule_version: 1,
  fact_kind: 'manual', fact_source_table: null, fact_source_id: null,
};

describe('loadPeriods', () => {
  it('keeps a refusal as a failed read, with the status and the server text', async () => {
    stubFetch(async () => new Response('ledger down', { status: 500 }));
    const res = await loadPeriods();
    expect(res.read).toEqual({ kind: 'failed', error: 'HTTP 500: ledger down' });
    expect(res.periods).toEqual([]);
  });

  it('keeps a network error as a failed read', async () => {
    stubFetch(async () => {
      throw new Error('connection refused');
    });
    const res = await loadPeriods();
    expect(res.read).toEqual({ kind: 'failed', error: 'connection refused' });
  });

  it('an empty ledger is an ok read with no rows — the only true "No periods yet."', async () => {
    stubFetch(async () => new Response('[]', { status: 200 }));
    const res = await loadPeriods();
    expect(res.read).toEqual({ kind: 'ok' });
    expect(res.periods).toEqual([]);
  });

  it('hands the rows back on a healthy read', async () => {
    stubFetch(async () => new Response(JSON.stringify([PERIOD]), { status: 200 }));
    const res = await loadPeriods();
    expect(res.read).toEqual({ kind: 'ok' });
    expect(res.periods.map((p) => p.id)).toEqual(['per-2026-09']);
  });
});

describe('loadEntriesForAccount', () => {
  it('keeps a refusal as a failed read, never an uncapped empty page', async () => {
    stubFetch(async () => new Response('ledger down', { status: 503 }));
    const res = await loadEntriesForAccount('1000');
    expect(res.read).toEqual({ kind: 'failed', error: 'HTTP 503: ledger down' });
    expect(res.data).toEqual([]);
    expect(res.capped).toBe(false);
  });

  it('an account with no entries is an ok read', async () => {
    stubFetch(async () => new Response('[]', { status: 200 }));
    const res = await loadEntriesForAccount('1000');
    expect(res.read).toEqual({ kind: 'ok' });
    expect(res.data).toEqual([]);
  });

  it('no account selected reads nothing and is not a failure', async () => {
    const seen: string[] = [];
    stubFetch(async (url) => {
      seen.push(url);
      return new Response('[]', { status: 200 });
    });
    const res = await loadEntriesForAccount(null);
    expect(seen).toEqual([]);
    expect(res.read).toEqual({ kind: 'ok' });
  });

  it('still trims an over-cap page and marks it capped', async () => {
    const rows = Array.from({ length: ENTRIES_PER_ACCOUNT_CAP + 1 }, (_, i) => ({ ...ENTRY, id: `ent-${i}` }));
    stubFetch(async () => new Response(JSON.stringify(rows), { status: 200 }));
    const res = await loadEntriesForAccount('1000');
    expect(res.read).toEqual({ kind: 'ok' });
    expect(res.data.length).toBe(ENTRIES_PER_ACCOUNT_CAP);
    expect(res.capped).toBe(true);
  });
});
