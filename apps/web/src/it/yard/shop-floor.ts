// THE SHOP FLOOR DRAWS AGENTS AS ACTORS (design 62de32ae, decision 8,
// approved by David 2026-09-24; car E on backlog c3105b2a).
//
// The middle third is where agents pick an item up and build it, and
// the review of 2026-09-24 found it the thinnest part of the map. The
// floor drew one platform per crew with its runs as anonymous marks: on
// the live floor every session was `claude@algedonic.dev` and five
// builder runs stood on one of them, so which run was building what,
// and for how long, was nowhere on the picture — the region said "5
// runs in flight" and drew five grey ticks. The design: "one lamp per
// session, labelled with run and item and with idle or at-work age,
// grouped under the shared identity."
//
// So the picture is three levels deep. An IDENTITY heads its group —
// one human, one agent identity, many sessions, which is the normal
// case here. Under it, a lamp per SESSION (the work-session packet the
// SessionStart hook files), with its host and how long since its last
// prompt. Under each session, a lamp per RUN it dispatched, labelled
// with the run, the item it builds and the step, and how long it has
// been at it. A run no open session claims is drawn under its own
// identity on a "no open session" row, never dropped (a lamp drawn
// nowhere is the false-empty class).
//
// NOTHING HERE READS AN ENDPOINT OR JUDGES SILENCE ANEW. The crews and
// the unclaimed runs are `crew.ts`'s fold of the reads the crew board
// already makes (handed up by CrewBoardPage, as the queue boards hand
// their platforms up); a session's idle line is `IDLE_AFTER_MS`, pinned
// equal to the server's `CREW_IDLE_HOURS`; a run's silence is
// `silence()`, on the age-out rule's own bound. This module labels,
// orders and places. RegionMap owns the strokes.

import { silence, silenceText, type AgentRun, type Crew } from '../crew/crew';
import type { Territory } from './world';

/** What a lamp says, before any colour: a session or run working; a
 *  session silent past the idle line; a run past its build, waiting on
 *  its next step; a run unmoved past the age-out bound; and a reading
 *  nobody could take, which is never drawn as either of the calm ones. */
export type Lamp = 'at-work' | 'idle' | 'waiting' | 'silent' | 'unknown';

export type RunLamp = Readonly<{
  /** `run:<id>` — unique on the floor, whoever dispatched it. */
  key: string;
  /** `run fa4014f2 → c3105b2a build`: the run, the item, the step. */
  label: string;
  /** The item's title, without the `builder run:` the run's own title
   *  puts before it. */
  title: string;
  lamp: Lamp;
  age: string;
}>;

export type SessionLamp = Readonly<{
  /** `session:<id>`, or `session:none:<identity>` for the row that holds
   *  the runs no open session claims. */
  key: string;
  label: string;
  lamp: Lamp;
  age: string;
  runs: ReadonlyArray<RunLamp>;
}>;

export type Actor = Readonly<{
  identity: string;
  sessions: ReadonlyArray<SessionLamp>;
  /** The runs drawn under this identity. */
  runs: number;
}>;

const MINUTE = 60_000;

/** An age as a lamp prints it: `11m`, `4h 41m`, `3h`, `2d`. */
export function ageText(ms: number): string {
  const minutes = Math.floor(Math.max(0, ms) / MINUTE);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours >= 48) return `${Math.floor(hours / 24)}d`;
  const rest = minutes % 60;
  return rest === 0 ? `${hours}h` : `${hours}h ${rest}m`;
}

const since = (iso: string | null, nowMs: number): number | null => {
  if (iso === null) return null;
  const t = Date.parse(iso);
  return Number.isFinite(t) && Number.isFinite(nowMs) ? nowMs - t : null;
};

const short = (id: string | null): string | null => (id === null ? null : id.slice(0, 8));

/** One run's lamp. At work while its `building` step is open — aged
 *  from when the run opened — unless it has stood unmoved past the
 *  age-out rule's bound, which is the rule's own `silent`. Past its
 *  build it is WAITING on the step it stands at, aged from its last
 *  move. */
function runLamp(r: AgentRun, now: string, nowMs: number): RunLamp {
  const item = [short(r.packet), r.step].filter((p) => p !== null).join(' ');
  const label = `run ${short(r.id)}${item === '' ? '' : ` → ${item}`}`;
  const title = r.title.replace(/^[\w-]+ run: /, '');
  const key = `run:${r.id}`;
  if (!r.building) {
    const moved = since(r.lastMovedAt, nowMs);
    const at = r.at ?? 'between steps';
    return { key, label, title, lamp: 'waiting', age: moved === null ? at : `${at} · ${ageText(moved)}` };
  }
  const s = silence(r, now);
  if (s !== null && s.past) return { key, label, title, lamp: 'silent', age: silenceText(s) };
  const opened = since(r.openedAt, nowMs);
  return { key, label, title, lamp: 'at-work', age: opened === null ? 'at work' : `at work ${ageText(opened)}` };
}

const byAge = (a: AgentRun, b: AgentRun): number =>
  (a.openedAt ?? '').localeCompare(b.openedAt ?? '') || a.id.localeCompare(b.id);

/** The lamps a session carries: at work while its heartbeat is inside
 *  the idle line, idle past it, and unknown with no heartbeat at all —
 *  `Crew.idle` is that judgement, made once in crew.ts. */
