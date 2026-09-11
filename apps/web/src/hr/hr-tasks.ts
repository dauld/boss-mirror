// The HR workflows tab's read model (backlog a704c5eb).
//
// What this module exists to prevent: the page used to read an
// employee's onboarding Steps with
//
//     try { ... if (!r.ok) { tasks = []; return; } ... } catch { tasks = [] }
//
// and render the result behind `{#if selectedEmp && tasks.length > 0}`.
// Three different outcomes — no HR Job for this person, a non-ok steps
// read, and a thrown error — all arrived as an empty array, and the
// template then said the same thing about all three: nothing. On a page
// about a person's onboarding that reads as "this employee has no
// onboarding tasks", which a broken read has no standing to claim.
//
// The fix is the house discriminated union (`Remote<T>`, packet
// 3fba9c35) plus ONE extra variant for the genuinely different third
// outcome. A surface holding a `TasksRead` cannot render at all without
// branching, so the failure is visible by construction — the same shape
// the Crew Board uses for all four of its reads, and the same principle
// `fetchMyDay` states: collapsing a failure to an empty result is how
// "the server is down" once rendered as "you have no work".
//
// Pure functions here, rendering in HrPage.svelte: `bun test` has no
// Svelte pass (bunfig.toml), so the logic has to be testable without
// one.

import { fetchRemote, type Remote } from '../data/remote';

/// One Step of an employee's HR Job, in the shape the tasks table
/// renders. `due_date` and `notes` are always null — the Step model
/// carries neither; they are kept so the row shape matches the
/// pre-#101 `/api/people/{id}/tasks` payload the table was written
/// against.
export type WorkflowTask = Readonly<{
  id: string;
  job_id: string;
  employee_id: string;
  workflow: string;
  task: string;
  category: string;
  assignee_id: string | null;
  status: string;
  due_date: string | null;
  completed_at: string | null;
  notes: string | null;
}>;

/// The minimum an active-workflow row has to carry for its tasks to be
/// readable: which employee, which Job, and the Workflow's display
/// label. A structural type so `ActiveWorkflow` satisfies it without a
/// conversion.
export type HrJobRef = Readonly<{
  employee_id: string;
  job_id: string;
  workflow: string;
}>;

/// The answer to "what are this employee's onboarding tasks?", with the
/// three outcomes the old code collapsed kept apart:
///
/// - `ready`  — the Steps were read. An EMPTY `data` means the Job
///              really has no steps, and the page may say so.
/// - `failed` — the read did not happen (non-ok, network throw, or a
///              payload that would not parse). The page must say THAT,
///              and must not describe the onboarding.
/// - `no-job` — nothing failed and there is nothing to read: this
///              employee has no open HR Job in the current list.
export type TasksRead =
  | Exclude<Remote<ReadonlyArray<WorkflowTask>>, { kind: 'loading' }>
  | { kind: 'no-job' };

/// The `/api/jobs/{id}/steps` payload, as much of it as the tasks table
/// uses.
type RawStep = Readonly<{
  id?: unknown;
  kind?: unknown;
  title?: unknown;
  status?: unknown;
  assignee_id?: unknown;
  completed_on?: unknown;
}>;

const str = (v: unknown): string => (typeof v === 'string' ? v : '');
const strOrNull = (v: unknown): string | null =>
  typeof v === 'string' && v !== '' ? v : null;

/// Shape a steps payload into task rows. THROWS when the payload is not
/// an array, which is the point: `fetchRemote` turns the throw into
/// `{kind:'failed'}`, so a malformed response is a failure rather than
/// an employee with nothing to do.
export function taskRows(
  raw: unknown,
  job: HrJobRef,
  empId: string,
): ReadonlyArray<WorkflowTask> {
  if (!Array.isArray(raw)) {
    throw new Error('steps: expected an array');
  }
  return (raw as ReadonlyArray<RawStep>).map((s) => ({
    id: str(s.id),
    job_id: job.job_id,
    employee_id: empId,
    workflow: job.workflow,
    task: str(s.title),
    category: str(s.kind),
    assignee_id: strOrNull(s.assignee_id),
    status: str(s.status),
    due_date: null,
    completed_at: strOrNull(s.completed_on),
    notes: null,
  }));
}

/// Read one employee's HR tasks. Never returns an empty list to mean
/// anything other than "this Job has no steps".
export async function fetchEmployeeTasks(
  empId: string,
  workflows: ReadonlyArray<HrJobRef>,
): Promise<TasksRead> {
  const job = workflows.find((w) => w.employee_id === empId);
  if (!job) return { kind: 'no-job' };
  return fetchRemote(
    `/api/jobs/${encodeURIComponent(job.job_id)}/steps`,
    (raw) => taskRows(raw, job, empId),
  );
}

/// One open HR Job as the list endpoint sends it. Only the three fields
/// the workflows tab reads.
export type HrJobRow = Readonly<{
  id: string;
  employeeId: string;
}>;

/// Rows out of a `/api/jobs?kind=…` answer, for Jobs about an Employee.
///
/// THROWS on an envelope it does not recognise. This is the §Doors rule
/// applied to a payload: a wrong key answers instead of erroring.
/// Measured against the system of record on 2026-09-11, the page had two
/// of them — it read `payload.jobs` where the endpoint answers
/// `{data:[…]}`, and flat `job.subject_kind` / `job.subject_id` where the
/// Job carries a nested `subject:{subject_kind,id}`. Either one on its
/// own emptied the list, so "No active workflows." was the only thing
/// the Active Workflows table could say, to every operator, always —
/// and the tasks table below it was unreachable because its button only
/// exists on one of those rows. Throwing means the next key change
/// renders as a failure instead of as an empty department.
export function hrJobRows(raw: unknown): ReadonlyArray<HrJobRow> {
  const rows = Array.isArray(raw)
    ? raw
    : Array.isArray((raw as { data?: unknown } | null)?.data)
      ? ((raw as { data: unknown[] }).data)
      : null;
  if (rows === null) {
    throw new Error('jobs: expected {data:[…]} or an array');
  }
  return (rows as ReadonlyArray<Record<string, unknown>>).flatMap((j) => {
    const subject = (j.subject ?? {}) as Record<string, unknown>;
    const id = strOrNull(j.id);
    const employeeId = strOrNull(subject.id);
    if (id === null || employeeId === null) return [];
    if (subject.subject_kind !== 'employee') return [];
    return [{ id, employeeId }];
  });
}

/// Step counts for one Job's progress bar. THROWS on a non-array for
/// the same reason `taskRows` does: the previous code swallowed a failed
/// step read into `[]` and the table then rendered the workflow as 0/0
/// tasks, 0% done — a statement about progress that no read supported.
export function stepProgress(raw: unknown): { total: number; done: number } {
  if (!Array.isArray(raw)) {
    throw new Error('steps: expected an array');
  }
  const steps = raw as ReadonlyArray<{ status?: unknown }>;
  return {
    total: steps.length,
    done: steps.filter((s) => s.status === 'completed').length,
  };
}

/// The progress counts for one Job, or `null` when the step read failed.
/// `null` is rendered as "unknown", never as zero.
export async function fetchStepProgress(
  jobId: string,
): Promise<{ total: number; done: number } | null> {
  const res = await fetchRemote(
    `/api/jobs/${encodeURIComponent(jobId)}/steps`,
    stepProgress,
  );
  return res.kind === 'ready' ? res.data : null;
}
