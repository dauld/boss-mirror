// Who acted, read off an audit row (backlog 03f79eca).
//
// Every writer stamps its payload with `_actor`, and park-a-job,
// rotate-a-credential and ship-a-change each state that the log answers
// "who and when" — but the Audit Log painted Time / Source / Kind, so
// who acted was visible only by opening a row's raw JSON. The column and
// the filter read the same stamp through this one function, and the
// server filters on the same field (`payload->>'_actor'`, tail_http.rs).

/// The row's actor, or null when the payload carries none (a row
/// predating the stamp) or carries something that is not a name.
export function actorOf(payload: unknown): string | null {
  if (payload === null || typeof payload !== 'object' || Array.isArray(payload)) return null;
  const a = (payload as Readonly<Record<string, unknown>>)._actor;
  return typeof a === 'string' && a !== '' ? a : null;
}

/// The distinct actors on screen, sorted — the filter's datalist, built
/// the way the Source datalist is: from the current batch, no extra read.
export function knownActors(rows: ReadonlyArray<Readonly<{ payload: unknown }>>): string[] {
  return [...new Set(rows.map((r) => actorOf(r.payload)).filter((a): a is string => a !== null))].sort();
}
