// Where clicking a Subject lands. Every branch of `subjectPath` goes to
// the Subject's OWN page or the Subject's OWN packets; none of them may
// go to a workflow kind, because a kind is tenant data and this file is
// shared by every instance (backlog 423a531d).
//
// The campaign branch did: it returned `/jobs?kind=marketing-motion`, a
// workflow one tenant's seed bundle authors, so clicking a campaign on
// an instance running another tenant landed on an empty list. The honest
// destination is the campaign's packets, whatever kind they are — the
// same `subject_id` filter the router already parses on /jobs.
//
// And none may go to `'#'` (backlog 4af37dd8, page audit 473f4f92 GAP 2):
// custom and every kind the switch did not name returned it, `href('#')`
// is `/#`, which parses as `/`, so a Subject click landed on Home — for
// every open packet (286 of 286 `custom` at the audit, 322 of 322 when
// this was built, 2026-09-24). Subject
// kinds are registry data (boss-subject-kinds); a kind with no page of
// its own lands on its own packets without a branch naming it.
import { describe, expect, it } from 'bun:test';
import { subjectPath } from './types';
import { parseRoute, type Route } from '../router';
import type { Subject } from './types';

const subject = (subject_kind: string, id: string): Subject =>
  ({ subject_kind, id }) as unknown as Subject;

describe('subjectPath', () => {
  it('sends a campaign to its own packets, not to a workflow kind', () => {
    expect(subjectPath(subject('campaign', 'camp-2026-spring'))).toBe(
      '/ux/jobs?subject_id=camp-2026-spring',
    );
  });

  it('url-encodes a campaign id', () => {
    expect(subjectPath(subject('campaign', 'camp a/b'))).toBe('/ux/jobs?subject_id=camp%20a%2Fb');
  });

  it('sends a custom subject to its own packets, not to Home', () => {
    expect(subjectPath(subject('custom', '/ux/jobs'))).toBe('/ux/jobs?subject_id=%2Fux%2Fjobs');
  });

  it('sends a registry kind this file never names to its own packets', () => {
    // `recipe` is a brewery-tenant row, `department` a platform one
    // added after this switch was written; neither has a case here.
    expect(subjectPath(subject('recipe', 'west-coast-ipa'))).toBe(
      '/ux/jobs?subject_id=west-coast-ipa',
    );
    expect(subjectPath(subject('department', 'it'))).toBe('/ux/jobs?subject_id=it');
  });

  it('names no workflow kind on any branch', () => {
    const kinds = ['asset', 'account', 'purchase_order', 'campaign', 'employee', 'vendor', 'custom'];
    for (const k of kinds) {
      expect(subjectPath(subject(k, 'x'))).not.toContain('kind=');
    }
  });

  // The branches spelled the router's unprefixed legacy paths — /jobs,
  // /accounts, /assets — which parse only because the router strips
  // `/ux` "defensively"; every other link in the app is the canonical
  // /ux/… (backlog d0b93b80, found by the /ux/jobs page audit's spec).
  it('spells every branch under /ux, the way entityHref does, to a route the router serves', () => {
    const cases: ReadonlyArray<[string, string, Route['kind']]> = [
      ['asset', '/ux/assets/x', 'asset'],
      ['account', '/ux/accounts/x', 'account'],
      ['purchase_order', '/ux/purchase-orders/x', 'po'],
      ['employee', '/ux/people/x', 'employee'],
      ['vendor', '/ux/vendors/x', 'vendor'],
      ['custom', '/ux/jobs?subject_id=x', 'jobs'],
    ];
    for (const [k, want, route] of cases) {
      const p = subjectPath(subject(k, 'x'));
      expect(p).toBe(want);
      const [path, query = ''] = p.split('?');
      expect(parseRoute(path!, query ? `?${query}` : '').kind).toBe(route);
    }
  });

  it('answers a real path for every kind, never #', () => {
    const kinds = ['asset', 'account', 'purchase_order', 'campaign', 'employee', 'vendor', 'custom', 'recipe'];
    for (const k of kinds) {
      const p = subjectPath(subject(k, 'x'));
      expect(p.startsWith('/')).toBe(true);
      expect(p).not.toContain('#');
    }
  });
});
