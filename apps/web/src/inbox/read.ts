// The inbox read's parse, for fetchRemote (backlog e2679b23 (c)).
//
// Only a list is an inbox. The parse this replaces coerced every other
// 200 to [], so a changed response shape — an envelope where the list
// was due, a null — painted "Nothing is waiting on you": the false-empty
// class 3fba9c35 swept out of the fetch's status and catch, still alive
// one layer down in its parse. Throwing makes it a failed read, which
// the page renders as the failure line and Retry.
//
// The check itself is the shared list reader (data/shape.ts, generalised
// from this file for the marshalling board and /it/design, 67825067), so
// every page's malformed-read line says what came back the same way.

import { readList } from '../data/shape';
import type { Message } from './types';

/// The parse for a read of `path`; the error names the path the way
/// fetchRemote names a refused status, so the two failures read alike.
export function parseInbox(path: string): (raw: unknown) => Message[] {
  return (raw) => readList(path, raw) as Message[];
}
