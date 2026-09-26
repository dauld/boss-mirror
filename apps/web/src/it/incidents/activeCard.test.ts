// The active incident card's facts, pinned at the data layer (backlog
// 1a242883, gap 2 of page-audit 9b9849f5). The card printed a title, a
// `when` that was blank on every live packet, and "(unassigned)" for
// six of the incident protocol's ten steps — role-held, not unheld.
// CLAUDE.md §Diagnosis: a troubled packet must look troubled, and an
// incidents page is where that matters most. Each reader below is one
// question the card answers, and each falls back in a stated order.

import { describe, expect, test } from 'bun:test';
import {
  holderOf,
  openedAtMs,
  severityOf,
  startedAt,
  type IncidentJob,
} from './postMortemDoc';
import type { Step } from '../../jobs/types';

const step = (over: Partial<Step>): Step => ({
  id: 's',
  job_id: 'j',
  kind: 'task',
  title: 'a step',
  assignee_id: null,
  status: 'ready',
  sort_order: 0,
  blocked_by: [],
  completed_on: null,
  metadata: {},
  ...over,
});

const job = (over: Partial<IncidentJob>): IncidentJob => ({
  id: 'j',
  kind: 'incident',
  subject: { subject_kind: 'custom', id: 'x' } as IncidentJob['subject'],
  title: 'an incident',
  owner_id: 'emp-david',
  status: 'open',
  priority: 'standard',
  opened_on: '2026-09-26',
  due_on: null,
  closed_on: null,
  metadata: {},
  tags: [],
  steps: [],
  ...over,
});

describe('severityOf — metadata.severity, shown beside priority', () => {
  test('reads the string the raise recorded', () => {
    expect(severityOf(job({ metadata: { severity: 'degradation (no service outage)' } }))).toBe(
      'degradation (no service outage)',
    );
  });
  test('absent or blank is null, never an invented level', () => {
    expect(severityOf(job({}))).toBeNull();
    expect(severityOf(job({ metadata: { severity: '  ' } }))).toBeNull();
  });
});

describe('startedAt — started_at, then the authored when, then the opening', () => {
  test('the raised step carries started_at as one of its fields', () => {
    const j = job({
      metadata: { opened_at: '2026-09-26T02:32:07Z' },
      steps: [step({ sort_order: 0, metadata: { started_at: '2026-09-26T02:10Z' } })],
    });
    expect(startedAt(j)).toBe('2026-09-26T02:10Z');
  });
  test('a job-level started_at wins over the step', () => {
    const j = job({
      metadata: { started_at: 'job-level' },
      steps: [step({ metadata: { started_at: 'step-level' } })],
    });
    expect(startedAt(j)).toBe('job-level');
  });
  test('the older packets\' incident_at / incident_date still answer', () => {
    expect(startedAt(job({ metadata: { incident_at: '18:10-18:35Z' } }))).toBe('18:10-18:35Z');
    expect(startedAt(job({ metadata: { incident_date: '2026-08-13' } }))).toBe('2026-08-13');
  });
  test('the live shape (a65ba21e): no started_at anywhere, so metadata.opened_at', () => {
    const j = job({
      metadata: { opened_at: '2026-09-26T02:32:07Z' },
      steps: [step({ metadata: { symptom: 's' } })],
    });
    expect(startedAt(j)).toBe('2026-09-26T02:32:07Z');
  });
  test('the server admission stamp precedes the metadata convention', () => {
    const j = job({ opened_at: '2026-09-26T02:32:08Z', metadata: { opened_at: 'meta' } });
    expect(startedAt(j)).toBe('2026-09-26T02:32:08Z');
  });
  test('nothing else — opened_on, which every packet carries', () => {
    expect(startedAt(job({ opened_on: '2026-09-24' }))).toBe('2026-09-24');
  });
});

describe('openedAtMs — the instant time open is measured from', () => {
  test('job.opened_at, then metadata.opened_at, then opened_on at midnight UTC', () => {
    expect(openedAtMs(job({ opened_at: '2026-09-26T02:00:00Z' }))).toBe(
      Date.parse('2026-09-26T02:00:00Z'),
    );
    expect(openedAtMs(job({ metadata: { opened_at: '2026-09-26T03:00:00Z' } }))).toBe(
      Date.parse('2026-09-26T03:00:00Z'),
    );
    expect(openedAtMs(job({ opened_on: '2026-09-24' }))).toBe(Date.parse('2026-09-24T00:00:00Z'));
  });
  test('an unparseable stamp falls through rather than answering NaN', () => {
    expect(openedAtMs(job({ metadata: { opened_at: 'soon' }, opened_on: '2026-09-24' }))).toBe(
      Date.parse('2026-09-24T00:00:00Z'),
    );
  });
});

describe('holderOf — the step\'s real audience, never "(unassigned)" for a role queue', () => {
  test('a named assignee', () => {
    expect(holderOf(step({ assignee_id: 'claude@algedonic.dev' }))).toBe('claude@algedonic.dev');
  });
  test('a role-held step (6 of the incident protocol\'s 10)', () => {
    expect(holderOf(step({ metadata: { authority_role: 'platform-admin' } }))).toBe(
      'role platform-admin',
    );
  });
  test('a station-held step', () => {
    expect(holderOf(step({ metadata: { station: 'design-review' } }))).toBe(
      'station design-review',
    );
  });
  test('a department audience, which projects no placement key yet', () => {
    expect(holderOf(step({ metadata: { audience: { department: 'it' } } }))).toBe('department it');
  });
  test('only a step declaring none of them is unassigned', () => {
    expect(holderOf(step({}))).toBe('unassigned');
  });
});
