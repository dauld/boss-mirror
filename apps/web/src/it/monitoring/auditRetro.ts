// What a retro or an incident needs from the Audit Log, read off a row
// and off the page's window controls (backlog 62a0bbee, page audit
// 65a273d5). The page read only the newest 500 rows — about ten minutes
// of log — and a jobs.* row carried its packet's id without linking to
// it, so reading back past that meant downloading, and reading a row's
// packet meant copying an id out of raw JSON.

type Payload = Readonly<Record<string, unknown>>;

function asPayload(payload: unknown): Payload | null {
  if (payload === null || typeof payload !== 'object' || Array.isArray(payload)) return null;
  return payload as Payload;
}

function nonBlank(v: unknown): string | null {
  return typeof v === 'string' && v !== '' ? v : null;
}

/// The packet a row belongs to, or null. A step event serializes the
/// Step, whose own identity is `id` and whose packet is `job_id`; every
/// step and job marker that is not a Job says `job_id` too (boss-jobs
/// events.rs). Only a jobs.job.* payload IS a Job, so only there is
/// `id` the packet — elsewhere `id` names a step, a workflow, a
/// credential.
export function packetOf(kind: string, payload: unknown): string | null {
  const p = asPayload(payload);
  if (p === null) return null;
  const jobId = nonBlank(p.job_id);
  if (jobId !== null) return jobId;
  return kind.startsWith('jobs.job.') ? nonBlank(p.id) : null;
}

/// A `datetime-local` value as the RFC 3339 instant /api/events/tail
/// takes for `since` / `until`. The input holds the browser's own zone,
/// the zone the page paints its times in, so the operator types the
/// time they read off a row; the server compares in UTC. Blank or
/// unreadable is no bound — a window the operator did not set must not
/// narrow the read.
export function windowBound(local: string): string | null {
  if (local.trim() === '') return null;
  const d = new Date(local);
  return Number.isNaN(d.getTime()) ? null : d.toISOString();
}
