// open-page-audits — the page march's opener (design 0e07ce64, backlog
// 0c4ff12b): one `page-audit` packet per catalogued route, from the ONE
// roster, in the decided order, idempotently.
//
//   bun apps/web/scripts/open-page-audits.ts --dry-run          # the plan
//   bun apps/web/scripts/open-page-audits.ts --all              # open them
//   bun apps/web/scripts/open-page-audits.ts --route /ux/support
//
// WHY A BUN SCRIPT AND NOT A `boss` VERB — measured, not preferred.
// The roster is `ROUTE_CATALOG` in apps/web/src/shell/nav-catalog.ts,
// a TypeScript const with no JSON twin: the router, the sidebar, the
// app tabs and the mocked crawl's drift test all import it, and
// nothing outside Bun can read it without parsing TypeScript or
// keeping a second list — the drift CLAUDE.md §9a names. So the opener
// imports it the way route-smoke.mocked.spec.ts's drift test does
// (`import { ROUTE_CATALOG } from '../src/shell/nav-catalog'`). Note
// that tests/mocked/_routes.ts does NOT derive from the catalog: it is
// a hand list of crawled paths, pinned a SUPERSET of the catalog by
// that drift test, and it carries routes the catalog does not (bare
// `/`, `/ux/me`, a seeded detail page, `/ux/departments/sales`). The
// catalog is the roster; `_routes.ts` is a crawl list held to it.
//
// THE DOOR, NOT A SECOND HEADER SHAPE. Every read and write goes
// through `boss-api` (infra/dev/boss-api, the same script on the
// workstation's PATH): the base URL from BOSS_JOBS_URL or its sor-url,
// the X-Boss-User envelope, the machine token, the refusal of an
// unnamed write — one definition, and this script spawns it rather
// than restating any of it. The body rides stdin (`boss-api` hands
// curl `--data-binary @<arg>`, and `@-` is curl's stdin). What this
// script must know for itself is the packet's `owner_id`, which the
// jobs API requires in the body: it reads the actor by the door's own
// rule — BOSS_ACTOR, else one line in $HOME/.config/boss/actor
// (crates/orchestrators/boss-cli/src/identity.rs) — and refuses to
// open anything unnamed. `--dry-run` needs no actor.
//
// IDEMPOTENT. A route with an OPEN page-audit, or one closed
// `audited`, is skipped; a withdrawn one (the route left the catalog
// and came back) does not block. Every existing packet is read across
// pages on `total` — a limit is not a filter.
//
// ORDER (the decision): routes under the departments whose first
// protocols land first — support, sales, product, finance, marketing,
// hosting — in that order; then every other department in catalog
// order; then Home's surfaces (cross-cutting, built by IT, carried as
// department `it`); then IT's own. `product` and `hosting` own no
// catalog route today (they are Algedonic's departments, and the
// catalog's `app` codes are pinned against the platform + playground
// seeds), so they match nothing until a surface is added — the order
// still names them so that surface lands in its place.

import { readFileSync } from 'node:fs';
import { ROUTE_CATALOG, departmentJobsPath } from '../src/shell/nav-catalog';

export type PlannedAudit = Readonly<{
  route: string;
  department: string;
  label: string;
}>;

export type Skipped = Readonly<{ route: string; why: string }>;

export type Plan = Readonly<{
  open: ReadonlyArray<PlannedAudit>;
  skipped: ReadonlyArray<Skipped>;
}>;

/// The shape of an existing page-audit packet this script reads: its
/// status and, from metadata, the route and (once closed) the outcome.
export type ExistingAudit = Readonly<{
  status: string;
  metadata: Readonly<{ route?: unknown; outcome?: unknown }>;
}>;

/// The departments whose first protocols are landing first, in the
/// decided order. Everything else follows in catalog order, then
/// Home, then IT.
export const FIRST_DEPARTMENTS: ReadonlyArray<string> = [
  'support',
  'sales',
  'product',
  'finance',
  'marketing',
  'hosting',
];

/// Home surfaces are cross-cutting — personal work, not a department's
/// — and the packet's `department` must be a Class code the readiness
/// read answers for. IT builds and owns them.
export const HOME_DEPARTMENT = 'it';

type CatalogEntry = Readonly<{ label: string; path: string; app?: string }>;

/// Every catalogued route, once, in catalog order — with the department
/// the catalog assigns it. Two catalog keys can share a path
/// (`system-dispatcher-rules` and `system-dispatcher-rule` both answer
/// /it/registry/rules), so the first entry names it; a parameterised
/// path is not a page.
export function catalogRoutes(
  catalog: Readonly<Record<string, CatalogEntry>> = ROUTE_CATALOG,
): ReadonlyArray<PlannedAudit> {
  const seen = new Set<string>();
  return Object.values(catalog)
    .filter((e) => !e.path.includes(':'))
    .filter((e) => {
      if (seen.has(e.path)) return false;
      seen.add(e.path);
      return true;
    })
    .map((e) => ({
      route: e.path,
      department: departmentFor(e.app),
      label: e.label,
    }));
}

