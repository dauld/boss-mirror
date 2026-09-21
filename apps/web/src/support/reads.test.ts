import { describe, expect, test } from 'bun:test';
import {
  accountHealthView,
  deviceCellMeaning,
  failedRead,
  okRead,
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
