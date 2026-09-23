import { afterEach, describe, it, expect } from 'bun:test';
import { ACCOUNTS_LIST_URL, fetchAccountsPage, loadAccountBundle } from './api';

// Backlog 2d1d298e (2026-09-23): GET /api/people/accounts answered an
// unbounded bare array, so every page that read it had to special-case
// the shape and none could say when the list was incomplete. It now
// answers the paged envelope, and the directory read is one helper.

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
});

function stubFetch(fn: (url: string) => Promise<Response>) {
  globalThis.fetch = ((input: RequestInfo | URL) => fn(String(input))) as unknown as typeof fetch;
}

const ACCT = {
  id: 'acct-1',
  name: 'One',
  director: null,
  city: null,
  state: null,
  tier: null,
  customer_since: null,
  territory_rep_id: null,
  account_type: 'unspecified',
};

describe('fetchAccountsPage', () => {
  it('asks for a bounded page and hands back the envelope, total included', async () => {
    const seen: string[] = [];
    stubFetch(async (url) => {
      seen.push(url);
      return new Response(JSON.stringify({ data: [ACCT], total: 1500, limit: 1000, offset: 0 }), {
        status: 200,
      });
    });
    const res = await fetchAccountsPage();
    expect(seen).toEqual([ACCOUNTS_LIST_URL]);
    expect(ACCOUNTS_LIST_URL).toMatch(/^\/api\/people\/accounts\?limit=\d+$/);
    expect(res.kind).toBe('ready');
    if (res.kind === 'ready') expect(res.page.total).toBe(1500);
  });

  it('a failed read is failed, never an empty directory', async () => {
    stubFetch(async () => new Response('down', { status: 503 }));
    const res = await fetchAccountsPage();
    expect(res.kind).toBe('failed');
  });
});

describe('loadAccountBundle', () => {
  it('reads the one account by id rather than scanning the directory', async () => {
    const seen: string[] = [];
    stubFetch(async (url) => {
      seen.push(url);
      if (url === '/api/people/accounts/acct-1') {
        return new Response(JSON.stringify({ ...ACCT, contacts: [] }), { status: 200 });
      }
      return new Response('nope', { status: 404 });
    });
    await loadAccountBundle('acct-1').catch(() => undefined);
    expect(seen[0]).toBe('/api/people/accounts/acct-1');
    expect(seen).not.toContain('/api/people/accounts');
  });

  it('an account the service does not hold is not-found', async () => {
    stubFetch(async () => new Response('nope', { status: 404 }));
    expect(await loadAccountBundle('acct-missing')).toBe('not-found');
  });
});
