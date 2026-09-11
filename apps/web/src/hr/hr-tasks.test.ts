// Backlog a704c5eb — "A failed onboarding-task read renders as the
// employee having no tasks".
//
// `fetchTasks` collapsed THREE outcomes into `tasks = []`: no matching
// HR Job, a non-ok `/api/jobs/{id}/steps`, and a thrown error swallowed
// by `catch {}`. The template then guarded on `tasks.length > 0`, so a
// broken read rendered as "this employee has no onboarding tasks" — a
// false statement about a person's onboarding, with nothing on the page
// to tell it apart from a true one.
//
// `/ux/hr` IS crawled by route-smoke.mocked.spec.ts under an
// adversarial backend and PASSES, because the page renders fine. It
// just renders a falsehood. A crawl asserts no runtime crash; it cannot
// assert that an empty list means empty rather than unread. So these
// assertions are the check that was missing, and they live in a pure
// module because `bun test` has no Svelte pass (bunfig.toml).

import { afterEach, describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import {
  fetchEmployeeTasks,
  hrJobRows,
  stepProgress,
  taskRows,
  type HrJobRef,
} from './hr-tasks';

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
});

function stubFetch(fn: (url: string) => Promise<Response>) {
  globalThis.fetch = fn as unknown as typeof fetch;
}

const JOB: HrJobRef = {
  employee_id: 'emp-001',
  job_id: 'job-abc',
  workflow: 'Onboarding',
};

const STEP = {
  id: 'step-1',
  kind: 'it-setup',
  title: 'Issue a laptop',
  status: 'ready',
  assignee_id: null,
  completed_on: null,
};

describe('the three outcomes of an onboarding-task read are distinct', () => {
  test('a non-ok steps read is failed — NOT an employee with no tasks', async () => {
    stubFetch(async () => new Response('steps down', { status: 503 }));
    const res = await fetchEmployeeTasks('emp-001', [JOB]);
    expect(res.kind).toBe('failed');
    if (res.kind === 'failed') expect(res.error).toContain('503');
  });

  test('a thrown fetch is failed, not empty', async () => {
    stubFetch(async () => {
      throw new TypeError('Failed to fetch');
    });
    const res = await fetchEmployeeTasks('emp-001', [JOB]);
    expect(res.kind).toBe('failed');
    if (res.kind === 'failed') expect(res.error).toContain('Failed to fetch');
  });

  test('a malformed payload is failed — garbage is not an empty onboarding', async () => {
    stubFetch(async () => new Response('{"steps":"soon"}', { status: 200 }));
    const res = await fetchEmployeeTasks('emp-001', [JOB]);
    expect(res.kind).toBe('failed');
  });

  test('no matching HR Job is its own answer, distinguishable from both', async () => {
    stubFetch(async () => {
      throw new Error('must not be called — there is no Job to read');
    });
    const res = await fetchEmployeeTasks('emp-404', [JOB]);
    expect(res.kind).toBe('no-job');
  });

  test('a Job with no steps is ready-and-empty — empty still reads as empty', async () => {
    stubFetch(async () => new Response('[]', { status: 200 }));
    const res = await fetchEmployeeTasks('emp-001', [JOB]);
    expect(res.kind).toBe('ready');
    if (res.kind === 'ready') expect(res.data).toHaveLength(0);
  });

  test('a healthy read shapes the steps into tasks', async () => {
    stubFetch(async (url) => {
      expect(url).toContain('/api/jobs/job-abc/steps');
      return new Response(JSON.stringify([STEP]), { status: 200 });
    });
    const res = await fetchEmployeeTasks('emp-001', [JOB]);
    expect(res.kind).toBe('ready');
    if (res.kind !== 'ready') return;
    expect(res.data[0]).toEqual({
      id: 'step-1',
      job_id: 'job-abc',
      employee_id: 'emp-001',
      workflow: 'Onboarding',
      task: 'Issue a laptop',
      category: 'it-setup',
      assignee_id: null,
      status: 'ready',
      due_date: null,
      completed_at: null,
      notes: null,
    });
  });
});

describe('taskRows refuses to invent an empty list', () => {
  test('throws on a non-array payload so fetchRemote reports failed', () => {
    expect(() => taskRows({ oops: true }, JOB, 'emp-001')).toThrow();
  });

  test('maps an array of steps', () => {
    expect(taskRows([STEP], JOB, 'emp-001')).toHaveLength(1);
  });
});

