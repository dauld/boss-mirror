// Naming the owners the launch calendar shows — and saying so when it
// cannot.
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

import { failedRead, okRead, readStateOfResponse, type ReadState } from '../data/readState';

/// The distinct owner ids of the rows shown, sorted so two reads of
/// the same rows ask the same questions. A row with no owner asks none.
export function ownerIdsOf(
  rows: ReadonlyArray<{ readonly owner_id: string | null }>,
): string[] {
  const ids = new Set<string>();
  for (const r of rows) if (r.owner_id) ids.add(r.owner_id);
  return [...ids].sort();
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
  const results = await Promise.all(
    ids.map(async (id): Promise<{ id: string; name?: string; read: ReadState }> => {
      const url = `/api/people/${encodeURIComponent(id)}`;
      try {
        const resp = await fetchFn(url);
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
