// Surface opens — the SPA tells the system which surface it opened
// (backlog 628f182b).
//
// David, 2026-09-16, running the company as one human plus agents: "I
// am mostly concerned about my personal HCI … the UI and good
// transparency into underlying state has been really important for
// driving reliability and trust." Then: "let's measure which surfaces I
// open for a week." Measured that day: nothing recorded a route open
// per actor. The SPA routes client-side, so only hard loads reach the
// gateway at all, and those are login redirects, not a record; the
// audit log carries no per-request events by decision. This module is
// the record's client half: one POST per client-side navigation to
// `/api/surface-opens`, carrying the route PATTERN and the time and
// nothing else. The actor is the session's, signed by the gateway —
// the body never names one, so it cannot lie about one.
//
// THE PATTERN, NEVER THE ID. `/ux/jobs/:jobId`, not the uuid: the
// question is which SURFACES get opened, and a per-id row would count
// a busy day on one packet as a hundred surfaces. The pattern is the
// pathname with every id-valued field of the parsed Route replaced by
// its field name, so the vocabulary is the router's own and a new
// route's id becomes a placeholder without this file learning it.
// Query parameters (a jobs-list filter, a search) are not part of the
// pattern: they are how a surface was opened, not which.
//
// ONCE PER NAVIGATION, DEBOUNCED AGAINST THE SAME PATTERN. A popstate
// that re-parses to the same surface (an in-page filter change, a
// detail page opened for a second id) is the same open, and is not
// counted twice. Navigating away and back is two opens.
//
// FAILURES ARE SILENT TO THE OPERATOR. A fetch that fails changes
// nothing on the page: the count is a measurement, and a measurement
// must never get in the way of the thing it measures.

import type { Route } from '../router';

/// Route fields the router reads off the QUERY STRING, not the path.
/// They are how a surface was opened (a list filter, a search, where
/// Back goes) and never appear in the pathname — and their values are
/// free text, so `fromLabel: 'jobs'` must not turn `/ux/jobs/…` into
/// `/ux/:fromLabel/…`. Pinned against router.ts by surface-opens.test.ts:
/// a new `sp.get(…)` field that is not listed here fails the test.
export const QUERY_FIELDS: ReadonlySet<string> = new Set([
  'workflow',
  'workflowPrefix',
  'jobStatus',
  'jobOwnerId',
  'jobSubjectId',
  'newJobSubjectKind',
  'newJobSubjectId',
  'q',
  'from',
  'fromLabel',
]);

/// A route's id-valued fields, longest value first so a value that is a
/// prefix of another is replaced after the longer one.
function idFields(route: Route): ReadonlyArray<readonly [string, string]> {
  return Object.entries(route)
    .filter(
      (e): e is [string, string] =>
        e[0] !== 'kind' && !QUERY_FIELDS.has(e[0]) && typeof e[1] === 'string' && e[1] !== '',
    )
    .sort(([, a], [, b]) => b.length - a.length);
}

/// Decode one path segment; a malformed escape stays as it was rather
/// than throwing on a URL somebody typed.
function decodeSegment(s: string): string {
  try {
    return decodeURIComponent(s);
  } catch {
    return s;
  }
}

/// The route pattern for a parsed Route at a pathname: the pathname
/// (minus the /dashboard mount and a trailing slash, as the router
/// reads it) with each id-valued field's value replaced by `:field`.
/// `/ux/jobs/abc-123` for `{kind:'jobDetail', jobId:'abc-123'}` is
/// `/ux/jobs/:jobId`; `/it/codebase` stays `/it/codebase`.
export function routePattern(route: Route, pathname: string): string {
  const raw = pathname.replace(/^\/dashboard/, '').replace(/\/$/, '') || '/';
  const decoded = raw.split('/').map(decodeSegment).join('/');
  return idFields(route).reduce((path, [field, value]) => replaceSegment(path, value, field), decoded);
}

/// `path` with the LAST segment-bounded occurrence of `/value` replaced
/// by `/:field`. Segment-bounded: `/` + value followed by `/` or the
/// end, so `assetId: 'asset'` rewrites `/ux/assets/asset` to
/// `/ux/assets/:assetId` and leaves the `assets` segment alone; last,
/// because the router puts ids at the tail and a static segment that
/// merely starts with the id (`/jobs` for `jobId: 'j'`) sits earlier.
function replaceSegment(path: string, value: string, field: string): string {
  const needle = `/${value}`;
  for (let at = path.lastIndexOf(needle); at !== -1; at = at === 0 ? -1 : path.lastIndexOf(needle, at - 1)) {
    const after = path.charAt(at + needle.length);
    if (after === '' || after === '/') {
      return `${path.slice(0, at)}/:${field}${path.slice(at + needle.length)}`;
    }
  }
  return path;
}

/// What the recorder posts. `at` is the client's clock: when the
/// operator opened it, as the operator's machine saw it.
export type SurfaceOpen = Readonly<{ route: string; at: string }>;

/// A recorder: call it with the current pattern on every navigation;
/// it posts once per change of pattern and swallows every failure.
/// `post` is injected so the debounce is testable without a network.
export function makeSurfaceOpenRecorder(
  post: (open: SurfaceOpen) => Promise<unknown>,
  now: () => Date = () => new Date(),
): (pattern: string) => void {
  let last: string | null = null;
  return (pattern: string): void => {
    if (pattern === last) return;
    last = pattern;
    void post({ route: pattern, at: now().toISOString() }).catch(() => {
      // A failed record changes nothing the operator sees.
    });
  };
}

/// The one live transport: a fire-and-forget POST through the gateway,
/// which signs the session's actor onto it. `keepalive` so a navigation
/// that unloads the document (a full-page link) still delivers.
export function postSurfaceOpen(open: SurfaceOpen): Promise<unknown> {
  return fetch('/api/surface-opens', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(open),
    keepalive: true,
  });
}
