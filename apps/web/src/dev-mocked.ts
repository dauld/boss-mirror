// MOCKED MODE: answer an unmocked `/api/**` request here, not upstream.
//
// The mocked Playwright suite (tests/run-mocked.ts) intercepts every
// `/api/**` call in-browser and starts no backend. Any request a spec's
// page.route did not catch used to fall through to the dev-server's
// proxy and be forwarded to a service port nothing listens on — and
// bun printed a six-line connect-refusal block for each one, in every
// PASSING run. Measured 2026-09-19 (backlog 82b87a09): 260 such blocks
// across 104 green specs, twelve distinct paths, 70 of the 124 lines a
// red gate's excerpt quoted. Three readers had grown filters for the
// noise (the gate runner's receipt, its excerpt window, the red-train
// alert); this file deletes it at the source instead.
//
// In mocked mode a miss is answered 404 with a body that says what it
// is, counted per `METHOD /path`, and reported ONCE at shutdown as a
// single line naming every distinct miss — so a spec whose mock missed a
// route still shows it, in one line rather than six per request. Off
// mocked mode (`bun run dev`) nothing here is reached: the proxy is
// handed back untouched.
//
// NODE-SAFE ON PURPOSE, like dev-tree.ts: the runner imports the flag
// from here, and its Playwright config loads under node.

// The one spelling of the flag: the runner sets it, the dev-server reads
// it, both through this constant (CLAUDE.md §9a).
export const MOCKED_FLAG = 'BOSS_MOCKED';

export function isMocked(env: Record<string, string | undefined>): boolean {
  return env[MOCKED_FLAG] === '1';
}

export type ApiHandler = (
  req: Request,
  path: string,
  url: URL,
) => Response | Promise<Response>;

// Misses keyed `METHOD /path` (query dropped — `/api/jobs?status=open`
// and `?status=closed` are one unmocked route), valued by count.
export type Misses = Map<string, number>;

// The `/api/*` route's handler: `proxy` untouched off mocked mode; in
// mocked mode a local 404 that never reaches a socket, recorded in
// `misses` for the shutdown summary.
export function apiHandler(mocked: boolean, proxy: ApiHandler, misses: Misses): ApiHandler {
  if (!mocked) return proxy;
  return (req, path) => {
    const key = `${req.method} ${path}`;
    misses.set(key, (misses.get(key) ?? 0) + 1);
    return Response.json({ mock: 'unanswered', path }, { status: 404 });
  };
}

// The one spelling of the summary line's opening, so the server that
// PRINTS it and the runner that reads it off stdout cannot drift
// (CLAUDE.md §9a).
export const MISS_SUMMARY_PREFIX = 'boss-web dev server (mocked): ';

// The one line printed at shutdown, or null when the mock answered
// everything. Most frequent first: the route a reader should mock next
// is the one that missed most.
export function missSummary(misses: ReadonlyMap<string, number>): string | null {
  if (misses.size === 0) return null;
  const rows = [...misses.entries()].sort(([, a], [, b]) => b - a);
  const total = rows.reduce((n, [, count]) => n + count, 0);
  const listed = rows.map(([key, count]) => `${key} x${count}`).join(', ');
  return (
    `${MISS_SUMMARY_PREFIX}${total} /api/** request(s) the in-browser mock did not ` +
    `answer, ${rows.length} distinct, each answered 404 {"mock":"unanswered"} locally: ${listed}`
  );
}

// Is this line of the dev-server's stdout the summary above? The runner
// reads every line the server prints; this is how it tells that one
// from the ready line and the bundle timings.
export function isMissSummary(line: string): boolean {
  return line.startsWith(MISS_SUMMARY_PREFIX);
}

// What a run that printed a summary is told, or null when there was
// none. Backlog 06038ed8: echoing the line kept the fact visible but
// let the run stay green, so the floor car f88e7908 built (403 misses
// to zero) had nothing holding it there — a spec that mounts without
// installApiFloor leaks its reads again and nothing refuses. The line
// already names every path and count; this says what it means and what
// to do.
export function missRefusal(summaryLine: string | null): string | null {
  if (summaryLine === null) return null;
  return (
    `refusing this run: the dev-server answered /api/** reads the in-browser mock did not. ` +
    `Every mocked spec starts from installApiFloor(page) (tests/mocked/_smokeMocks.ts) and mocks ` +
    `only what it tests; a leaked read means the page under test saw a 404 nobody intended. ` +
    `${summaryLine}`
  );
}
