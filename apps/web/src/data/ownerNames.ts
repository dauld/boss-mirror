// Naming the few people a page shows — and saying so when it cannot.
// Written for the launch calendar's owners; that page retired with the
// second example tenant (design 2ea444f5, backlog a8991c86), and the
// pages below that name a handful of people read through it still.
//
// WHY THIS IS A MODULE (backlog 0268a829, page audit 0ceeffa6 GAP 8,
// 2026-09-23). The page fetched the WHOLE employee roster to name the
// handful of owners on screen, and a refusal was an `if (r.ok)` with no
// else while a network error fell into `catch { // ignore }`. Either
// way the owner links silently became raw ids, and nothing on the page
// said the names had failed to load — an outage wearing the look of an
// employee with an id for a name.
//
// NARROWER, NOT A NEW ENDPOINT. `GET /api/people` has no id filter
// (role, status and email only), and `GET /api/people/{id}` already
// exists and answers exactly one row, so the calendar reads one row per
// distinct owner shown — a few, against the whole roster — with no
// server change. The outcome is the shared ReadState (../data/readState),
// the idiom the exec page adopted for the same launch owners (223ebcd6),
// so the failure line reads the same wherever it appears.
//
// LIFTED INTO data/ (backlog 1e73bd93, 2026-09-24). Measured on
// origin/main that day, the calendar was one of twenty web files that
// fetched the whole `/api/people` roster; about half of them only named
// the few people on screen, and most dropped a failed read. The ones
// that move here are those — a page that names a handful of people.
// A page that OFFERS the roster (an owner or recipient picker) or
// summarises it (HR, QA, the people list) genuinely needs every row and
// keeps its own read; this module is not for them.

import { isHumanActor } from './actor';
import { failedRead, okRead, readStateOfResponse, type ReadState } from './readState';

/// The distinct PEOPLE among the ids a page shows, sorted so two reads
/// of the same rows ask the same questions. Blanks ask nothing, and so
/// do machine actors — an agent, a named automation, a session login —
/// because none of them has a people row: asking would be a guaranteed
/// 404 dressed as a failed name, and `formatActor` labels them without
/// a lookup. `isHumanActor` is the one client-side definition of that
/// line, so this asks it rather than re-deriving it.
export function personIdsOf(ids: Iterable<string | null | undefined>): string[] {
  const out = new Set<string>();
  for (const id of ids) if (id && isHumanActor(id)) out.add(id);
  return [...out].sort();
}

/// The people owning the rows shown. A row with no owner asks nothing.
export function ownerIdsOf(
  rows: ReadonlyArray<{ readonly owner_id: string | null }>,
): string[] {
  return personIdsOf(rows.map((r) => r.owner_id));
}

type Fetch = (url: string) => Promise<Pick<Response, 'ok' | 'status' | 'json'>>;

export type OwnerNames = Readonly<{
  names: ReadonlyMap<string, string>;
  read: ReadState;
}>;

/// One read per owner, concurrently. Names that loaded are kept even
/// when another failed, because an owner we CAN name is still true; the
/// read is the first failure in id order, named by its URL, so the line
/// the page renders says which read an operator should chase.
///
/// An employee whose `name` is null (the people domain is identity-first,
/// so a name may not be enriched yet) is NOT a failed read: the read
/// worked, and the id is the honest label for that row.
export async function loadOwnerNames(
  ids: ReadonlyArray<string>,
  fetchFn: Fetch = (url) => fetch(url),
): Promise<OwnerNames> {
  return readNames(ids, fetchFn, false);
}

/// Names for ids that may be a person OR a team — a handoff's `from_id`
/// and `to_id`, which the handoff StepType declares "Person or team",
/// and which every seeded handoff fills with a team or a role
/// (`inventory-clerk` → `shipping-clerk`). The people service answers
/// such an id 404 "no employee with ID …": the read WORKED and said
/// "not a person", so the id is its own label and nothing failed.
/// Every other refusal and every network error is still a failure
/// (backlog 1e73bd93). An owner or an actor id is always a person, so
/// `loadOwnerNames` keeps reading a 404 there as a failed read.
export async function loadPersonOrTeamNames(
  ids: ReadonlyArray<string>,
  fetchFn: Fetch = (url) => fetch(url),
): Promise<OwnerNames> {
  return readNames(ids, fetchFn, true);
}

async function readNames(
  ids: ReadonlyArray<string>,
  fetchFn: Fetch,
  notFoundIsNotAPerson: boolean,
): Promise<OwnerNames> {
  const results = await Promise.all(
    ids.map(async (id): Promise<{ id: string; name?: string; read: ReadState }> => {
      const url = `/api/people/${encodeURIComponent(id)}`;
      try {
        const resp = await fetchFn(url);
        if (notFoundIsNotAPerson && resp.status === 404) return { id, read: okRead };
        const read = readStateOfResponse(url, resp);
        if (read.kind === 'failed') return { id, read };
        const body = (await resp.json()) as { name?: string | null } | null;
        const name = typeof body?.name === 'string' && body.name.length > 0 ? body.name : undefined;
        return { id, name, read };
      } catch (e) {
        return { id, read: failedRead(`${url}: ${e instanceof Error ? e.message : String(e)}`) };
      }
    }),
  );
  const names = new Map<string, string>();
  for (const r of results) if (r.name !== undefined) names.set(r.id, r.name);
  const failure = results.find((r) => r.read.kind === 'failed');
  return { names, read: failure ? failure.read : okRead };
}
