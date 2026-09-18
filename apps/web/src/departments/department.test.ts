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
} from './department';

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
    // Blocked and pending-sign-off are live: something happened, then
    // it stopped — that is working, not inbound.
    expect(thirdOf(job('d', 'blocked', ['completed', 'pending']))).toBe('working');
    expect(thirdOf(job('e', 'pending-sign-off', ['completed', 'active']))).toBe('working');
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
