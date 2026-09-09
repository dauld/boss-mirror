// Dev server. Bun's Fullstack Dev Server bundles index.html with
// `bun-plugin-svelte` (registered in bunfig.toml) and ships HMR when
// .svelte / .ts / .css sources change — no manual rebuild loop.
//
// Custom responsibilities beyond static + HMR:
//   1. Proxy `/api/*` directly to service ports (dev-mode bypass of
//      the gateway; tests + demo personas don't need OAuth).
//   2. Serve `/plugins/*` bundles from `/var/lib/boss/step-plugins/`
//      (prod gateway serves these; dev reads from disk).
//   3. Synthesise `x-boss-user` on every proxied API call from the
//      `boss-persona` cookie so backend policy scoping reflects the
//      "viewing as" persona chosen by the PersonaSwitcher.
//
// The `Bun.serve` routes object holds the bundled-HTML entries; the
// `fetch` handler is the fallback for everything else. Hot reload
// is driven by Bun — no SSE, no manual wiring here.

import { serve } from 'bun';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

// Bun rewrites this import into a bundled-and-served HTML handle.
// In `routes`, using it as a value means "serve the bundled HTML
// for this path, with HMR attached."
import index from '../index.html';

import { DEFAULT_PORT, TREE_ID, TREE_PATH, treeResponse } from './dev-tree';

const PORT = Number(process.env['PORT'] ?? DEFAULT_PORT);

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
  // /api/design is an alias path on docs-api.
  ['/api/design', 'docs'],
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

async function proxyApi(req: Request, path: string, url: URL): Promise<Response> {
  const upstream = upstreamFor(path);
  if (!upstream) {
    return new Response(`no upstream for ${path}`, { status: 502 });
  }
  const full = `${upstream}${path}${url.search}`;
  const headers = new Headers(req.headers);
  if (!headers.has('x-boss-user')) {
    // Demo Mode default identity: a public-playground visitor lands
    // as audit-readonly (read every projection, write nothing). The
    // persona-cookie can override the *identity* (employee_id +
    // department flow into read-scoping) but never the role —
    // "Viewing As" is read-only no matter who you switch to.
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
          // Force audit-readonly regardless of the persona's "real"
          // role — this is what makes "Viewing As" safe to expose
          // on a public demo.
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
  return fetch(full, {
    method: req.method,
    headers,
    body:
      req.method === 'GET' || req.method === 'HEAD' ? undefined : req.body,
  });
}

function servePlugin(path: string): Response {
  const pluginPath = join(
    '/var/lib/boss/step-plugins',
    path.slice('/plugins/'.length),
  );
  if (existsSync(pluginPath)) {
    return new Response(readFileSync(pluginPath), {
      headers: { 'content-type': 'application/javascript; charset=utf-8' },
    });
  }
  return new Response('plugin not found', { status: 404 });
}

// Tenant module manifest — `[modules]` block from the active tenant's
// tenant.toml. The SPA fetches this once on session load and gates
// sidebar entries by it. Production gateway will serve the same
// path; for now the dev-server reads the brewery seed directly.
const TENANT_MANIFEST_PATH =
  process.env['BOSS_TENANT_MANIFEST_TOML']
  ?? '/opt/boss/examples/brewery/seeds/tenant.toml';

function serveTenantManifest(): Response {
  if (!existsSync(TENANT_MANIFEST_PATH)) {
    return new Response('tenant manifest not found', { status: 404 });
  }
  // Parse the [modules] + [labels] blocks. Hand-parser to avoid
  // pulling a TOML library for the dev server — the production
  // gateway uses the real toml crate.
  const text = readFileSync(TENANT_MANIFEST_PATH, 'utf8');
  const lines = text.split('\n');
  const modules: Record<string, boolean> = {};
  const labels: Record<string, string> = {};
  let section: 'modules' | 'labels' | null = null;
  for (const raw of lines) {
    const line = raw.replace(/#.*$/, '').trim();
    if (line.startsWith('[')) {
      if (line === '[modules]') section = 'modules';
      else if (line === '[labels]') section = 'labels';
      else section = null;
      continue;
    }
    if (!section || !line) continue;
    if (section === 'modules') {
      const m = line.match(/^([a-zA-Z_]+)\s*=\s*(true|false)\s*$/);
      if (m && m[1] && m[2]) modules[m[1]] = m[2] === 'true';
    } else if (section === 'labels') {
      // labels are TOML strings: `key = "value"` (double-quoted).
      const m = line.match(/^([a-zA-Z_.]+)\s*=\s*"([^"]*)"\s*$/);
      if (m && m[1] && m[2] !== undefined) labels[m[1]] = m[2];
    }
  }
  return Response.json({ modules, labels });
}

serve({
  port: PORT,
  development: true,
  routes: {
    // Explicit handlers bind before the SPA catch-all — Bun matches
    // routes in declaration order and the catch-all would otherwise
    // swallow /api/* and /plugins/*.
    // Tenant manifest is dev-server-local — read straight from the
    // brewery seed file. Must come before the generic /api/* proxy
    // so the catch-all doesn't try to forward it.
    // Whose tree is this? Answered before anything else so the mocked
    // suite can tell "a server is up" from "MY server is up" without
    // waiting on the SPA bundle. See src/dev-tree.ts for why a suite
    // that could not ask this went green against another worktree.
    [TREE_PATH]: () => treeResponse(),
    '/api/tenant/manifest': () => serveTenantManifest(),
    // /api/session — gateway-only route in production; mocked here
    // so the SPA's session probe doesn't 502 in dev. Returns the
    // demo-mode synthetic session shape (matches what
    // boss-gateway's mint_demo_session emits).
    '/api/session': () =>
      Response.json({
        username: 'demo@anonymous',
        expires_at: Math.floor(Date.now() / 1000) + 8 * 3600,
        role: 'audit-readonly',
      }),
    '/api/*': (req) => {
      const url = new URL(req.url);
      return proxyApi(req, url.pathname, url);
    },
    '/plugins/*': (req) => servePlugin(new URL(req.url).pathname),
    // Bun bundles index.html + all imported Svelte/TS/CSS sources
    // behind this entry; `development: true` attaches the HMR client
    // so browsers pick up source changes without a full reload.
    '/*': index,
  },
});

console.log(`boss-web dev server: http://127.0.0.1:${PORT}`);
console.log('  HMR: enabled via bun-plugin-svelte + Bun.serve routes');
console.log(
  `  api proxy → ${SCRATCH ? 'SCRATCH ports (boss_scratch DB) for paired services' : 'prod service ports (boss DB)'}`,
);
console.log('  /plugins/* → /var/lib/boss/step-plugins/');
console.log(`  serving tree: ${TREE_ID} (named at ${TREE_PATH})`);