describe('the progress counts behind the Active Workflows table', () => {
  // The same class, one function up: the old code read a Job's steps
  // with `.then(sr => sr.ok ? sr.json() : []).catch(() => [])`, so a
  // failed step read rendered as a workflow that is 0/0 tasks — 0%
  // done — a claim about progress the read never supported.
  test('a non-array payload throws rather than counting zero of zero', () => {
    expect(() => stepProgress('nope')).toThrow();
  });

  test('counts completed against total', () => {
    expect(
      stepProgress([
        { status: 'completed' },
        { status: 'ready' },
        { status: 'completed' },
      ]),
    ).toEqual({ total: 3, done: 2 });
  });
});

describe('the open-HR-Jobs list', () => {
  // Measured against the system of record, 2026-09-11: the list endpoint
  // answers `{data:[…]}` and each Job carries a NESTED
  // `subject:{subject_kind,id}`. The page read `payload.jobs` and flat
  // `subject_kind`/`subject_id`, so the Active Workflows table was empty
  // for every operator, always, and said "No active workflows." about
  // it. Two wrong keys, each answering instead of erroring.
  const ENVELOPE = {
    data: [
      {
        id: 'job-abc',
        kind: 'onboarding',
        subject: { subject_kind: 'employee', id: 'emp-001' },
      },
      {
        id: 'job-other',
        kind: 'onboarding',
        subject: { subject_kind: 'asset', id: 'asset-9' },
      },
    ],
  };

  test('reads the data envelope the endpoint actually sends', () => {
    expect(hrJobRows(ENVELOPE)).toEqual([{ id: 'job-abc', employeeId: 'emp-001' }]);
  });

  test('reads the nested subject, not flat subject_kind/subject_id', () => {
    const flat = { data: [{ id: 'j', subject_kind: 'employee', subject_id: 'emp-001' }] };
    // No nested subject: the row is not about an Employee as far as this
    // payload says, and inventing one would be worse than dropping it.
    expect(hrJobRows(flat)).toEqual([]);
  });

  test('an unrecognised envelope throws — it does not read as no workflows', () => {
    expect(() => hrJobRows({ jobs: [] })).toThrow();
    expect(() => hrJobRows(null)).toThrow();
  });

  test('a bare array is accepted too', () => {
    expect(
      hrJobRows([{ id: 'j1', subject: { subject_kind: 'employee', id: 'emp-002' } }]),
    ).toEqual([{ id: 'j1', employeeId: 'emp-002' }]);
  });
});

// ---------------------------------------------------------------------
// The component wiring, pinned at source level — the TriageBoard
// idiom. `bun test` has no Svelte pass, and the mocked suite proves the
// rendering; these assertions pin the SHAPE, because the regression is
// a plausible refactor ("just set it back to an array") that nothing
// else in the suite would catch.
// ---------------------------------------------------------------------

const page = readFileSync(new URL('./HrPage.svelte', import.meta.url), 'utf8');
const code = page
  .replace(/<!--[\s\S]*?-->/g, '')
  .replace(/\/\*[\s\S]*?\*\//g, '')
  .replace(/(^|[^:])\/\/.*$/gm, '$1');

describe('HrPage cannot render a failed task read as an empty one', () => {
  test('the task read comes from the pure module, not a local catch', () => {
    expect(code).toMatch(/from '\.\/hr-tasks'/);
    expect(code).not.toMatch(/tasks\s*=\s*\[\]/);
  });

  test('the tasks section branches on the failure', () => {
    expect(code).toMatch(/tasks\.kind === 'failed'/);
  });

  test('it distinguishes "no HR Job" from "the Job has no steps"', () => {
    expect(code).toMatch(/'no-job'/);
    // The ready-and-empty branch. `tasks.data` only type-checks once
    // `failed` and `no-job` have been ruled out, so its presence is the
    // proof that all three outcomes have their own words.
    expect(code).toMatch(/tasks\.data\.length === 0/);
  });

  test('the section is not gated on the list being non-empty', () => {
    // `selectedEmp && tasks.length > 0` is the exact guard that made a
    // failure invisible: it hides the section for all three outcomes.
    expect(code).not.toMatch(/tasks\.length > 0/);
  });

  test('a failed row count renders as unknown, never as 0 of 0', () => {
    expect(code).toMatch(/total_tasks:\s*number\s*\|\s*null/);
  });
});
