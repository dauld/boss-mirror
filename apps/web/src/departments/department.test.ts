// The department jobs view's derivation — which third a packet stands
// in, and the one read it comes from (cc76f755).

import { describe, expect, it } from 'bun:test';
import type { Job, Step, StepStatus } from '../jobs/types';
import {
  OUT_WINDOW_DAYS,
  PAGE,
  departmentJobsUrl,
  parseJobsPage,
  thirdOf,
  thirds,
  waitedFor,
  waitingAt,
} from './department';
import { parseStepWaits } from '../jobs/queueAge';

function step(status: StepStatus): Step {
  return {
    id: `s-${status}`,
    job_id: 'j',
    kind: 'checklist',
    title: status,
    assignee_id: null,
    status,
    sort_order: 0,
    blocked_by: [],
    completed_on: null,
    metadata: {},
  };
}

function job(
  id: string,
  status: Job['status'],
  steps: ReadonlyArray<StepStatus>,
  dates: Readonly<{ opened: string; closed?: string }> = { opened: '2026-09-01' },
): Job {
  return {
    id,
    kind: 'receive-an-inquiry',
    subject: { subject_kind: 'custom', id: 'algedonic' },
    title: id,
    owner_id: 'emp-david',
    status,
    priority: 'standard',
    opened_on: dates.opened,
    due_on: null,
    closed_on: dates.closed ?? null,
    metadata: {},
    tags: [],
    steps: steps.map(step),
  };
}

describe('thirdOf — which third a packet stands in', () => {
  it('a live packet nothing has been done on is IN', () => {
    expect(thirdOf(job('a', 'open', ['ready', 'pending']))).toBe('in');
    expect(thirdOf(job('b', 'open', ['pending', 'pending']))).toBe('in');
    // No steps at all: it stands at its (absent) first step.
    expect(thirdOf(job('c', 'open', []))).toBe('in');
    expect(thirdOf({ status: 'open' })).toBe('in');
  });

  it('a live packet with a step started or finished is WORKING', () => {
    expect(thirdOf(job('a', 'open', ['active', 'pending']))).toBe('working');
    expect(thirdOf(job('b', 'open', ['completed', 'ready']))).toBe('working');
    expect(thirdOf(job('c', 'open', ['skipped', 'ready']))).toBe('working');
    // Any non-terminal status is live: a draft whose step has moved is
    // working, not inbound.
    expect(thirdOf(job('d', 'draft', ['completed', 'pending']))).toBe('working');
  });

  it('a terminal packet is OUT whatever its steps say', () => {
    expect(thirdOf(job('a', 'closed', ['completed', 'completed']))).toBe('out');
    expect(thirdOf(job('b', 'cancelled', ['ready']))).toBe('out');
    expect(thirdOf(job('c', 'closed', []))).toBe('out');
  });
});

describe('thirds — the three lists, in reading order', () => {
  it('splits one page into in / working / out and orders each', () => {
    const page = [
      job('in-new', 'open', ['ready'], { opened: '2026-09-15' }),
      job('in-old', 'open', ['ready'], { opened: '2026-09-02' }),
      job('working-1', 'open', ['completed', 'ready'], { opened: '2026-09-10' }),
      job('out-earlier', 'closed', ['completed'], { opened: '2026-08-20', closed: '2026-09-05' }),
      job('out-latest', 'closed', ['completed'], { opened: '2026-08-25', closed: '2026-09-16' }),
      job('working-2', 'open', ['active'], { opened: '2026-09-12' }),
    ];
    const t = thirds(page);
    // The inbound queue: what has waited longest leads.
    expect(t.in.map((j) => j.id)).toEqual(['in-old', 'in-new']);
    // Working keeps the listing's order.
    expect(t.working.map((j) => j.id)).toEqual(['working-1', 'working-2']);
    // Departures: most recent first.
    expect(t.out.map((j) => j.id)).toEqual(['out-latest', 'out-earlier']);
    // Every packet lands in exactly one third.
    expect(t.in.length + t.working.length + t.out.length).toBe(page.length);
  });

  it('an empty page is three empty thirds, not a crash', () => {
    const t = thirds([]);
    expect(t).toEqual({ in: [], working: [], out: [] });
  });
});