function departmentFor(app: string | undefined): string {
  if (app === undefined || app === 'home' || app === 'simulator') return HOME_DEPARTMENT;
  return app;
}

/// The rank a route sorts by: its place in FIRST_DEPARTMENTS, else
/// after them (other departments keep catalog order), Home after those,
/// IT last. `app` is what the catalog said; `department` is what the
/// packet carries — Home's `it` must not sort with IT's own.
function rank(audit: PlannedAudit, app: string | undefined): number {
  if (app === 'it') return FIRST_DEPARTMENTS.length + 2;
  if (audit.department === HOME_DEPARTMENT) return FIRST_DEPARTMENTS.length + 1;
  const i = FIRST_DEPARTMENTS.indexOf(audit.department);
  return i === -1 ? FIRST_DEPARTMENTS.length : i;
}

/// The catalog's routes in march order.
export function marchOrder(
  catalog: Readonly<Record<string, CatalogEntry>> = ROUTE_CATALOG,
): ReadonlyArray<PlannedAudit> {
  const appOf = new Map(Object.values(catalog).map((e) => [e.path, e.app] as const));
  return catalogRoutes(catalog)
    .map((a, i) => ({ a, i, r: rank(a, appOf.get(a.route)) }))
    .sort((x, y) => x.r - y.r || x.i - y.i)
    .map(({ a }) => a);
}

/// Is this packet the reason not to open another for its route?
function blocks(existing: ExistingAudit): boolean {
  if (existing.status === 'open' || existing.status === 'draft') return true;
  return existing.metadata.outcome === 'audited';
}

/// The plan: which routes get a packet, which are skipped and why.
export function plan(
  routes: ReadonlyArray<PlannedAudit>,
  existing: ReadonlyArray<ExistingAudit>,
): Plan {
  const blocked = new Map<string, string>();
  for (const e of existing) {
    if (typeof e.metadata.route !== 'string' || !blocks(e)) continue;
    const why = e.status === 'closed' ? 'already audited' : `a page-audit is ${e.status}`;
    blocked.set(e.metadata.route, why);
  }
  const open: PlannedAudit[] = [];
  const skipped: Skipped[] = [];
  for (const r of routes) {
    const why = blocked.get(r.route);
    if (why === undefined) open.push(r);
    else skipped.push({ route: r.route, why });
  }
  return { open, skipped };
}

/// One route by name: a catalogued path, or a department's jobs view
/// (`/ux/departments/<code>`, one surface for every declared
/// department, which has no catalog entry of its own).
export function routeByName(
  route: string,
  catalog: Readonly<Record<string, CatalogEntry>> = ROUTE_CATALOG,
): PlannedAudit | undefined {
  const listed = catalogRoutes(catalog).find((a) => a.route === route);
  if (listed) return listed;
  const prefix = departmentJobsPath('');
  if (route.startsWith(prefix) && route.length > prefix.length) {
    const code = decodeURIComponent(route.slice(prefix.length));
    if (!code.includes('/')) {
      return { route, department: code, label: `${code} jobs view` };
    }
  }
  return undefined;
}

/// The packet body the jobs API admits: the route is the Subject and
/// rides metadata beside its department (page-audit's metadata_schema
/// requires both).
export function packetFor(audit: PlannedAudit, owner: string): Record<string, unknown> {
  return {
    kind: 'page-audit',
    title: `Page audit: ${audit.route} — ${audit.label} (${audit.department})`,
    tags: [],
    subject: { id: audit.route, subject_kind: 'custom' },
    owner_id: owner,
    status: 'open',
    priority: 'standard',
    metadata: { route: audit.route, department: audit.department },
  };
}

// ---------------------------------------------------------------------
// The door.
// ---------------------------------------------------------------------

export type Api = Readonly<{
  get: (path: string) => Promise<unknown>;
  post: (path: string, body: Record<string, unknown>) => Promise<unknown>;
}>;

/// `boss-api` spawned per call: BOSS_API names the script, else it is
/// on PATH. Non-2xx exits 1 there, and here that is a thrown error
/// carrying the door's stderr (its `HTTP:<code>` line and refusal).
export function bossApi(command: string = process.env['BOSS_API'] ?? 'boss-api'): Api {
  const call = async (method: string, path: string, body?: Record<string, unknown>) => {
    const argv = body === undefined ? [command, method, path] : [command, method, path, '-'];
    const proc = Bun.spawn(argv, {
      stdin: body === undefined ? 'ignore' : new Blob([JSON.stringify(body)]),
      stdout: 'pipe',
      stderr: 'pipe',
    });
    const [out, err, code] = await Promise.all([
      new Response(proc.stdout).text(),
      new Response(proc.stderr).text(),
      proc.exited,
    ]);
    if (code !== 0) {
      throw new Error(`${command} ${method} ${path} failed (exit ${code}): ${err.trim()}`);
    }
    return out.length === 0 ? null : (JSON.parse(out) as unknown);
  };
  return {
    get: (path) => call('GET', path),
    post: (path, body) => call('POST', path, body),
  };
}

