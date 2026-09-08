// The signals panel — what fired what, newest first, read off the
// packets' own completed steps: the trains (open and recent), the
// gate-runs, and the ops-requests. WHO is derived from the packet kind:
// a pr-train's steps are the conductor's, a gate-run's the
// gate-runner's, an ops-request's the dispatcher's request (when a rule
// spawned it; an operator's otherwise) and the ops-runner's answer.
//
// WHEN is the instant the record carries, and only that: a train step's
// `completed_at` (the conductor stamps one in the step's metadata); a
// gate-run's verdict at the packet's `closed_at` (the packet closes on
// the verdict; its steps carry no instant); an ops-request's request at
// `opened_at` and answer at `closed_at`. A step with no instant is not
// a signal — a day-granular stamp cannot order a list, and inventing a
// time is the one thing this panel must not do.
//
// A step whose metadata says `completed_by_hand: true` (the conductor's
// own convention — trains #257 and #258 on 2026-09-07 carry it with a
// `hand_reason`) renders in the warn tone with "by hand". That is the
// signal an operator most needs to see and the one a summary would
// soften.

import { prNumber } from './yard-floor';
import { stampAt, type JobLite, type StepLite } from './yard';

/** How many signals the panel shows. */
export const SIGNALS_SHOWN = 10;

export type SignalWho = 'conductor' | 'dispatcher' | 'gate-runner' | 'ops-runner' | 'operator';

export type Signal = Readonly<{
  id: string;
  at: string;
  who: SignalWho;
  what: string;
  sev: 'ok' | 'warn' | 'err';
  hand: boolean;
  /** The packet the signal came from — the row opens it. */
  packetId: string;
}>;

const str = (v: unknown): string | null => (typeof v === 'string' && v !== '' ? v : null);
const shortSha = (v: unknown): string | null =>
  typeof v === 'string' && /^[0-9a-f]{7,40}$/i.test(v) ? v.slice(0, 7) : null;
const md = (j: JobLite): Record<string, unknown> => j.metadata ?? {};
const closedAt = (j: JobLite): string | null => str(md(j).closed_at);
const openedAt = (j: JobLite): string | null => str(md(j).opened_at);
const stepMd = (s: StepLite): Record<string, unknown> => s.metadata ?? {};

type Phrase = Readonly<{ what: string; sev: Signal['sev'] }>;

/** A train step as a phrase, keyed on the conductor's step slugs; an
 *  unknown slug falls back to the step's own title. */
function trainPhrase(name: string, s: StepLite): Phrase {
  const m = stepMd(s);
  switch (s.spec_slug) {
    case 'collect': {
      const boarded = str(m.boarded);
      const n = boarded ? boarded.split(',').length : null;
      return { what: n !== null ? `${name} boarded ${n} car${n === 1 ? '' : 's'}` : `${name} boarded`, sev: 'ok' };
    }
    case 'assemble':
      return { what: `${name} assembled`, sev: 'ok' };
    case 'pr':
      return { what: `${name} PR opened`, sev: 'ok' };
    case 'ci': {
      const result = str(m.result) ?? 'done';
      const ok = result === 'green';
      const checks = ok ? '' : str(m.checks) ? ` · ${str(m.checks)}` : '';
      return { what: `${name} CI ${result}${checks}`, sev: ok ? 'ok' : 'err' };
    }
    case 'merged': {
      const ref = shortSha(m.merge_ref);
      return { what: ref ? `${name} merged → main at ${ref}` : `${name} merged → main`, sev: 'ok' };
    }
    case 'deployed':
      return { what: `${name} deployed`, sev: 'ok' };
    case 'converged': {
      const sha = shortSha(m.cluster_commit);
      return { what: sha ? `${name} converged · cluster on ${sha}` : `${name} converged`, sev: 'ok' };
    }
    case 'arrived':
      return { what: `${name} arrived`, sev: 'ok' };
    case 'cancelled': {
      const reason = str(m.reason);
      return { what: reason ? `${name} cancelled — ${reason}` : `${name} cancelled`, sev: 'warn' };
    }
    default:
      return { what: `${name} ${s.title}`, sev: 'ok' };
  }
}

/** A hand-completed step: the phrase becomes "… by hand — reason". */
function byHand(name: string, s: StepLite): Phrase {
  const reason = str(stepMd(s).hand_reason);
  const stepName = s.spec_slug ?? s.title;
  return { what: `${name} ${stepName} by hand${reason ? ` — ${reason}` : ''}`, sev: 'warn' };
}

