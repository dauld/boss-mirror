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
import type { Subject } from './types';

const subject = (subject_kind: string, id: string): Subject =>
  ({ subject_kind, id }) as unknown as Subject;

describe('subjectPath', () => {
  it('sends a campaign to its own packets, not to a workflow kind', () => {
    expect(subjectPath(subject('campaign', 'camp-2026-spring'))).toBe(
      '/jobs?subject_id=camp-2026-spring',
    );
  });

  it('url-encodes a campaign id', () => {
    expect(subjectPath(subject('campaign', 'camp a/b'))).toBe('/jobs?subject_id=camp%20a%2Fb');
  });

  it('sends a custom subject to its own packets, not to Home', () => {
    expect(subjectPath(subject('custom', '/ux/jobs'))).toBe('/jobs?subject_id=%2Fux%2Fjobs');
  });

  it('sends a registry kind this file never names to its own packets', () => {
    // `recipe` is a brewery-tenant row, `department` a platform one
    // added after this switch was written; neither has a case here.
    expect(subjectPath(subject('recipe', 'west-coast-ipa'))).toBe(
      '/jobs?subject_id=west-coast-ipa',
    );
    expect(subjectPath(subject('department', 'it'))).toBe('/jobs?subject_id=it');
  });

  it('names no workflow kind on any branch', () => {
    const kinds = ['asset', 'account', 'purchase_order', 'campaign', 'employee', 'vendor', 'custom'];
    for (const k of kinds) {
      expect(subjectPath(subject(k, 'x'))).not.toContain('kind=');
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
