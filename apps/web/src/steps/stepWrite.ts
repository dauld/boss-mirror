// Shared write path for the platform step surfaces (packet cc9d7fc6).
//
// The class this exists to kill: surfaces that `await fetch(...)` a
// PUT and ignore the response. A Complete click that 400s did nothing
// visible — the operator believed the step closed. Every step-surface
// write now flows through here and comes back as a discriminated
// result the surface must branch on: `ok` continues, `failed` renders
// inline and leaves state untouched.

export type StepWriteResult =
  | { kind: 'ok'; response: Response }
  | { kind: 'failed'; error: string };

const MAX_BODY_CHARS = 200;

/// One human-readable line for a refused write. Prefers the server's
/// own words: the `{error|message|detail}` JSON fields the BOSS APIs
/// use, and the 409 sign-off conflict shape (`missing_or_stale_roles`)
/// gets the same wording ApprovalSurface always rendered for it.
export function describeWriteFailure(status: number, bodyText: string): string {
  const clip = (s: string): string =>
    s.length > MAX_BODY_CHARS ? `${s.slice(0, MAX_BODY_CHARS)}…` : s;
  try {
    const parsed: unknown = JSON.parse(bodyText);
    if (typeof parsed === 'string' && parsed.trim()) {
      return `HTTP ${status} — ${clip(parsed.trim())}`;
    }
    if (parsed && typeof parsed === 'object') {
      const rec = parsed as Record<string, unknown>;
      const roles = rec['missing_or_stale_roles'];
      if (Array.isArray(roles) && roles.length > 0) {
        return `sign-offs outstanding: ${roles.join(', ')}`;
      }
      for (const key of ['error', 'message', 'detail']) {
        const v = rec[key];
        if (typeof v === 'string' && v.trim()) {
          return `HTTP ${status} — ${clip(v.trim())}`;
        }
      }
    }
  } catch {
    // Not JSON — fall through to plain text.
  }
  const text = bodyText.trim();
  return text ? `HTTP ${status} — ${clip(text)}` : `HTTP ${status}`;
}

/// The bounded retry that rides out a deploy roll (packet 04cc82ab).
///
/// A scheduled Recreate roll of the SoR pod leaves a seconds-long
/// window where a write gets a refused connection or a 5xx from a pod
/// that is seconds old. `boss` the CLI already survives this — a
/// reconcile that hit `Connection refused` mid-converge used to fail
/// the whole verb until a bounded retry was added (train.rs
/// `retryable`/`JOBS_API_RETRY`). The web had no such tolerance, so a
/// completed design review submitted DURING a roll surfaced an error
/// and the operator's typed resolutions were lost. This is that same
/// tolerance on the web write path, deliberately the SAME budget: a
/// pod roll is over inside 3 attempts at 2s then 4s, and a SoR still
/// refusing after it is an outage to surface, not to paper over.
type RetryPolicy = { attempts: number; baseMs: number };
export const WRITE_RETRY: RetryPolicy = { attempts: 3, baseMs: 2000 };

const realSleep = (ms: number): Promise<void> =>
  new Promise((resolve) => setTimeout(resolve, ms));

/// Options only the tests set — a no-wait sleep so the retry semantics
/// are pinned without spending the backoff, mirroring the CLI's
/// `no_wait` test policy.
export type WriteOpts = {
  policy?: RetryPolicy;
  sleep?: (ms: number) => Promise<void>;
  /// Resend through an ambiguous failure even though the METHOD is not
  /// on the idempotent list. Set only by a caller that knows its call
  /// is: [`saveStep`]'s merge PATCH sets the same keys to the same
  /// values however many times it lands.
  idempotent?: boolean;
};

/// Whether a method may be resent after an AMBIGUOUS failure — one
/// where the request may already have been applied. The same list the
/// CLI draws (train.rs): re-sending an ambiguous POST is how one blip
/// becomes two creates, so only idempotent methods retry through it.
function isIdempotent(method: string | undefined): boolean {
  switch ((method ?? 'GET').toUpperCase()) {
    case 'GET':
    case 'PUT':
    case 'DELETE':
    case 'HEAD':
      return true;
    default:
      return false;
  }
}

