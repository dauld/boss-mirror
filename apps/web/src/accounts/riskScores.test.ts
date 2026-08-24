import { describe, expect, test } from 'bun:test';
import {
  accountsFromResponse,
  riskScoresFromResponse,
  type RiskScore,
} from './riskScores';

function score(over: Partial<RiskScore> = {}): RiskScore {
  return {
    account_id: 'acct-1',
    account_name: 'Northgate Brewing',
    score: 62,
    top_factor: 'no invoice in 210 days',
    factors: {
      days_since_last_invoice: 210,
      open_ticket_count: 3,
      has_active_contract: true,
      days_since_last_note: null,
    },
    ...over,
  };
}

describe('riskScoresFromResponse', () => {
  test('the declared envelope reads as ready, rows intact', () => {
    const s = riskScoresFromResponse(200, { accounts: [score()], total_scored: 1 });
    expect(s.kind).toBe('ready');
    if (s.kind !== 'ready') return;
    expect(s.scores).toHaveLength(1);
    expect(s.scores[0]!.account_name).toBe('Northgate Brewing');
    expect(s.scores[0]!.factors.days_since_last_note).toBeNull();
  });

  test('zero scored accounts is READY-and-empty, not an error', () => {
    // The page has an empty state and it is the truthful one here: the
    // service answered, and the answer was "nobody is at risk".
    const s = riskScoresFromResponse(200, { accounts: [], total_scored: 0 });
    expect(s.kind).toBe('ready');
    if (s.kind !== 'ready') return;
    expect(s.scores).toEqual([]);
  });

  test('a bare array where the envelope belongs never reaches the render', () => {
    // THE REGRESSION. `[]` is what the route-smoke harness's `/api/**`
    // catch-all answers with, and the page used to store `body.accounts`
    // — undefined — as its ready-state rows, which threw
    // "Cannot read properties of undefined (reading 'length')" on the
    // first render of the subtitle.
    const s = riskScoresFromResponse(200, []);
    expect(s.kind).toBe('error');
  });

  test('an unreadable body is an error, whatever shape it took', () => {
    for (const body of [null, undefined, 'accounts', 42, {}, { accounts: null }, { accounts: 'many' }]) {
      expect(riskScoresFromResponse(200, body).kind).toBe('error');
    }
  });

  test('a row missing its factors is unreadable, not a landmine in the table', () => {
    // The table dereferences `s.factors.open_ticket_count` on every row.
    // Admitting a row without `factors` only moves the same TypeError
    // one line down, so the payload is refused whole.
    const s = riskScoresFromResponse(200, {
      accounts: [score(), { account_id: 'a2', account_name: 'B', score: 1, top_factor: 'x' }],
    });
    expect(s.kind).toBe('error');
  });

  test('a failed request says which status it got', () => {
    const s = riskScoresFromResponse(503, null);
    expect(s.kind).toBe('error');
    if (s.kind !== 'error') return;
    expect(s.message).toContain('503');
  });

  test('a ready state always carries a real array — the crash class, stated', () => {
    const bodies: unknown[] = [
      [],
      null,
      {},
      { accounts: [score()] },
      { accounts: [] },
      { accounts: [{}] },
      { accounts: [score(), null] },
    ];
    for (const body of bodies) {
      const s = riskScoresFromResponse(200, body);
      if (s.kind === 'ready') expect(Array.isArray(s.scores)).toBe(true);
    }
  });
});

describe('accountsFromResponse', () => {
  test('reads both shapes the accounts endpoint has answered with', () => {
    const a = { id: 'acct-1', tier: 'gold', city: 'Portland' };
    expect(accountsFromResponse([a])).toHaveLength(1);
    expect(accountsFromResponse({ data: [a] })).toHaveLength(1);
  });

  test('an unreadable directory degrades the filters, never the page', () => {
    // This list only decorates the rows with tier + city. Losing it
    // must not cost the risk scores, so anything unreadable is simply
    // no directory.
    for (const body of [null, undefined, 'nope', {}, { data: 'nope' }]) {
      expect(accountsFromResponse(body)).toEqual([]);
    }
  });

  test('rows without an id are dropped, since id is what they are keyed by', () => {
    expect(accountsFromResponse([{ id: 'acct-1' }, {}, null, { id: 7 }])).toHaveLength(1);
  });
});
