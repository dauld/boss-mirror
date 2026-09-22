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

  it('names no workflow kind on any branch', () => {
    const kinds = ['asset', 'account', 'purchase_order', 'campaign', 'employee', 'vendor', 'custom'];
    for (const k of kinds) {
      expect(subjectPath(subject(k, 'x'))).not.toContain('kind=');
    }
  });
});
