import { describe, expect, test } from 'bun:test';
import {
  accountHealthView,
  deviceCellMeaning,
  ESCALATION_DAYS,
  failedRead,
  isEscalated,
  okRead,
  orderedHealthRows,
} from './reads';

// Packets 325f34cd and 8c9e1190, both from the /ux/support page audit
// (9876ef0d). The page already learned this lesson ONCE for its primary
// read — packet 3fba9c35 added `loadFailed` so a cases outage stops
// reading as "no open cases" — and the two SECONDARY reads kept the old
// shape beside it:
//
//   const pBody = pResp.ok ? await pResp.json() : [];      // accounts
//   devicesPage = dPaged.kind === 'ready' ? dPaged.page : null;  // devices
//
// The first turns any non-2xx into an empty account list, and the
// Account Health tab is BUILT from that list, so a total outage renders
// "No account data." — the page's normal empty state, with no banner
// and nothing for an operator to notice. The second discards the
// `failed` arm that PagedResult exists to carry, so a 500 on /api/assets
// renders every device cell as an em-dash, identical on screen to "this
// case has no device".
//
// Silence is the one forbidden failure mode. These are the two places
// this page manufactures it.

describe('the Account Health tab distinguishes an outage from an empty set', () => {
  test('a cases failure still wins — it is the primary read', () => {
    const v = accountHealthView(failedRead('cases boom'), okRead, 0);
    expect(v).toEqual({ kind: 'cases-failed', error: 'cases boom' });
  });

  test('an accounts failure is NOT rendered as an empty account list', () => {
    // The whole defect in one assertion: zero rows because the read
    // failed must not produce the same view as zero rows because there
    // are none.
    const v = accountHealthView(okRead, failedRead('HTTP 500'), 0);
    expect(v).toEqual({ kind: 'accounts-failed', error: 'HTTP 500' });
    expect(v).not.toEqual({ kind: 'empty' });
  });

  test('an accounts failure is reported even when rows happened to build', () => {
    // Rows can be non-empty from a previous render or a partial join;
    // the read still failed and the numbers are not to be trusted.
    const v = accountHealthView(okRead, failedRead('HTTP 503'), 7);
    expect(v.kind).toBe('accounts-failed');
  });

  test('genuinely empty stays empty', () => {
    expect(accountHealthView(okRead, okRead, 0)).toEqual({ kind: 'empty' });
  });

  test('rows render when both reads are good', () => {
    expect(accountHealthView(okRead, okRead, 3)).toEqual({ kind: 'rows' });
  });
});

describe('a device em-dash says which kind of nothing it is', () => {
  test('a good read means the case genuinely has no device', () => {
    expect(deviceCellMeaning(okRead)).toBe('no-device');
  });

  test('a failed read means we could not say', () => {
    // isCapped(null) is false, so the overflow banner cannot fire
    // either — before this, the page had no surface at all that could
    // report the read failed.
    expect(deviceCellMeaning(failedRead('HTTP 500'))).toBe('unknown');
  });
});

describe('the read states carry their reason', () => {
  test('a failure keeps the error text for the operator to read', () => {
    const r = failedRead('/api/assets: HTTP 500');
    expect(r.kind).toBe('failed');
    expect(r.kind === 'failed' && r.error).toBe('/api/assets: HTTP 500');
  });
});

