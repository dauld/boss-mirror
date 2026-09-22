import { describe, expect, test } from 'bun:test';
import {
  blankMeaning,
  failedRead,
  listView,
  okRead,
  readStateOf,
  readStateOfResponse,
  type ReadState,
} from './readState';

// Packet 7a7bfc88. The false-empty class is live once per page:
//
//   const pBody = pResp.ok ? await pResp.json() : [];            // 5 pages
//   devicesPage = dPaged.kind === 'ready' ? dPaged.page : null;  // AccountsList
//
// Each turns a non-2xx into the page's normal empty state, with no
// banner and nothing for an operator to notice. The support page's fix
// (apps/web/src/support/reads.ts) works; copied six more times it would
// be six copies of one idea (CLAUDE.md 9a). This module is that idea
// lifted, so adopting it on each page is mechanical.

describe('a read remembers whether it worked, and why not', () => {
  test('a failure keeps the reason — the operator is who has to act on it', () => {
    const r = failedRead('/api/assets: HTTP 500');
    expect(r.kind).toBe('failed');
    expect(r.kind === 'failed' && r.error).toBe('/api/assets: HTTP 500');
  });
});

describe('a raw fetch Response becomes a read state', () => {
  // The five `pResp.ok ? await pResp.json() : []` sites all fetch with
  // bare `fetch`, so this is the adapter each of them needs. The error
  // text matches fetchPaged / fetchRemote so one banner can render any
  // of the three.
  test('a 2xx is a good read', () => {
    expect(readStateOfResponse('/api/people/accounts', { ok: true, status: 200 })).toEqual(okRead);
  });

  test('a non-2xx is failed and names the url and the status', () => {
    const r = readStateOfResponse('/api/people/accounts', { ok: false, status: 503 });
    expect(r.kind).toBe('failed');
    expect(r.kind === 'failed' && r.error).toBe('/api/people/accounts: HTTP 503');
  });
});

describe('a PagedResult or a Remote becomes a read state', () => {
  // AccountsList discards this arm three times over
  // (`dPaged.kind === 'ready' ? dPaged.page : null`), which is the half
  // of the class that leaves no error text anywhere on the page.
  test('ready is a good read', () => {
    const paged = {
      kind: 'ready',
      page: { data: [], total: 0, limit: 0, offset: 0 },
    } as const;
    expect(readStateOf(paged)).toEqual(okRead);
  });

  test('failed carries the error across', () => {
    const paged = { kind: 'failed', error: '/api/assets: HTTP 500' } as const;
    expect(readStateOf(paged)).toEqual(failedRead('/api/assets: HTTP 500'));
  });

  test('loading is not a failure — a Remote may still be in flight', () => {
    const remote = { kind: 'loading' } as const;
    expect(readStateOf(remote)).toEqual(okRead);
  });
});

describe('a list view refuses to let a failed read wear the empty state', () => {
  const good = (source: string) => ({ source, state: okRead });
  const bad = (source: string, error: string) => ({
    source,
    state: failedRead(error),
  });

  test('a failure is reported instead of the empty state', () => {
    // The whole defect in one assertion: zero rows because the read
    // failed must not produce the same view as zero rows because there
    // are none.
    const v = listView([bad('accounts', 'HTTP 500')], 0);
    expect(v).toEqual({ kind: 'failed', source: 'accounts', error: 'HTTP 500' });
    expect(v).not.toEqual({ kind: 'empty' });
  });

  test('a failure is reported even when rows happened to build', () => {
    // Rows can stand from a partial join or a previous render; the read
    // still failed and the numbers are not to be trusted. This is the
    // quieter half of the bug.
    const v = listView([good('jobs'), bad('assets', 'HTTP 503')], 7);
    expect(v.kind).toBe('failed');
  });

  test('the first failure in the declared order wins', () => {
    // Declaration order is the page's own ranking: its primary dataset
    // first, so the banner names the read an operator should chase.
    const v = listView([bad('jobs', 'boom'), bad('accounts', 'HTTP 500')], 0);
    expect(v).toEqual({ kind: 'failed', source: 'jobs', error: 'boom' });
  });

  test('genuinely empty stays empty', () => {
    expect(listView([good('jobs'), good('accounts')], 0)).toEqual({
      kind: 'empty',
    });
  });

  test('rows render when every read is good', () => {
    expect(listView([good('jobs'), good('accounts')], 3)).toEqual({
      kind: 'rows',
    });
  });

  test('a page with no reads declared still answers honestly', () => {
    expect(listView([], 0)).toEqual({ kind: 'empty' });
  });
});

describe('a blank cell says which kind of nothing it is', () => {
  // isCapped(null) is false, so with the failed arm discarded a page
  // has no surface at all that can report the read failed — the em-dash
  // in an AccountsList device cell reads as "no device" either way.
  test('a good read means the value is genuinely absent', () => {
    expect(blankMeaning(okRead)).toBe('absent');
  });

  test('a failed read means we could not say', () => {
    expect(blankMeaning(failedRead('HTTP 500'))).toBe('unknown');
  });

  test('every state is one of the two — no third reading', () => {
    const states: ReadState[] = [okRead, failedRead('x')];
    expect(states.map(blankMeaning)).toEqual(['absent', 'unknown']);
  });
});