/// fetch that can only come back as a StepWriteResult: non-ok statuses
/// and thrown network errors both land in `failed` with a message fit
/// for inline rendering. It cannot be ignored by accident — the caller
/// has to branch to get anything out of it. Transient failures during
/// a deploy roll are retried, bounded, per [`WRITE_RETRY`].
export async function writeStep(
  url: string,
  init: RequestInit,
  opts?: WriteOpts,
): Promise<StepWriteResult> {
  const policy = opts?.policy ?? WRITE_RETRY;
  const sleep = opts?.sleep ?? realSleep;
  const idempotent = opts?.idempotent ?? isIdempotent(init.method);

  for (let attempt = 1; ; attempt += 1) {
    let result: StepWriteResult;
    let retriable: boolean;
    try {
      const response = await fetch(url, init);
      if (!response.ok) {
        const text = await response.text().catch(() => '');
        result = { kind: 'failed', error: describeWriteFailure(response.status, text) };
        // A 4xx is an ANSWER and is never retried. Nor is a 500: the app
        // RAN and returned an error (`db down`, `registry unavailable`),
        // which the operator must see now, not after a backoff. A deploy
        // roll instead takes the pod out from under the GATEWAY, which
        // answers 502/503/504 (a refused connection is handled in the
        // catch) — those are the blips, and only an idempotent call may
        // be re-sent through one. This is where the web policy diverges
        // from the CLI's blanket-5xx (train.rs `retryable`): the CLI
        // talks straight to the jobs API, the browser talks through the
        // gateway, so the roll looks different on the wire.
        retriable = idempotent && [502, 503, 504].includes(response.status);
      } else {
        return { kind: 'ok', response };
      }
    } catch (e) {
      const detail = e instanceof Error ? e.message : String(e);
      result = { kind: 'failed', error: `network error — ${detail}` };
      // A browser fetch rejection is opaque: it cannot distinguish
      // "refused, nothing sent" (safe to resend) from "sent, no reply"
      // (ambiguous). Treat it as ambiguous — resend only when the call
      // is idempotent, so a roll never turns one POST into two creates.
      retriable = idempotent;
    }
    if (retriable && attempt < policy.attempts) {
      await sleep(policy.baseMs * 2 ** (attempt - 1));
      continue;
    }
    return result;
  }
}

/// A step write that carries METADATA, through the two doors (backlog
/// e39a9d2a, Stage 1): the metadata keys through the step merge door
/// (`PATCH …/steps/{id}/metadata`), then everything else — status,
/// notes, assignee — through the step PUT, which then carries no
/// metadata at all.
///
/// WHY NOT ONE PUT. The step PUT replaces `metadata` wholesale. The
/// surfaces built it as `{...step.metadata, key: x || undefined}`, and
/// JSON drops an undefined key, so emptying a field cleared it BY
/// OMISSION — and anything written to the step since the surface read
/// it (a claim, a hook's stamp) was dropped the same way, silently. The
/// merge door changes only the keys it is sent, in one transaction
/// against the row as it stands. So: send only the keys the surface
/// owns, never a spread of the step's metadata; an emptied field goes
/// as an explicit `null`, which the door deletes.
///
/// Merge FIRST: the step's required-at-done fields are validated when
/// it flips to completed, so they must already be there. A refused
/// merge stops the chain — no status flips on top of a write the
/// server rejected. An empty metadata object sends no merge, and a
/// body with nothing but metadata sends no PUT.
export async function saveStep(
  jobId: string,
  stepId: string,
  body: Readonly<{ metadata?: Readonly<Record<string, unknown>> } & Record<string, unknown>>,
  opts?: WriteOpts,
): Promise<StepWriteResult> {
  const { metadata, ...rest } = body;
  const patch = Object.fromEntries(
    Object.entries(metadata ?? {}).map(([k, v]) => [k, v === undefined ? null : v]),
  );
  let result: StepWriteResult | null = null;
  if (Object.keys(patch).length > 0) {
    result = await writeStep(
      `/api/jobs/${jobId}/steps/${stepId}/metadata`,
      {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(patch),
      },
      { ...opts, idempotent: true },
    );
    if (result.kind === 'failed') return result;
  }
  if (result === null || Object.keys(rest).length > 0) {
    return writeStep(
      `/api/jobs/${jobId}/steps/${stepId}`,
      {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(rest),
      },
      opts,
    );
  }
  return result;
}

/// The standard step PUT (PATCH semantics server-side). A body that
/// carries `metadata` belongs in [`saveStep`] instead.
export function putStep(
  jobId: string,
  stepId: string,
  body: unknown,
): Promise<StepWriteResult> {
  return writeStep(`/api/jobs/${jobId}/steps/${stepId}`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
}