/// Every existing page-audit, across pages: the list answers `total`,
/// and a page read as the whole would skip nothing it did not see.
export async function existingAudits(api: Api, pageSize = 200): Promise<ReadonlyArray<ExistingAudit>> {
  const rows: ExistingAudit[] = [];
  for (let offset = 0; ; offset += pageSize) {
    const page = (await api.get(
      `/api/jobs?kind=page-audit&limit=${pageSize}&offset=${offset}`,
    )) as { data?: unknown; total?: unknown } | null;
    const data = Array.isArray(page?.data) ? (page.data as ExistingAudit[]) : [];
    rows.push(...data);
    const total = typeof page?.total === 'number' ? page.total : rows.length;
    if (data.length === 0 || rows.length >= total) break;
  }
  return rows;
}

/// The actor by the door's rule: BOSS_ACTOR, else the actor file.
/// Blank is not an answer.
export function actorId(
  env: Readonly<Record<string, string | undefined>> = process.env,
  readFile: (path: string) => string | undefined = (p) => {
    try {
      return readFileSync(p, 'utf8');
    } catch {
      return undefined;
    }
  },
): string | undefined {
  const fromEnv = (env['BOSS_ACTOR'] ?? '').trim();
  if (fromEnv !== '') return fromEnv;
  const file = env['BOSS_ACTOR_FILE'] ?? `${env['HOME'] ?? ''}/.config/boss/actor`;
  const line = (readFile(file) ?? '').split('\n')[0]?.trim() ?? '';
  return line === '' ? undefined : line;
}

/// Open the plan's packets through the door, in order, and read each
/// one back — a 201 is a claim; the read-back is the fact.
export async function openAll(
  api: Api,
  audits: ReadonlyArray<PlannedAudit>,
  owner: string,
  log: (line: string) => void,
): Promise<ReadonlyArray<string>> {
  const ids: string[] = [];
  for (const a of audits) {
    const created = (await api.post('/api/jobs', packetFor(a, owner))) as { id?: unknown } | null;
    const id = typeof created?.id === 'string' ? created.id : undefined;
    if (id === undefined) {
      throw new Error(`the create for ${a.route} returned no id — refusing to call that opened`);
    }
    const back = (await api.get(`/api/jobs/${id}`)) as { id?: unknown } | null;
    if (back?.id !== id) {
      throw new Error(`created ${id} for ${a.route} and the API will not read it back`);
    }
    ids.push(id);
    log(`opened ${id.slice(0, 8)}  ${a.department.padEnd(12)} ${a.route}`);
  }
  return ids;
}

export function renderPlan(p: Plan): string {
  const lines = [
    `page-audit: ${p.open.length} to open, ${p.skipped.length} skipped`,
    ...p.open.map((a, i) => `  ${String(i + 1).padStart(2)}. ${a.department.padEnd(12)} ${a.route}  ${a.label}`),
    ...p.skipped.map((s) => `  skip ${s.route}: ${s.why}`),
  ];
  return lines.join('\n');
}

async function main(argv: ReadonlyArray<string>): Promise<number> {
  const dryRun = argv.includes('--dry-run');
  const all = argv.includes('--all');
  const routeFlag = argv.indexOf('--route');
  const route = routeFlag === -1 ? undefined : argv[routeFlag + 1];
  if (!dryRun && !all && route === undefined) {
    console.error('usage: open-page-audits.ts --dry-run | --all | --route <path>');
    return 2;
  }
  const wanted: ReadonlyArray<PlannedAudit> = route === undefined
    ? marchOrder()
    : (() => {
        const one = routeByName(route);
        if (one === undefined) {
          throw new Error(`${route} is not a catalogued route (apps/web/src/shell/nav-catalog.ts) nor a department jobs view`);
        }
        return [one];
      })();

  const api = bossApi();
  const p = plan(wanted, await existingAudits(api));
  console.log(renderPlan(p));
  if (dryRun) return 0;

  const owner = actorId();
  if (owner === undefined) {
    console.error(
      'open-page-audits: nothing names the actor running this command — set BOSS_ACTOR or write your id into $HOME/.config/boss/actor. Nothing was opened.',
    );
    return 2;
  }
  const ids = await openAll(api, p.open, owner, (l) => console.log(l));
  console.log(`page-audit: opened ${ids.length}, signed as ${owner}`);
  return 0;
}

if (import.meta.main) {
  main(process.argv.slice(2)).then(
    (code) => process.exit(code),
    (err: unknown) => {
      console.error(`open-page-audits: ${err instanceof Error ? err.message : String(err)}`);
      process.exit(1);
    },
  );
}