// Packet 4c708662, gap 7 of the same page audit. accountHealthRows
// filtered to `openCount > 0`, and openCount counts open field-service
// jobs — which are structurally zero, because no such workflow is
// published. So the Account Health tab rendered "No account data."
// forever WHILE ACCOUNT DATA EXISTED.
//
// The packet offers two readings: the words are wrong, or the filter
// is. The filter is. The table's own columns are account, tier,
// openCount, deviceCount and lastDate — three of the five are
// meaningful for an account with no open case, and a tab called
// Account Health that hides every healthy account hides the roster.
// Removing it also makes the empty state's words true again: "no
// account data" is then exactly what it says.
describe('Account Health shows accounts, not only accounts in trouble', () => {
  const row = (name: string, openCount: number) => ({
    account: { name },
    openCount,
  });

  test('an account with no open case is still shown', () => {
    const rows = orderedHealthRows([row('Anonymous Sponsor', 0)]);
    expect(rows).toHaveLength(1);
    expect(rows.map((r) => r.account.name)).toEqual(['Anonymous Sponsor']);
  });

  test('accounts needing attention sort to the top', () => {
    // Removing the filter must not cost the worklist reading it had:
    // whatever WAS visible before stays visible and stays first.
    const rows = orderedHealthRows([
      row('Quiet', 0),
      row('Busy', 3),
      row('Some', 1),
    ]);
    expect(rows.map((r) => r.account.name)).toEqual(['Busy', 'Some', 'Quiet']);
  });

  test('ties break by name, so the order is stable to read', () => {
    const rows = orderedHealthRows([row('Beta', 0), row('Alpha', 0)]);
    expect(rows.map((r) => r.account.name)).toEqual(['Alpha', 'Beta']);
  });

  test('no accounts is still genuinely empty', () => {
    expect(orderedHealthRows([])).toHaveLength(0);
  });

  test('the input is not mutated', () => {
    const input = [row('Quiet', 0), row('Busy', 3)];
    orderedHealthRows(input);
    expect(input.map((r) => r.account.name)).toEqual(['Quiet', 'Busy']);
  });

  // Backlog ae7d1ce4. The page now types its accounts from the accounts
  // domain, which is identity-first: name is null until enriched. The
  // tie-break called name.localeCompare, so one unnamed account among
  // equals threw and took the tab down with it.
  test('an unnamed account breaks ties without throwing, before the named ones', () => {
    const rows = orderedHealthRows([
      { account: { name: 'Beta' }, openCount: 0 },
      { account: { name: null }, openCount: 0 },
      { account: { name: 'Alpha' }, openCount: 0 },
    ]);
    expect(rows.map((r) => r.account.name)).toEqual([null, 'Alpha', 'Beta']);
  });
});

// Packet e0a40c81, gap 11 of the same audit. `daysOpen > 14` decided
// both the escalatedCount tile and the amber row styling, as a literal
// in two expressions, with the tile's LABEL carrying a third copy of
// the same 14. No protocol row declares that rule, so the page was
// inventing an operating rule a tenant cannot change without editing
// the frontend. The real fix is data — the threshold belongs on the
// workflow that defines a support case — and that is blocked behind
// gap 1: no such workflow is published. Until it is, the number lives
// once and is pinned here, so it cannot drift silently between the two
// call sites and the words on the tile.
describe('the escalation threshold is named, not magic', () => {
  test('the threshold is 14 days — pinned, so a change is deliberate', () => {
    expect(ESCALATION_DAYS).toBe(14);
  });

  test('escalated means STRICTLY older than the threshold', () => {
    // The boundary the two literals encoded: 14 days open is not yet
    // escalated, 15 is. Asserted because a reader of `> 14` cannot tell
    // whether the 14th day was meant to count.
    expect(isEscalated(ESCALATION_DAYS - 1)).toBe(false);
    expect(isEscalated(ESCALATION_DAYS)).toBe(false);
    expect(isEscalated(ESCALATION_DAYS + 1)).toBe(true);
  });

  test('the page reads the constant — no copy of the number survives', async () => {
    // The drift pin. A named constant that two call sites bypass is no
    // better than the literal, and the tile's label is the copy most
    // likely to be left behind: it is prose, so nothing else would ever
    // flag it.
    const src = await Bun.file(
      new URL('./SupportPage.svelte', import.meta.url),
    ).text();
    const escalation = src
      .split('\n')
      .filter((l) => /\bdaysOpen\b|Escalated|escalatedCount/.test(l));
    expect(escalation.length).toBeGreaterThan(0);
    for (const line of escalation) {
      expect(line).not.toMatch(/\b14\b/);
    }
    expect(src).toMatch(/ESCALATION_DAYS/);
    expect(src).toMatch(/isEscalated\(/);
  });
});
