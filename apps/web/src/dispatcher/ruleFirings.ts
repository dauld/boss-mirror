// When each dispatcher rule last fired, and whether its handler is
// failing — `GET /api/yard/rule-firings` (boss-jobs
// http/rule_firings.rs), read by the rules list at /it/registry/rules
// (backlog 43c4451a, found by page-audit 08a444bc gap 5).
//
// WHY. The list showed a rule's trigger and version and nothing about
// whether it RUNS, so a stalled auto-park-on-gate-green and an idle one
// painted the same row. Two records already held the answer: the
// firing a rule leaves when its handlers succeed (dispatcher_firings,
// b14afc48 — the schedule runner's firings too, since 4b175523), and the
// dead-letter it lands on the packet it owed when they fail past the
// budget (a9c498eb) — or, on a topic that names no packet, the
// dead-letter row it records in dispatcher_firings instead (4b175523).
// A rule whose newest dead-letter is LATER than its newest firing is
// failing now; one with neither is idle.
//
// UNREAD IS NOT EMPTY. The server sends each half as null with its
// reason when it could not read it, and the words below print
// "unknown" for that — never "none", which is what a stopped machine
// would then look like.

import { fetchRemote, type Remote } from '../data/remote';
import type { DispatcherRule } from './types';

export type RuleLastFiring = Readonly<{ rule: string; fired_on: string; fired_at: string }>;

export type DeadLetterRollup = Readonly<{
  rule: string;
  packets: number;
  /** Dead-letters on topics that name no packet — recorded in the
   *  firing record instead (4b175523). */
  unrouted: number;
  newest_at: string | null;
  newest_job_id: string | null;
}>;

export type RuleFirings = Readonly<{
  now: string;
  /** The window both halves answer — the firing record prunes past it. */
  retention_days: number;
  firings: ReadonlyArray<RuleLastFiring> | null;
  firings_error: string | null;
  dead_letters: ReadonlyArray<DeadLetterRollup> | null;
  dead_letters_error: string | null;
}>;

const str = (v: unknown): string | null => (typeof v === 'string' ? v : null);

/** The payload, or a throw: a body with neither list's key is a wrong
 *  server (or the crawl's `[]` fallthrough), not a record with no rows. */
export function parseRuleFirings(raw: unknown): RuleFirings {
  if (raw === null || typeof raw !== 'object' || Array.isArray(raw)) {
    throw new Error('rule firings: expected an object');
  }
  const o = raw as Record<string, unknown>;
  if (!('firings' in o) || !('dead_letters' in o)) {
    throw new Error('rule firings: expected firings and dead_letters');
  }
  const firings = Array.isArray(o.firings)
    ? o.firings.map((f) => {
        const r = (f ?? {}) as Record<string, unknown>;
        return { rule: String(r.rule ?? ''), fired_on: String(r.fired_on ?? ''), fired_at: String(r.fired_at ?? '') };
      })
    : null;
  const deadLetters = Array.isArray(o.dead_letters)
    ? o.dead_letters.map((d) => {
        const r = (d ?? {}) as Record<string, unknown>;
        return {
          rule: String(r.rule ?? ''),
          packets: typeof r.packets === 'number' ? r.packets : 0,
          unrouted: typeof r.unrouted === 'number' ? r.unrouted : 0,
          newest_at: str(r.newest_at),
          newest_job_id: str(r.newest_job_id),
        };
      })
    : null;
  return {
    now: String(o.now ?? ''),
    retention_days: typeof o.retention_days === 'number' ? o.retention_days : 0,
    firings,
    firings_error: firings === null ? (str(o.firings_error) ?? 'the firing record was not read') : null,
    dead_letters: deadLetters,
    dead_letters_error:
      deadLetters === null ? (str(o.dead_letters_error) ?? 'the dead-letters were not read') : null,
  };
}

export async function fetchRuleFirings(): Promise<Remote<RuleFirings>> {
  return fetchRemote('/api/yard/rule-firings', parseRuleFirings);
}

/** `4m`, `3h`, `2d` — how long before `now`. Empty when either instant
 *  cannot be read, so a caller never prints an invented age. */
