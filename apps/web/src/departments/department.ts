// A department's jobs view — in / working / out over the packets whose
// workflow declares the department.
//
// David, 2026-09-12: "each department will have a view of jobs flowing
// in, jobs getting worked within the department, and jobs flowing
// out." IT built its three first (Receiving Yard / Crew Board / Train
// Yard), each IT-shaped. This is the department-shaped instance: one
// read, three thirds, for every department the Class registry declares
// — the tab a surface-less department used to land on All jobs from
// (backlog cc76f755, 2026-09-18).
//
// WHICH PACKETS ARE THE DEPARTMENT'S is the server's question, not
// this file's: `GET /api/jobs?department=<code>` keeps the packets
// whose own `metadata.department` is `<code>` (a retro, a page audit,
// the items an audit files) and, for a packet naming none, those of
// the kinds whose active workflow row declares it (backlog 481d7939,
// `DepartmentFilter` in boss-jobs). Before that
// parameter existed the listing ignored it and answered the
// unfiltered count (1944 on prod), which is the reading this page
// must never make — so the loader keeps `total` and the page reports
// a truncated read rather than calling a page the world.
//
// WHICH THIRD A PACKET IS IN is derived here, from its steps, and the
// rule is deliberately the plainest one that is true:
//
//   out      — terminal (closed or cancelled). The read asks for the
//              last OUT_WINDOW_DAYS of them, so this third is "what
//              left recently", not the archive.
//   in       — live, and nothing has been done on it yet: no step has
//              started or finished. It stands at its first step.
//   working  — live, and something has: a step is active, or one has
//              completed and the next is waiting.
//
// A packet whose kind has no steps is inbound until it closes; a
// blocked packet is working (something happened, then it stopped).
// "First HUMAN step" — skipping past automated admission steps — is
// the sharper reading and needs the step's actor on the wire, which
// the listing does not carry; this rule is the honest first cut and
// says so.

import { fetchRemote, type Remote } from '../data/remote';
import type { Job, Step } from '../jobs/types';

/** The OUT third is the last thirty days of departures. */
export const OUT_WINDOW_DAYS = 30;
/** One page. A department past this is reported, not silently cut
 *  (a-limit-is-not-a-filter). */
export const PAGE = 200;

export type Third = 'in' | 'working' | 'out';

export const THIRD_LABEL: Readonly<Record<Third, string>> = {
  in: 'In',
  working: 'Working',
  out: 'Out',
};

/** Which third a packet stands in, from its status and its steps. */
export function thirdOf(job: Pick<Job, 'status' | 'steps'>): Third {
  if (job.status === 'closed' || job.status === 'cancelled') return 'out';
  const steps: ReadonlyArray<Step> = job.steps ?? [];
  const moved = steps.some(
    (s) => s.status === 'active' || s.status === 'completed' || s.status === 'skipped',
  );
  return moved ? 'working' : 'in';
}

export type Thirds = Readonly<{
  in: ReadonlyArray<Job>;
  working: ReadonlyArray<Job>;
  out: ReadonlyArray<Job>;
}>;

/** The three thirds, each in reading order: the inbound queue oldest
 *  first (what has waited longest leads), working newest first (the
 *  listing's own order), departures most recent first. */
export function thirds(jobs: ReadonlyArray<Job>): Thirds {
  const by = (t: Third): ReadonlyArray<Job> => jobs.filter((j) => thirdOf(j) === t);
  const asc = (a: Job, b: Job): number => a.opened_on.localeCompare(b.opened_on);
  const closedDesc = (a: Job, b: Job): number =>
    (b.closed_on ?? '').localeCompare(a.closed_on ?? '');
  return {
    in: by('in').slice().sort(asc),
    working: by('working'),
    out: by('out').slice().sort(closedDesc),
  };
}

export type JobsPage = Readonly<{ rows: ReadonlyArray<Job>; total: number }>;

/** The listing's envelope, kept whole: `total` is what the page
 *  compares itself against. */
export function parseJobsPage(raw: unknown): JobsPage {
  const env = raw as { data?: unknown; total?: unknown } | null;
  const rows = Array.isArray(env?.data) ? (env.data as ReadonlyArray<Job>) : [];
  const total = typeof env?.total === 'number' ? env.total : rows.length;
  return { rows, total };
}

/** The one read: live packets plus the window's departures, of the
 *  department's kinds. `closed_within` is the server's retention
 *  window (live OR closed since), so the three thirds come from one
 *  page and agree with each other. */
export function departmentJobsUrl(code: string): string {
  return `/api/jobs?department=${encodeURIComponent(code)}&closed_within=${OUT_WINDOW_DAYS}&limit=${PAGE}`;
}

export function loadDepartment(
  code: string,
): Promise<Exclude<Remote<JobsPage>, { kind: 'loading' }>> {
  return fetchRemote(departmentJobsUrl(code), parseJobsPage);
}
