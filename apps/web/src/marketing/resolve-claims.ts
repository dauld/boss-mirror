// THE RESOLVER: what does each claim's row actually hold? (backlog
// e1524f57, the other half of design 59a776c5).
//
// `check-claims.ts` decides what a resolved value MEANS; until this file
// nothing produced one, so the marked page, the registry and the checker
// were complete and inert. The split the design chose still holds: the
// I/O — reading a repo file, asking the router — arrives as `readers`,
// and everything here is a pure function of what they return, so a
// broken resolver is a failing unit test before it is a wrong green.
//
// A source that cannot be read resolves to `unreadable` with the reason,
// never to a guess and never to nothing: no evidence is not a pass.

import type { Claim } from './claims';
import type { Resolved } from './check-claims';

/** The edge. `readTree` returns a repo file PARSED (the caller picks the
 *  parser for the file's format) and may throw; `serves` answers whether
 *  this app routes a path to a page of its own rather than to the
 *  catch-all. */
export type ClaimReaders = Readonly<{
  readTree: (where: string) => unknown;
  serves: (path: string) => boolean;
}>;

/** The value at a dotted pointer, descending only through tables. An
 *  array index is not a pointer segment: no claim needs one, and a
 *  pointer that silently indexed a list would read whichever row
 *  happened to sort first. */
export function valueAt(doc: unknown, pointer: string): unknown {
  return pointer.split('.').reduce<unknown>((at, key) => {
    if (at === null || typeof at !== 'object' || Array.isArray(at)) return undefined;
    return (at as Record<string, unknown>)[key];
  }, doc);
}

export function resolveClaim(claim: Claim, readers: ClaimReaders): Resolved {
  const source = claim.source;
  switch (source.kind) {
    case 'route':
      // A route that falls to the catch-all was READ: the record says it
      // is not served, so the answer is a value the page's link cannot
      // equal — a disagreement naming the path, not an unreadable row.
      return readers.serves(source.path)
        ? { kind: 'read', value: source.path }
        : { kind: 'read', value: `(not served by this app: ${source.path})` };
    case 'tree': {
      let doc: unknown;
      try {
        doc = readers.readTree(source.where);
      } catch (e) {
        const why = e instanceof Error ? e.message : String(e);
        return { kind: 'unreadable', why: `${source.where} could not be read: ${why}` };
      }
      const value = valueAt(doc, source.pointer);
      if (value === undefined) {
        return { kind: 'unreadable', why: `${source.where} has no \`${source.pointer}\`` };
      }
      // Stringifying a number or a table would "agree" with whatever the
      // page happens to print for it — a guess, not a reading.
      if (typeof value !== 'string') {
        return {
          kind: 'unreadable',
          why: `${source.where} \`${source.pointer}\` is a ${Array.isArray(value) ? 'list' : typeof value}, not text`,
        };
      }
      return { kind: 'read', value };
    }
  }
}

/** Every claim in the registry, resolved, keyed by id — the map
 *  `checkClaims` takes. */
export function resolveClaims(
  registry: ReadonlyArray<Claim>,
  readers: ClaimReaders,
): ReadonlyMap<string, Resolved> {
  return new Map(registry.map((c) => [c.id, resolveClaim(c, readers)] as const));
}
