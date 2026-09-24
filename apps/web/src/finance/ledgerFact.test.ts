import { afterEach, describe, it, expect } from 'bun:test';
import { loadEntryIdForFact } from './ledger';

// entity-href's `fact` kind links /ux/finance?fact=<id>, and the page
// now opens the journal entry that fact posted (backlog 2ab44d55). A
// fact is resolved to its entry through the ledger's own filter,
// GET /api/ledger/entries?fact_id=; a fact with no entry and a read
// that failed are different answers and must stay different.

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
});

function stubFetch(fn: (url: string) => Promise<Response>) {
  globalThis.fetch = ((input: RequestInfo | URL) => fn(String(input))) as unknown as typeof fetch;
}

const ENTRY = {
  id: 'ent-9', fact_id: 'f-9', posted_on: '2026-09-02', memo: null, rule_version: 1,
  fact_kind: 'manual', fact_source_table: null, fact_source_id: null,
};

describe('loadEntryIdForFact', () => {
  it('asks the ledger for the entries of that one fact, the id encoded', async () => {
    const seen: string[] = [];
    stubFetch(async (url) => {
      seen.push(url);
      return new Response(JSON.stringify([ENTRY]), { status: 200 });
    });
    const res = await loadEntryIdForFact('f 9');
    expect(seen).toEqual(['/api/ledger/entries?fact_id=f%209&limit=1']);
    expect(res).toEqual({ read: { kind: 'ok' }, entryId: 'ent-9' });
  });

  it('a fact that posted no entry is an ok read with no id', async () => {
    stubFetch(async () => new Response('[]', { status: 200 }));
    expect(await loadEntryIdForFact('f-9')).toEqual({ read: { kind: 'ok' }, entryId: null });
  });

  it('a refusal is a failed read, with the reason — never "no entry"', async () => {
    stubFetch(async () => new Response('bad fact_id', { status: 400 }));
    expect(await loadEntryIdForFact('nope')).toEqual({
      read: { kind: 'failed', error: 'HTTP 400: bad fact_id' },
      entryId: null,
    });
  });
});
