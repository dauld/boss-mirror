// Dev server for the Simulator UX. Bun's Fullstack Dev Server bundles
// index.html with `bun-plugin-svelte` (registered in bunfig.toml) and
// ships HMR when .svelte / .ts / .css sources change — no manual
// rebuild loop.
//
// Custom responsibilities beyond static + HMR:
//   1. Proxy `/api/*` directly to service ports (dev-mode bypass of
//      the gateway; the cockpit's read endpoints — /api/jobs/live,
//      /api/events/tail — resolve through this).
//   2. Proxy `/simulator/api/*` to the boss-simulator service
//      (127.0.0.1:7010) — the engine control + status surface the
//      ControlsPanel calls.
//   3. Synthesise `x-boss-user` on every proxied API call from the
//      `boss-persona` cookie so backend policy scoping reflects the
//      "viewing as" persona.
//
// The `Bun.serve` routes object holds the bundled-HTML entries; the
// `fetch` handler is the fallback for everything else. Hot reload is
// driven by Bun — no SSE, no manual wiring here.

import { serve } from 'bun';

// Bun rewrites this import into a bundled-and-served HTML handle.
// In `routes`, using it as a value means "serve the bundled HTML
// for this path, with HMR attached."
import index from '../index.html';

const PORT = Number(process.env['PORT'] ?? 5175);

// The boss-simulator service hosts the /simulator/api/* control +
// status surface. In dev we proxy straight to its prod port.
const SIMULATOR_PORT = Number(process.env['BOSS_SIMULATOR_PORT'] ?? 7010);

// Scratch mode: when BOSS_SCRATCH=1, paired services route to their
// +1000 scratch ports (boss_scratch DB) so writes don't pollute the
// live boss DB. Mirrors the PAIRED_SERVICES list in
// infra/deploy-services.sh. Solo services have no scratch variant
// and stay on their prod ports.
const SCRATCH = process.env['BOSS_SCRATCH'] === '1';
const SCRATCH_OFFSET = 1000;
import { PORTS, PAIRED_NAMES, portFor } from './_generated/ports';

// Service-name → URL prefix overrides. Most services use their
// name as the URL prefix (`messages` → `/api/messages`); the few
// that diverge (multi-router binaries, alias paths) are spelled
// out here. Add a row when a new service introduces a non-default
// prefix, NOT when adding a vanilla service — those get picked up
// automatically from `PORTS`.
const EXTRA_ROUTES: ReadonlyArray<readonly [string, string]> = [
  // /api/scheduling rides on jobs-api.
  ['/api/scheduling', 'jobs'],
  // /api/events tail mounts on people-api.
  ['/api/events', 'people'],
  // /api/snapshot is mounted on observability.
  ['/api/snapshot', 'observability'],
  // /api/files mounts on content-api alongside bulletins/manual.
  ['/api/files', 'content'],
];

const PAIRED_PREFIXES = new Set([
  // From the canonical paired list...
  ...PAIRED_NAMES.map((n) => `/api/${n}`),
  // ...plus aliases that ride on a paired service's binary.
  '/api/scheduling', // rides on /api/jobs
  '/api/events',     // rides on /api/people
]);

// Derive the prefix → port table from the canonical PORTS list +
// the alias overrides above. Every service in PORTS gets a default
// `/api/<name>` entry; EXTRA_ROUTES adds alias prefixes that share
// another service's binary.
const SERVICE_PORTS_PROD: ReadonlyArray<readonly [string, number]> = [
  ...PORTS.map((p) => [`/api/${p.name}`, p.prod] as const),
  ...EXTRA_ROUTES.map(([prefix, service]) => [prefix, portFor(service).prod] as const),
];

const SERVICE_PORTS: ReadonlyArray<readonly [string, number]> =
  SCRATCH
    ? SERVICE_PORTS_PROD.map(([prefix, port]) =>
        PAIRED_PREFIXES.has(prefix)
          ? ([prefix, port + SCRATCH_OFFSET] as const)
          : ([prefix, port] as const),
      )
    : SERVICE_PORTS_PROD;

function upstreamFor(path: string): string | null {
  for (const [prefix, port] of SERVICE_PORTS) {
    if (path.startsWith(prefix)) return `http://127.0.0.1:${port}`;
  }
  return null;
}

type RosterEmployee = {
  id: string;
  role: string;
  department?: string | null;
};
const PEOPLE_PORT = 7500;
const ROSTER_TTL_MS = 30_000;
let rosterCache: Map<string, RosterEmployee> | null = null;
let rosterFetchedAt = 0;