export function ageText(at: string, now: string): string {
  const ms = Date.parse(now) - Date.parse(at);
  if (!Number.isFinite(ms)) return '';
  const min = Math.max(0, Math.floor(ms / 60_000));
  if (min < 60) return `${min}m`;
  const h = Math.floor(min / 60);
  if (h < 48) return `${h}h`;
  return `${Math.floor(h / 24)}d`;
}

/** What a rules row prints in its two activity cells. */
export type RuleActivity = Readonly<{
  lastFired: string;
  /** Hover text: the instant and topic, or why there is none. */
  lastFiredWhy: string;
  deadLetters: string;
  deadLettersWhy: string;
  /** The packet holding the newest dead-letter — where a reader goes to
   *  see what failed. */
  deadLetterJob: string | null;
  /** A dead-letter newer than the newest firing: the rule is failing
   *  now, not idle. */
  failing: boolean;
}>;

const UNREAD = 'unknown';

/** The two cells for one rule. Pure, so the stalled-vs-idle rule is
 *  pinned without a DOM. */
export function ruleActivity(rule: DispatcherRule, read: Remote<RuleFirings>): RuleActivity {
  if (read.kind === 'loading') {
    return { lastFired: '…', lastFiredWhy: '', deadLetters: '…', deadLettersWhy: '', deadLetterJob: null, failing: false };
  }
  if (read.kind === 'failed') {
    const why = `the firing record could not be read: ${read.error}`;
    return { lastFired: UNREAD, lastFiredWhy: why, deadLetters: UNREAD, deadLettersWhy: why, deadLetterJob: null, failing: false };
  }
  const d = read.data;
  const fired = d.firings?.find((f) => f.rule === rule.name) ?? null;
  const dead = d.dead_letters?.find((x) => x.rule === rule.name) ?? null;

  let lastFired: string;
  let lastFiredWhy: string;
  if (d.firings === null) {
    lastFired = UNREAD;
    lastFiredWhy = d.firings_error ?? '';
  } else if (fired !== null) {
    const age = ageText(fired.fired_at, d.now);
    lastFired = age === '' ? fired.fired_at : `${age} ago`;
    lastFiredWhy = `fired ${fired.fired_at} on ${fired.fired_on}`;
  } else {
    lastFired = `none in ${d.retention_days}d`;
    lastFiredWhy = `dispatcher_firings holds no firing of this rule in the last ${d.retention_days} days`;
  }

  if (d.dead_letters === null) {
    return { lastFired, lastFiredWhy, deadLetters: UNREAD, deadLettersWhy: d.dead_letters_error ?? '', deadLetterJob: null, failing: false };
  }
  const count = dead === null ? 0 : dead.packets + dead.unrouted;
  if (dead === null || count === 0) {
    return {
      lastFired, lastFiredWhy,
      deadLetters: 'none',
      deadLettersWhy: `no packet carries a dead-letter from this rule in the last ${d.retention_days} days`,
      deadLetterJob: null, failing: false,
    };
  }
  const failing =
    dead.newest_at !== null && (fired === null || Date.parse(dead.newest_at) > Date.parse(fired.fired_at));
  const age = dead.newest_at === null ? '' : ageText(dead.newest_at, d.now);
  const newest = age === '' ? '' : `, newest ${age} ago`;
  const verdict = !failing ? '' : fired === null ? ' — failing, no firing recorded' : ' — failing since its last firing';
  const where = [
    dead.packets > 0 ? `${dead.packets} on packet${dead.packets === 1 ? '' : 's'}` : '',
    dead.unrouted > 0 ? `${dead.unrouted} on a topic that names no packet, recorded in dispatcher_firings` : '',
  ].filter((w) => w !== '').join(' and ');
  return {
    lastFired, lastFiredWhy,
    deadLetters: `${count}${newest}${verdict}`,
    deadLettersWhy: `${count} dead-letter${count === 1 ? '' : 's'} from this rule in the last ${d.retention_days} days (${where})${dead.newest_at ? `; the newest was recorded ${dead.newest_at}` : ''}`,
    deadLetterJob: dead.newest_job_id,
    failing,
  };
}
