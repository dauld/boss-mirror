// Aborting a job asks for the reason (design c6f9fb3e, backlog
// 7a98040e). The three decisions, pinned as pure functions:
//   terminal  — the step(s) whose outcome_kind is aborted, read off
//               the job's own materialised steps; none means no control.
//   reason    — required, free text, at least one sentence; recorded on
//               the step as `reason`, merged with the metadata already
//               there (outcome_kind lives in the same object).
//   authority — the step's authority_role admits the viewer, or the
//               control is disabled with the role named.
import { describe, expect, test } from 'bun:test';

import { abortAuthority, abortBody, abortTerminals, reasonIsSentence } from './abort';
import type { Step } from './types';

const step = (over: Partial<Step>): Step => ({
  id: 's-1',
  job_id: 'j-1',
  kind: 'outcome',
  title: 'Closed without action',
  spec_slug: 'declined',
  assignee_id: null,
  status: 'pending',
  sort_order: 9,
  blocked_by: [],
  completed_on: null,
  metadata: { outcome_kind: 'aborted' },
  ...over,
});

describe('abortTerminals', () => {
  test('finds the step whose outcome_kind is aborted, by its metadata not its name', () => {
    const steps = [
      step({ id: 's-0', spec_slug: 'triage', title: 'Triage', metadata: { authority_role: 'platform-admin' } }),
      step({ id: 's-9', spec_slug: 'closed', title: 'Closed', metadata: { outcome_kind: 'completed' } }),
      step({ id: 's-1' }),
    ];
    expect(abortTerminals(steps)).toEqual([
      {
        id: 's-1',
        slug: 'declined',
        title: 'Closed without action',
        authority_role: null,
        status: 'pending',
        metadata: { outcome_kind: 'aborted' },
      },
    ]);
  });

  test('two aborted terminals: both are listed, in step order', () => {
    const steps = [
      step({ id: 's-2', sort_order: 11, spec_slug: 'cancelled', title: 'Cancelled' }),
      step({ id: 's-1', sort_order: 10, spec_slug: 'abandoned', title: 'Abandoned' }),
    ];
    expect(abortTerminals(steps).map((t) => t.slug)).toEqual(['abandoned', 'cancelled']);
  });

  test('none: an empty list, so the page renders no control', () => {
    expect(abortTerminals([step({ metadata: { outcome_kind: 'completed' } })])).toEqual([]);
    expect(abortTerminals([])).toEqual([]);
    expect(abortTerminals(undefined)).toEqual([]);
  });

  test('a terminal already closed is not offered again', () => {
    expect(abortTerminals([step({ status: 'completed' })])).toEqual([]);
    expect(abortTerminals([step({ status: 'skipped' })])).toEqual([]);
  });

  test('carries the authority_role the row declares', () => {
    const [t] = abortTerminals([step({ metadata: { outcome_kind: 'aborted', authority_role: 'platform-admin' } })]);
    expect(t?.authority_role).toBe('platform-admin');
  });

  test('a step without a spec_slug falls back to its id for the slug', () => {
    const [t] = abortTerminals([step({ spec_slug: undefined })]);
    expect(t?.slug).toBe('s-1');
  });
});

describe('reasonIsSentence', () => {
  test('a sentence passes', () => {
    expect(reasonIsSentence('The vendor withdrew the quote.')).toBe(true);
  });
  test('empty, whitespace, and a bare word or two are refused', () => {
    expect(reasonIsSentence('')).toBe(false);
    expect(reasonIsSentence('   ')).toBe(false);
    expect(reasonIsSentence('dup')).toBe(false);
    expect(reasonIsSentence('not needed')).toBe(false);
  });
});

describe('abortBody', () => {
  test('completes the step with the reason merged into the metadata it already has', () => {
    const t = step({ metadata: { outcome_kind: 'aborted', authority_role: 'platform-admin' } });
    expect(abortBody(t, '  The vendor withdrew the quote.  ')).toEqual({
      status: 'completed',
      metadata: {
        outcome_kind: 'aborted',
        authority_role: 'platform-admin',
        reason: 'The vendor withdrew the quote.',
      },
    });
  });

  test('a reason that is not a sentence yields no body — nothing to send', () => {
    expect(abortBody(step({}), 'dup')).toBeNull();
    expect(abortBody(step({}), '')).toBeNull();
  });

  test('names no completed_by: the server stamps the signing actor', () => {
    const body = abortBody(step({}), 'Filed twice; the other packet carries the work.');
    expect(body).not.toBeNull();
    expect(Object.keys(body ?? {})).toEqual(['status', 'metadata']);
  });
});

describe('abortAuthority', () => {
  test('a terminal with no authority_role admits any signed-in viewer', () => {
    expect(abortAuthority(null, 'brewer')).toEqual({ kind: 'admitted' });
  });
  test('the viewer holding the role is admitted', () => {
    expect(abortAuthority('platform-admin', 'platform-admin')).toEqual({ kind: 'admitted' });
  });
  test('any other viewer is refused with the role named', () => {
    expect(abortAuthority('platform-admin', 'brewer')).toEqual({
      kind: 'refused',
      role: 'platform-admin',
      why: 'Only platform-admin may abort this job.',
    });
  });
  test('no session at all is refused, naming the role when there is one', () => {
    expect(abortAuthority('platform-admin', null).kind).toBe('refused');
    expect(abortAuthority(null, null)).toEqual({
      kind: 'refused',
      role: null,
      why: 'Sign in to abort this job.',
    });
  });
});