async function rosterLookup(id: string): Promise<RosterEmployee | null> {
  const now = Date.now();
  if (!rosterCache || now - rosterFetchedAt > ROSTER_TTL_MS) {
    try {
      const r = await fetch(`http://127.0.0.1:${PEOPLE_PORT}/api/people`);
      if (r.ok) {
        const rows = (await r.json()) as RosterEmployee[];
        rosterCache = new Map(rows.map((e) => [e.id, e]));
        rosterFetchedAt = now;
      }
    } catch {
      // Network hiccup — fall through; caller gets null.
    }
  }
  return rosterCache?.get(id) ?? null;
}

function readCookie(cookieHeader: string | null, name: string): string | null {
  if (!cookieHeader) return null;
  for (const part of cookieHeader.split(';')) {
    const [k, v] = part.trim().split('=');
    if (k === name && v !== undefined) return decodeURIComponent(v);
  }
  return null;
}

// Synthesise the `x-boss-user` header from the persona cookie. Shared
// by the /api/* read proxy and the /simulator/api/* control proxy so
// backend policy scoping reflects the chosen "viewing as" persona. As
// in apps/web, the role is forced to audit-readonly regardless of the
// persona's real role — safe to expose on a public demo. (The
// simulator control endpoints decide for themselves whether
// audit-readonly is allowed to drive the engine; a 403 is surfaced as
// a read-only notice in the UI.)
async function personaHeaders(req: Request): Promise<Headers> {
  const headers = new Headers(req.headers);
  if (!headers.has('x-boss-user')) {
    const personaId = readCookie(req.headers.get('cookie'), 'boss-persona');
    let userJson: Record<string, unknown> = {
      id: 'emp-audit',
      role: 'audit-readonly',
      access_tier: 'user',
      territory_account_ids: [],
      direct_report_ids: [],
      department: null,
    };
    if (personaId) {
      const emp = await rosterLookup(personaId);
      if (emp) {
        userJson = {
          id: emp.id,
          role: 'audit-readonly',
          access_tier: 'user',
          territory_account_ids: [],
          direct_report_ids: [],
          department: emp.department ?? null,
        };
      }
    }
    headers.set('x-boss-user', JSON.stringify(userJson));
  }
  return headers;
}

async function proxyApi(req: Request, path: string, url: URL): Promise<Response> {
  const upstream = upstreamFor(path);
  if (!upstream) {
    return new Response(`no upstream for ${path}`, { status: 502 });
  }
  const full = `${upstream}${path}${url.search}`;
  const headers = await personaHeaders(req);
  return fetch(full, {
    method: req.method,
    headers,
    body: req.method === 'GET' || req.method === 'HEAD' ? undefined : req.body,
  });
}

// Forward /simulator/api/* to the boss-simulator service. The path is
// passed through verbatim (the service mounts its routes under
// /simulator/api/...).
async function proxySimulator(req: Request, path: string, url: URL): Promise<Response> {
  const full = `http://127.0.0.1:${SIMULATOR_PORT}${path}${url.search}`;
  const headers = await personaHeaders(req);
  return fetch(full, {
    method: req.method,
    headers,
    body: req.method === 'GET' || req.method === 'HEAD' ? undefined : req.body,
  });
}

serve({
  port: PORT,
  development: true,
  routes: {
    // Explicit handlers bind before the SPA catch-all — Bun matches
    // routes in declaration order and the catch-all would otherwise
    // swallow /api/* and /simulator/api/*.

    // /api/session — gateway-only route in production; mocked here so
    // the shared session/sim-clock plumbing doesn't 502 in dev.
    // Returns the demo-mode synthetic session shape (matches what
    // boss-gateway's mint_demo_session emits).
    '/api/session': () =>
      Response.json({
        username: 'demo@anonymous',
        expires_at: Math.floor(Date.now() / 1000) + 8 * 3600,
        role: 'audit-readonly',
      }),
    // boss-simulator control + status surface. Must come before the
    // generic /api/* proxy and the SPA catch-all.
    '/simulator/api/*': (req) => {
      const url = new URL(req.url);
      return proxySimulator(req, url.pathname, url);
    },
    '/api/*': (req) => {
      const url = new URL(req.url);
      return proxyApi(req, url.pathname, url);
    },
    // Bun bundles index.html + all imported Svelte/TS/CSS sources
    // behind this entry; `development: true` attaches the HMR client
    // so browsers pick up source changes without a full reload. Served
    // at root in dev (publicPath `/`), under /simulator in prod.
    '/*': index,
  },
});

console.log(`boss-simulator-web dev server: http://127.0.0.1:${PORT}`);
console.log('  HMR: enabled via bun-plugin-svelte + Bun.serve routes');
console.log(
  `  api proxy → ${SCRATCH ? 'SCRATCH ports (boss_scratch DB) for paired services' : 'prod service ports (boss DB)'}`,
);
console.log(`  /simulator/api/* → http://127.0.0.1:${SIMULATOR_PORT} (boss-simulator)`);
