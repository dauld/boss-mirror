// The semi-structured post-mortem renderer, pinned at the data layer.
//
// Incident packets carry their findings as free-form Job
// metadata, and the shape has already drifted between the two live
// packets: the 2026-08-22 one carries incident_at / summary /
// mitigations_shipped / open_questions / evidence, the 2026-08-13 one
// carries ask / declared_by / incident_date / outcome. The renderer's
// contract is therefore SEMI-structured: keys it knows get first-class
// sections in a fixed reading order, keys it does not know render as
// labeled prose blocks after them — never dropped, never dumped as raw
// JSON.

import { describe, expect, test } from 'bun:test';
import {
  closedOutcome,
  humanizeKey,
  incidentAt,
  postMortemSections,
} from './postMortemDoc';
import type { Step } from '../../jobs/types';

describe('postMortemSections — well-known keys in reading order', () => {
  test('orders the known sections canonically, whatever order the metadata carries them in', () => {
    const sections = postMortemSections({
      evidence: 'readyz verbose captured',
      open_questions: '(a) dedicated etcd disk',
      mitigations_shipped: 'gate-run protocol v1 ACTIVE',
      summary: 'COMPLETE TIMELINE.',
      incident_at: '2026-08-22, two windows',
    });
    expect(sections.map((s) => s.label)).toEqual([
      'When it happened',
      'Summary',
      'Mitigations shipped',
      'Open questions',
      'Evidence',
    ]);
  });

  test('timeline and root_cause slot between summary and the mitigations family', () => {
    const sections = postMortemSections({
      root_cause: 'an unbounded dev container',
      mitigations_shipped: 'throttles baked in',
      timeline: '16:38Z first casualty',
      summary: 'the short version',
    });
    expect(sections.map((s) => s.key)).toEqual([
      'summary',
      'timeline',
      'root_cause',
      'mitigations_shipped',
    ]);
  });

  test('every mitigations* key is first-class, kept in authored order within the family', () => {
    const sections = postMortemSections({
      open_questions: 'still open',
      mitigations_shipped: 'shipped',
      mitigation_next: 'planned',
    });
    expect(sections.map((s) => s.key)).toEqual([
      'mitigations_shipped',
      'mitigation_next',
      'open_questions',
    ]);
  });

  test('the older packet shape (incident_date) still gets the When section first', () => {
    const sections = postMortemSections({
      ask: 'Please write the post mortem.',
      incident_date: '2026-08-13',
    });
    expect(sections[0]).toEqual({
      key: 'incident_date',
      label: 'When it happened',
      body: '2026-08-13',
    });
  });
});

describe('postMortemSections — unknown keys are labeled prose, never dropped', () => {
  test('unknown string keys render after the known sections, in authored order', () => {
    const sections = postMortemSections({
      ask: 'Do we have a good surface for post mortems?',
      declared_by: 'emp-david',
      summary: 'the incident',
    });
    expect(sections.map((s) => s.label)).toEqual(['Summary', 'Ask', 'Declared by']);
    expect(sections[1]?.body).toBe('Do we have a good surface for post mortems?');
  });

  test('non-string values are preserved as prose, not dropped and not raw JSON', () => {
    const sections = postMortemSections({
      retries: 3,
      self_inflicted: true,
      affected_nodes: ['cp-2', 'cp-3'],
      window: { start: '18:10Z', end: '18:35Z' },
    });
    const byKey = new Map(sections.map((s) => [s.key, s.body]));
    expect(byKey.get('retries')).toBe('3');
    expect(byKey.get('self_inflicted')).toBe('true');
    expect(byKey.get('affected_nodes')).toBe('cp-2\ncp-3');
    // A flat object renders as labeled lines — no braces, no quotes.
    expect(byKey.get('window')).toBe('Start: 18:10Z\nEnd: 18:35Z');
  });

  test('blank and null values are omitted rather than rendered as empty sections', () => {
    expect(
      postMortemSections({ summary: '   ', evidence: '', gone: null, list: [] }),
    ).toEqual([]);
  });

  test('the omit list keeps keys the caller renders elsewhere out of the body', () => {
    const sections = postMortemSections(
      { outcome: 'shipped', summary: 'the incident', incident_date: '2026-08-13' },
      ['outcome', 'incident_date'],
    );
    expect(sections.map((s) => s.key)).toEqual(['summary']);
  });
});

describe('humanizeKey', () => {
  test('spells snake_case and kebab-case keys as sentence-cased labels', () => {
    expect(humanizeKey('declared_by')).toBe('Declared by');
    expect(humanizeKey('mitigations-shipped')).toBe('Mitigations shipped');
    expect(humanizeKey('ask')).toBe('Ask');
  });
});

describe('incidentAt', () => {
  test('prefers incident_at, falls back to the older incident_date, else null', () => {
    expect(incidentAt({ incident_at: '2026-08-22', incident_date: '2026-08-13' })).toBe(
      '2026-08-22',
    );
    expect(incidentAt({ incident_date: '2026-08-13' })).toBe('2026-08-13');
    expect(incidentAt({})).toBeNull();
    expect(incidentAt({ incident_at: '  ' })).toBeNull();
  });
});

describe('closedOutcome', () => {
  const step = (over: Partial<Step>): Step => ({
    id: 's',
    job_id: 'j',
    kind: 'task',
    title: 'a step',
    assignee_id: null,
    status: 'pending',
    sort_order: 0,
    blocked_by: [],
    completed_on: null,
    metadata: {},
    ...over,
  });

  test('reads the last completed transition — the terminal that actually fired', () => {
    const outcome = closedOutcome({
      metadata: {},
      steps: [
        step({ title: 'Incident opened', status: 'completed', sort_order: 0 }),
        step({ title: 'Post-mortem closed', status: 'completed', sort_order: 7 }),
        step({ title: 'Superseded by another post-mortem', status: 'skipped', sort_order: 8 }),
      ],
    });
    expect(outcome).toBe('Post-mortem closed');
  });

  test('falls back to a metadata.outcome string when steps are absent', () => {
    expect(closedOutcome({ metadata: { outcome: 'All mitigations shipped' } })).toBe(
      'All mitigations shipped',
    );
    expect(closedOutcome({ metadata: {} })).toBeNull();
  });
});