// Backlog 4d4dc204 (page audit 3f964c57, 2026-09-23): a receive-a-payout
// packet stood at its `post` step for 2.6 days and no surface a finance
// operator opens said so. The thirds say a packet is live; this says
// WHERE it stands — the step or steps that can be taken now.
describe('waitingAt — the step a live packet stands at', () => {
  const at = (
    status: Job['status'],
    steps: ReadonlyArray<Readonly<{ title: string; status: StepStatus; sort_order: number }>>,
  ): Job => ({
    ...job('p', status, []),
    steps: steps.map((s) => ({ ...step(s.status), id: s.title, title: s.title, sort_order: s.sort_order })),
  });

  it('names the ready or active step, not the done or the not-yet', () => {
    const j = at('open', [
      { title: 'Record the payout', status: 'completed', sort_order: 0 },
      { title: 'Post the payout', status: 'ready', sort_order: 1 },
      { title: 'Reconcile', status: 'pending', sort_order: 2 },
    ]);
    expect(waitingAt(j)).toBe('Post the payout');
  });

  it('names every step open in parallel, in the workflow order', () => {
    const j = at('open', [
      { title: 'Second', status: 'active', sort_order: 2 },
      { title: 'First', status: 'ready', sort_order: 1 },
      { title: 'Done', status: 'skipped', sort_order: 0 },
    ]);
    expect(waitingAt(j)).toBe('First · Second');
  });

  it('a terminal packet waits on nothing, whatever a stray step says', () => {
    expect(waitingAt(at('closed', [{ title: 'Post', status: 'ready', sort_order: 0 }]))).toBe('');
    expect(waitingAt(at('cancelled', [{ title: 'Post', status: 'active', sort_order: 0 }]))).toBe('');
  });

  it('a live packet with no open step says nothing rather than guessing', () => {
    expect(waitingAt(at('open', [{ title: 'Later', status: 'pending', sort_order: 0 }]))).toBe('');
    expect(waitingAt({ status: 'open' })).toBe('');
  });
});

describe('waitedFor — since when, from the queue-age lens (66a5d5be)', () => {
  // The finance audit's own packet: a payout at `post`, ready since
  // 2026-09-21T01:10Z, read 2.6 days later.
  const at = (
    status: Job['status'],
    steps: ReadonlyArray<Readonly<{ id: string; status: StepStatus; sort_order: number }>>,
  ): Job => ({
    ...job('p', status, []),
    steps: steps.map((s) => ({ ...step(s.status), id: s.id, title: s.id, sort_order: s.sort_order })),
  });
  const lens = {
    kind: 'ready' as const,
    data: parseStepWaits({
      data: [
        { step_id: 'post', since: '2026-09-21T01:10:00Z', exact: true },
        { step_id: 'check', since: '2026-09-23T13:40:00Z', exact: false },
      ],
      now: '2026-09-23T15:40:00Z',
    }),
  };
  const fallbackNow = Date.parse('2030-01-01T00:00:00Z');

  it('prints how long the open step has waited, on the lens clock', () => {
    const j = at('open', [
      { id: 'record', status: 'completed', sort_order: 0 },
      { id: 'post', status: 'ready', sort_order: 1 },
    ]);
    expect(waitedFor(j, lens, fallbackNow)).toBe('2d 14h');
  });

  it('a fallback stamp is a lower bound, and parallel steps read in workflow order', () => {
    const j = at('open', [
      { id: 'check', status: 'active', sort_order: 2 },
      { id: 'post', status: 'ready', sort_order: 1 },
    ]);
    expect(waitedFor(j, lens, fallbackNow)).toBe('2d 14h · ≥2h 0m');
  });

  it('an open step the lens has no row for says unknown, never a made-up age', () => {
    expect(waitedFor(at('open', [{ id: 'other', status: 'ready', sort_order: 0 }]), lens, fallbackNow)).toBe(
      'unknown',
    );
  });

  it('a terminal packet, or one with no open step, waits on nothing', () => {
    expect(waitedFor(at('closed', [{ id: 'post', status: 'ready', sort_order: 0 }]), lens, fallbackNow)).toBe('');
    expect(waitedFor(at('open', [{ id: 'post', status: 'pending', sort_order: 0 }]), lens, fallbackNow)).toBe('');
  });

  it('a lens still loading or failed says so in the cell, never an empty age', () => {
    const j = at('open', [{ id: 'post', status: 'ready', sort_order: 0 }]);
    expect(waitedFor(j, { kind: 'loading' }, fallbackNow)).toBe('…');
    expect(waitedFor(j, { kind: 'failed', error: 'HTTP 500' }, fallbackNow)).toBe('unreadable');
  });
});

describe('the one read', () => {
  it('asks the server for the department, the window, and one page', () => {
    const url = departmentJobsUrl('sales');
    expect(url).toBe(`/api/jobs?department=sales&closed_within=${OUT_WINDOW_DAYS}&limit=${PAGE}`);
    // A code the URL would eat survives.
    expect(departmentJobsUrl('front of house')).toContain('department=front%20of%20house');
  });

  it('keeps total beside the rows, so a truncated read is visible', () => {
    const page = parseJobsPage({ data: [job('a', 'open', [])], total: 201 });
    expect(page.rows.length).toBe(1);
    expect(page.total).toBe(201);
    // A malformed envelope is an empty page that says so, never a throw.
    expect(parseJobsPage(null)).toEqual({ rows: [], total: 0 });
    expect(parseJobsPage({ data: 'nope' })).toEqual({ rows: [], total: 0 });
  });
});
