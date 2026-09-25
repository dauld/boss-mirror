// The inbox read's parse, for fetchRemote (backlog e2679b23 (c)).
//
// Only a list is an inbox. The parse this replaces coerced every other
// 200 to [], so a changed response shape — an envelope where the list
// was due, a null — painted "Nothing is waiting on you": the false-empty
// class 3fba9c35 swept out of the fetch's status and catch, still alive
// one layer down in its parse. Throwing makes it a failed read, which
// the page renders as the failure line and Retry.

import type { Message } from './types';

function described(body: unknown): string {
  if (body === null) return 'null';
  const t = typeof body;
  return /^[aeiou]/.test(t) ? `an ${t}` : `a ${t}`;
}

/// The parse for a read of `path`; the error names the path the way
/// fetchRemote names a refused status, so the two failures read alike.
export function parseInbox(path: string): (raw: unknown) => Message[] {
  return (raw) => {
    if (!Array.isArray(raw)) {
      throw new Error(`${path}: HTTP 200, but the body is ${described(raw)}, not a list`);
    }
    return raw as Message[];
  };
}
