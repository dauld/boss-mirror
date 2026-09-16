// The one ask a decision step makes (user-feedback 26ae4d44).
//
// David, 2026-09-15, on a backlog-item triage step assigned to him and
// carrying a brief that said exactly what to enter: "Wasn't quite sure
// how to fill in the job properly to move to the next step." The
// surface had the brief, the form and a button — and none of them said
// which route each answer takes or what the button does. Everything
// this module derives comes from the Workflow's own step graph: the
// `ready_when` predicates already say which step each value opens.

import { describe, expect, test } from 'bun:test';
import {
  askRoutes,
  completeLabel,
  missingRequired,
  needsLine,
  specSlugOf,
} from './stepAsk';

// The backlog-item graph as the registry serves it, trimmed to the
// predicates that route on triage.
const BACKLOG_STEPS = [
  { title: 'filed', ready_when: 'true', title_template: 'Filed to the backlog' },
  { title: 'triage', ready_when: 'steps.filed.done', title_template: 'Measure the claim, choose a route' },
  {
    title: 'measure',
    ready_when: 'steps.triage.done AND steps.triage.metadata.disposition = "verify"',
    title_template: 'Re-measure the claim',
  },
  {
    title: 'design-review',
    ready_when: 'steps.triage.done AND steps.triage.metadata.disposition = "design"',
    title_template: 'Decide the design',
  },
  {
    title: 'build',
    ready_when:
      '(steps.triage.done AND steps.triage.metadata.disposition = "build") OR (steps.design-review.done AND steps.design-review.metadata.verdict = "approved")',
    title_template: 'Build the change',
  },
  {
    title: 'stale',
    ready_when: 'steps.triage.done AND steps.triage.metadata.disposition = "stale"',
    title_template: 'Closed — the claim no longer holds',
  },
  {
    title: 'declined',
    ready_when:
      '(steps.triage.done AND steps.triage.metadata.disposition = "decline") OR (steps.design-review.done AND steps.design-review.metadata.verdict = "declined")',
    title_template: 'Closed without action',
  },
];

const DISPOSITION = {
  name: 'disposition',
  field_type: 'verify|design|build|duplicate|stale|decline',
  required: true,
};
const EVIDENCE = { name: 'evidence', field_type: 'string', required: true };
const PROPOSED = { name: 'proposed', field_type: 'string', required: false };

describe('askRoutes — every option carries the step it opens', () => {
  test('reads the successor off the predicate, not a hardcoded map', () => {
    const routes = askRoutes(BACKLOG_STEPS, 'triage', DISPOSITION);
    expect(routes.map((r) => [r.value, r.route])).toEqual([
      ['verify', 'Re-measure the claim'],
      ['design', 'Decide the design'],
      ['build', 'Build the change'],
      ['duplicate', null],
      ['stale', 'Closed — the claim no longer holds'],
      ['decline', 'Closed without action'],
    ]);
  });

  test('a second-disjunct route is found: build is opened by design-review too', () => {
    const verdict = { name: 'verdict', field_type: 'approved|declined|answered', required: true };
    const routes = askRoutes(BACKLOG_STEPS, 'design-review', verdict);
    expect(routes.find((r) => r.value === 'approved')?.route).toBe('Build the change');
    expect(routes.find((r) => r.value === 'declined')?.route).toBe('Closed without action');
  });

  test('the comparison is anchored to THIS step: another step routing on the same field name is not ours', () => {
    const steps = [
      { title: 'a', ready_when: 'true', title_template: 'A' },
      { title: 'b', ready_when: 'steps.a.done', title_template: 'B' },
      { title: 'c', ready_when: 'steps.b.metadata.disposition = "go"', title_template: 'C' },
    ];
    const field = { name: 'disposition', field_type: 'go|stop', required: true };
    expect(askRoutes(steps, 'a', field).map((r) => r.route)).toEqual([null, null]);
    expect(askRoutes(steps, 'b', field).map((r) => r.route)).toEqual(['C', null]);
  });

  test('a free-text field has no routes, and a step with no spec has none for its enum', () => {
    expect(askRoutes(BACKLOG_STEPS, 'triage', EVIDENCE)).toEqual([]);
    expect(askRoutes(null, 'triage', DISPOSITION).map((r) => r.route)).toEqual([
      null, null, null, null, null, null,
    ]);
  });
});

describe('specSlugOf — which spec step a materialised step is', () => {
  test('spec_slug wins when the wire carries it', () => {
    expect(specSlugOf({ spec_slug: 'triage', title: 'anything' }, BACKLOG_STEPS)).toBe('triage');
  });
  test('falls back to the step whose rendered title matches', () => {
    expect(specSlugOf({ title: 'Measure the claim, choose a route' }, BACKLOG_STEPS)).toBe('triage');
    expect(specSlugOf({ title: 'Nowhere' }, BACKLOG_STEPS)).toBeNull();
    expect(specSlugOf({ title: 'triage' }, null)).toBeNull();
  });
});

describe('the completing control names its effect', () => {
  const routes = askRoutes(BACKLOG_STEPS, 'triage', DISPOSITION);
  test('a chosen value with a known route', () => {
    expect(completeLabel(routes, 'build')).toBe('Complete — routes to: Build the change');
  });
  test('nothing chosen, or a value the graph does not route, is plain Complete', () => {
    expect(completeLabel(routes, '')).toBe('Complete');
    expect(completeLabel(routes, 'duplicate')).toBe('Complete');
    expect(completeLabel([], 'build')).toBe('Complete');
  });
});

describe('the missing required field is NAMED, not hinted', () => {
  const fields = [DISPOSITION, EVIDENCE, PROPOSED];
  test('lists only required fields that are still blank', () => {
    expect(missingRequired(fields, { disposition: '', evidence: ' ', proposed: '' })).toEqual([
      'disposition',
      'evidence',
    ]);
    expect(missingRequired(fields, { disposition: 'build', evidence: '' })).toEqual(['evidence']);
    expect(missingRequired(fields, { disposition: 'build', evidence: 'option b' })).toEqual([]);
  });
  test('the line reads as a sentence with the field names humanised', () => {
    expect(needsLine(['disposition', 'evidence'])).toBe('Needs: disposition, evidence');
    expect(needsLine(['context_md'])).toBe('Needs: context md');
    expect(needsLine([])).toBeNull();
  });
});