function sessionLamp(c: Crew, now: string, nowMs: number): SessionLamp {
  const s = c.session;
  const quiet = since(s.lastActiveAt ?? s.startedAt, nowMs);
  const runs = [...c.runs].sort(byAge).map((r) => runLamp(r, now, nowMs));
  const label = `session ${short(s.id)}${s.host === null ? '' : ` · ${s.host}`}`;
  const key = `session:${s.id}`;
  if (c.idle === null || quiet === null) {
    return { key, label, lamp: 'unknown', age: 'silence not measured — no heartbeat on the packet', runs };
  }
  return c.idle
    ? { key, label, lamp: 'idle', age: `idle ${ageText(quiet)}`, runs }
    : { key, label, lamp: 'at-work', age: `at work · last prompt ${ageText(quiet)} ago`, runs };
}

const RANK: Readonly<Record<Lamp, number>> = { 'at-work': 0, silent: 1, waiting: 2, unknown: 3, idle: 4 };

/** The floor as actors: an identity per group, the busiest first; its
 *  sessions, those at work first; their runs, the oldest first. The
 *  runs no listed session claims stand under their own identity on a
 *  `no open session` row. */
export function actors(
  crews: ReadonlyArray<Crew>,
  unlinked: ReadonlyArray<AgentRun>,
  now: string,
): ReadonlyArray<Actor> {
  const nowMs = Date.parse(now);
  const sessions = crews.map((c) => ({
    identity: c.session.actor ?? c.session.title,
    lamp: sessionLamp(c, now, nowMs),
  }));
  const orphans = [...new Set(unlinked.map((r) => r.agent ?? 'an unnamed actor'))].map((identity) => ({
    identity,
    lamp: {
      key: `session:none:${identity}`,
      label: 'no open session',
      lamp: 'unknown' as const,
      age: 'dispatched by hand, or by a session this read did not reach',
      runs: unlinked
        .filter((r) => (r.agent ?? 'an unnamed actor') === identity)
        .sort(byAge)
        .map((r) => runLamp(r, now, nowMs)),
    },
  }));
  const all = [...sessions, ...orphans];
  const identities = [...new Set(all.map((s) => s.identity))];
  return identities
    .map((identity): Actor => {
      const mine = all
        .filter((s) => s.identity === identity)
        .map((s) => s.lamp)
        .sort(
          (a, b) =>
            RANK[a.lamp] - RANK[b.lamp] || b.runs.length - a.runs.length || a.label.localeCompare(b.label),
        );
      return { identity, sessions: mine, runs: mine.reduce((n, s) => n + s.runs.length, 0) };
    })
    .sort((a, b) => b.runs - a.runs || a.identity.localeCompare(b.identity));
}

/** How many runs the floor draws — what the region's head is read
 *  against (decision 5: an interior draws the head's count, or says
 *  what it leaves out). */
export const runsDrawn = (groups: ReadonlyArray<Actor>): number => groups.reduce((n, a) => n + a.runs, 0);

// ---------------------------------------------------------------------
// Where the lamps go on the region's canvas.
// ---------------------------------------------------------------------

export type ActorRow = Readonly<
  | { kind: 'actor'; key: string; x: number; y: number; w: number; actor: Actor }
  | { kind: 'session'; key: string; x: number; y: number; w: number; session: SessionLamp }
  | { kind: 'run'; key: string; x: number; y: number; w: number; run: RunLamp }
>;

/** What one character of the rows' 11px mono text is taken to need. */
export const CHAR_W = 6.8;

/** A row's words cut to the pixels it has, with an ellipsis where they
 *  were cut — the full text rides the row's `<title>`. */
export function fitText(text: string, px: number): string {
  const chars = Math.max(1, Math.floor(px / CHAR_W));
  return text.length <= chars ? text : `${text.slice(0, Math.max(0, chars - 1)).trimEnd()}…`;
}

/** Row pitch and indent per level: the identity, its sessions, their
 *  runs. */
const ROW_H = { actor: 24, session: 21, run: 19 } as const;
const INDENT = { actor: 0, session: 16, run: 38 } as const;
/** Space between two identities' groups. */
const GROUP_GAP = 10;
const EDGE = 12;
/** The least a floor is drawn at, so an empty or one-lamp floor is
 *  still a region and not a sliver. */
export const MIN_FLOOR_H = 120;

/** Every lamp, one row each, stacked from the top of the region's
 *  canvas — the region's head is the page's, above the SVG, so none of
 *  the canvas is held back for one. The canvas is AS TALL AS ITS LAMPS
 *  (`height`): a floor of five runs is not drawn in a 420px box, and a
 *  crowded one grows rather than hiding a lamp — the other regions
 *  count what they cannot fit, and a run is the one thing on this floor
 *  too important to be a "+N". */
export function actorLayout(
  t: Territory,
  groups: ReadonlyArray<Actor>,
): Readonly<{ rows: ReadonlyArray<ActorRow>; height: number }> {
  const box = { x: t.x + EDGE, y: t.y + EDGE / 2, w: t.w - 2 * EDGE };
  const rows: ActorRow[] = [];
  let y = box.y;
  const place = (kind: keyof typeof ROW_H): Readonly<{ x: number; y: number; w: number }> => {
    y += ROW_H[kind];
    return { x: box.x + INDENT[kind], y, w: box.w - INDENT[kind] };
  };
  groups.forEach((actor, i) => {
    if (i > 0) y += GROUP_GAP;
    rows.push({ kind: 'actor', key: `actor:${actor.identity}`, actor, ...place('actor') });
    actor.sessions.forEach((session) => {
      rows.push({ kind: 'session', key: session.key, session, ...place('session') });
      session.runs.forEach((run) => rows.push({ kind: 'run', key: run.key, run, ...place('run') }));
    });
  });
  // No machinery strip is kept below: on the shop floor the machines ARE
  // the sessions, and each already has its lamp above.
  return { rows, height: Math.max(MIN_FLOOR_H, y - t.y + EDGE) };
}