function trainSignals(j: JobLite): Signal[] {
  const pr = (j.steps ?? []).find(s => s.spec_slug === 'pr');
  const n = prNumber(str(stepMd(pr ?? { title: '', status: '' }).pr_url));
  const name = n !== null ? `#${n}` : j.title;
  return (j.steps ?? []).flatMap((s): Signal[] => {
    if (s.status !== 'completed') return [];
    // An outcome step carries no stamp of its own; the packet's close
    // is when it happened.
    const at = stampAt(s) ?? (s.spec_slug === 'arrived' || s.spec_slug === 'cancelled' ? closedAt(j) : null);
    if (at === null) return [];
    const hand = stepMd(s).completed_by_hand === true;
    const p = hand ? byHand(name, s) : trainPhrase(name, s);
    return [{ id: `${j.id}:${s.spec_slug ?? s.title}`, at, who: 'conductor', what: p.what, sev: p.sev, hand, packetId: j.id }];
  });
}

function gateSignals(j: JobLite): Signal[] {
  const branch = str(md(j).branch) ?? j.title;
  const verdictStep = (j.steps ?? []).find(s => s.spec_slug === 'record-verdict');
  if (!verdictStep || verdictStep.status !== 'completed') return [];
  const at = stampAt(verdictStep) ?? closedAt(j);
  if (at === null) return [];
  const m = stepMd(verdictStep);
  let verdict = str(m.verdict) ?? 'unknown';
  let fails: string[] = [];
  const raw = m.receipt;
  if (typeof raw === 'string') {
    try {
      const r = JSON.parse(raw) as { verdict?: unknown; fails?: unknown };
      verdict = str(r.verdict) ?? verdict;
      fails = Array.isArray(r.fails) ? r.fails.filter((f): f is string => typeof f === 'string') : [];
    } catch {
      // A receipt the page cannot read leaves the step's own verdict.
    }
  }
  const sev: Signal['sev'] = verdict === 'green' ? 'ok' : verdict === 'lost' ? 'warn' : 'err';
  return [{
    id: `${j.id}:verdict`,
    at,
    who: 'gate-runner',
    what: `gate ${branch} ${verdict}${fails.length > 0 ? ` · ${fails.join(', ')}` : ''}`,
    sev,
    hand: false,
    packetId: j.id,
  }];
}

function opsSignals(j: JobLite): Signal[] {
  const m = md(j);
  const verb = str(m.verb) ?? 'request';
  const host = str(m.host) ?? '?';
  const rule = str(m.spawned_by_rule);
  const out: Signal[] = [];
  const requestedAt = openedAt(j);
  if (requestedAt !== null) {
    out.push({
      id: `${j.id}:filed`,
      at: requestedAt,
      who: rule !== null ? 'dispatcher' : 'operator',
      what: `${verb} requested on ${host}${rule !== null ? ` — rule ${rule}` : ''}`,
      sev: 'ok',
      hand: false,
      packetId: j.id,
    });
  }
  const execute = (j.steps ?? []).find(s => s.spec_slug === 'execute');
  const answeredAt = execute?.status === 'completed' ? (stampAt(execute) ?? closedAt(j)) : null;
  if (execute && answeredAt !== null) {
    const em = stepMd(execute);
    const disposition = str(em.disposition) ?? 'completed';
    const runnerHost = str(em.runner_host) ?? host;
    const code = str(em.exit_code);
    const output = (str(em.output) ?? '').split('\n')[0]?.trim() ?? '';
    const refused = disposition === 'refused';
    const failed = refused || (code !== null && code !== '0');
    const what = refused
      ? `${verb} refused on ${runnerHost}${output !== '' ? ` — ${output}` : ' — outside the allowlist'}`
      : `${verb} ${disposition} on ${runnerHost}${code !== null ? ` · exit ${code}` : ''}`;
    out.push({ id: `${j.id}:execute`, at: answeredAt, who: 'ops-runner', what, sev: failed ? 'err' : 'ok', hand: false, packetId: j.id });
  }
  return out;
}

/** The newest `limit` signals across every source, by instant. */
export function yardSignals(
  trains: readonly JobLite[],
  gateRuns: readonly JobLite[],
  opsRequests: readonly JobLite[],
  limit: number = SIGNALS_SHOWN,
): readonly Signal[] {
  const all = [...trains.flatMap(trainSignals), ...gateRuns.flatMap(gateSignals), ...opsRequests.flatMap(opsSignals)];
  // Newest first; at the same instant the later step in its packet is
  // the later signal (the conductor stamps collect, assemble and pr
  // within one second), so packet order breaks the tie, reversed.
  return all
    .map((s, i) => ({ s, i, ms: Date.parse(s.at) }))
    .filter(x => !Number.isNaN(x.ms))
    .sort((a, b) => b.ms - a.ms || b.i - a.i)
    .slice(0, limit)
    .map(x => x.s);
}
