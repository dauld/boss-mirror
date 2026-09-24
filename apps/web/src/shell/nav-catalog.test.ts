// The nav catalog is the single source of truth for which app owns a
// surface. These tests hold two lines.
//
// First, the de-duplication that made the app split safe: app
// membership used to live in two sets, in two vocabularies, in two
// files (AppShell's `MODEL_ROUTES: Set<RouteName>` for the sidebar,
// App.svelte's `MODEL_KINDS: Set<Route['kind']>` for the active tab),
// which had to agree for every routed surface or a page rendered under
// the wrong tab with nothing failing. That membership set is still
// pinned verbatim against the deleted list — it now belongs to the IT
// app rather than a top-level System Model tab, but which surfaces
// travel together has not changed.
//
// Second, the split itself: every surface belongs to exactly one app,
// every app the tab bar advertises can actually be reached, and no
// surface is stranded in an app with no tab.

import { describe, it, expect } from 'bun:test';
import type { AppId, Department } from '@boss/web-kit/nav';
import {
  APP_SUBJECT_KINDS,
  ROUTE_CATALOG,
  appForSection,
  appsFor,
  departmentJobsPath,
  departmentsWithoutSurfaces,
  inPerspective,
  type NavItem,
} from './nav-catalog';
import { parseRoute } from '../router';
import { readFileSync } from 'node:fs';

/// Department Classes, read from the files that seed them rather
/// than restated here — restating is the drift this test exists to
/// catch.
///
/// They come from TWO places, which is itself worth knowing: the
/// platform ships eleven (`01-registries.sql`), and the playground
/// tenant adds its own (`examples/brewery/seeds/classes.json`). Since
/// ce68f137 the SPA reads them from `/api/classes` at boot, so these
/// tests hand the seeded set to `appsFor` the way the shell hands it
/// the fetched one.
function registryDepartments(): ReadonlyArray<string> {
  const core = readFileSync(
    new URL('../../../../infra/postgres/schema/01-registries.sql', import.meta.url),
    'utf8',
  );
  const coreCodes = [
    ...core.matchAll(/\(\s*'employee',\s*'([a-z-]+)',\s*'[^']*',\s*'department'/g),
  ].map((m) => m[1]!);

  const tenant = JSON.parse(
    readFileSync(
      new URL('../../../../examples/brewery/seeds/classes.json', import.meta.url),
      'utf8',
    ),
  ) as ReadonlyArray<{ member_attribute?: string; code?: string }>;
  const tenantCodes = tenant
    .filter((c) => c.member_attribute === 'department' && c.code)
    .map((c) => c.code!);

  const all = [...new Set([...coreCodes, ...tenantCodes])];
  // A parser that silently matched nothing would make every
  // assertion below vacuous.
  expect(coreCodes.length).toBeGreaterThan(10);
  expect(tenantCodes.length).toBeGreaterThan(0);
  return all;
}

const SEEDED: ReadonlyArray<Department> = registryDepartments().map((code) => ({
  code,
  label: code,
}));

/// The bar the playground tenant gets: every seeded department, and
/// the Simulator because its manifest lists `sim = true`.
const APPS = appsFor(SEEDED, { simulator: true });

/// Verbatim copy of AppShell.svelte's deleted `MODEL_ROUTES`. These
/// surfaces have now moved wholesale from the retired `model` app
/// into `it` — the review resolved that IT is the department and
/// System Model lives inside it, rather than the two being separate
/// tabs (architecture-decisions.md §Step UX & frontend). The MEMBERSHIP
/// is still pinned verbatim: the app they belong to changed, which
/// surfaces belong together did not. If a future change moves a
/// surface into or out of the set, this list is the thing to update,
/// deliberately.
const LEGACY_MODEL_ROUTES: ReadonlyArray<string> = [
  'system-monitoring', 'system-step-plugins', 'system-dispatcher',
  'system-subjects', 'system-dispatcher-rules', 'system-dispatcher-rule',
  'system-kb', 'system-design', 'system-experiments', 'policy',
  // One entry, not two. `workflows` (the authoring row, already
  // dropped from the sidebar) and `workflows` (the catalog) were
  // separate keys pointing at the same path; the rename collapsed
  // them, which is what surfaced the redundancy.
  'workflows', 'auth-admin',
];

const entries = Object.entries(ROUTE_CATALOG) as ReadonlyArray<[string, NavItem]>;

describe('nav catalog — app assignment', () => {
  it('every catalog entry declares an app', () => {
    const missing = entries.filter(([, v]) => v.app === undefined).map(([k]) => k);
    expect(
      missing,
      `these surfaces declare no app and would render under whichever tab ` +
        `they happened to fall back to: ${missing.join(', ')}`,
    ).toEqual([]);
  });

  /// IT surfaces added SINCE the app split, listed explicitly.
  ///
  /// The pin below is about surfaces not silently CHANGING app; it was
  /// never meant to freeze IT at its 2026-08-05 size. Growth goes here
  /// deliberately, one line per surface, so the two properties stay
  /// separable: nothing drifted, and this is what we added.
  const IT_SURFACES_ADDED_SINCE: ReadonlyArray<string> = [
    // The feedback triage board — user-feedback Jobs, worked Kanban
    // style. New surface, not a moved one.
    'system-feedback',
    // The IT backlog board — the same TriageBoard pointed at
    // backlog-item Jobs (c1624b94: the backlog lost its page in the
    // consolidation). A route and a filter, not another board.
    'system-backlog',
    // The Operating System map — the executor network. Sits beside
    // the dispatcher cascade: same IT audience, different question
    // (job traffic, not rule wiring).
    // Flow — the team's own throughput, in wall-clock time. Distinct
    // from System Monitoring on purpose: monitoring answers what the
    // machine is doing, Flow answers what the people got through.
    // Fleet — every in-flight Job of a kind on its Workflow's DAG.
    // Beside Flow deliberately: Flow is throughput, Fleet is where
    // the work is piling up (queue-visibility Q4's depth signal).
    'system-fleet',
    // The Crew Board — the middle third of the operator surface: who is
    // building what, right now. New surface, and a new SIDEBAR ROW,
    // which the test below was written to forbid: David's decision on
    // backlog 04c5bbc0 (2026-09-11) read the proposal to make it a tab
    // in an existing family and overrode it.
    'system-crew',
    // The train yard — the departure board over the pipeline's queues
    // and the IT app's guest-visible landing (departure-board.md Q1).
    // Its car landed without this line; added when the map arrived.
    'system-yard',
    // The network map — every registry station as a node
    // (stations.md: priority queues, stations, and network nodes are
    // one concept). No edges until motion is evented.
    // Incidents — active incident packets to respond to,
    // plus the closed ones rendered as a durable archive (David:
    // "both where we respond to active incidents and document post
    // mortems for posterity").
    'system-incidents',
    // The hardware registry page — declared beside observed, plus the
    // dev-workspace ssh door (59ef456a).
    'system-estate',
    // The codebase — the tree's own numbers and trend; a row since
    // David's feedback 9827c699 (2026-09-14), formerly a Design tab.
    'system-codebase',
    // The Drift tab on Registry — the newest maintenance-protocol-drift
    // packet rendered (4ae9969e, car 2 of 8f4e9cc0). A TAB, not a row:
    // it gates under `workflows` like the registry it is a view of.
    'system-registry-drift',
    // The Receiving Yard and the Marshalling Yard — SIDEBAR ROWS onto
    // the two Operate tabs that already answered /it/operate/receiving
    // and /it/operate/marshalling. David's feedback 92921c2f
    // (2026-09-18): "graduate Receiving Yard and Marshalling Yard to
    // the left navbar ... the three yards plus the Crew Board as the
    // top 4"; design 55417146 decided the order. The routes did not
    // move; the rows are a second door onto the same pages.
    'system-receiving',
    'system-marshalling',
  ];

  it('the IT app contains the System Model set plus what we added deliberately', () => {
    const derived = entries
      .filter(([, v]) => v.app === 'it')
      .map(([k]) => k)
      .sort();
    const expected = [...LEGACY_MODEL_ROUTES, ...IT_SURFACES_ADDED_SINCE].sort();
    expect(derived).toEqual(expected);
  });

  // A route can sit in the catalog with `app: 'it'` and still be
  // unreachable, because AppShell builds the sidebar from its OWN
  // explicit list of groups. That is the same fact in two places, and
  // it drifted: the Operating System map shipped with a route, a
  // permission key and a catalog entry, and no way to click to it.
  //
  // Source-level because the groups live inside a component. Crude,
  // but it fails when someone adds an IT surface and forgets the
  // sidebar, which is exactly the mistake it exists for.
  it('the IT sidebar holds exactly ten rows, and every other IT surface is a tab or a documented door', () => {
    // The 2026-08-31 consolidation (packet 1f6d55e0): David — "we do
    // have too many IT pages though. We should consolidate." The
    // sidebar is EXACTLY seven rows; every remaining IT catalog entry
    // must be reachable as a tab on one of them (ItTabs.svelte) or be
    // on the short documented list of parent-reached doors. This test
    // is also the executable "17 pages became 6, plus one decided since"
    // claim.
    const shell = readFileSync(
      new URL('./AppShell.svelte', import.meta.url),
      'utf8',
    );
    const groups = shell.slice(
      shell.indexOf('const IT_GROUPS'),
      shell.indexOf('// Home —'),
    );
    const SIDEBAR_ROWS: ReadonlyArray<string> = [
      'system-receiving',   // Receiving Yard — see below
      'system-marshalling', // Marshalling Yard — see below
      'system-yard',        // /it — the landing
      'system-crew',        // Crew Board — see below
      'system-incidents',   // Operate
      'workflows',          // Registry
      'system-design',      // Design
      'system-codebase',    // Codebase — see below
      'system-estate',      // Estate
      'system-kb',          // Knowledge Base
    ];
    for (const k of SIDEBAR_ROWS) {
      expect(
        groups.includes(`'${k}'`) || groups.includes(`ROUTE_CATALOG.${k}`),
        `sidebar row missing: ${k}`,
      ).toBe(true);
    }
    // No EIGHTH row: count the catalog references inside IT_GROUPS.
    //
    // This was six until 2026-09-11. The seventh is the Crew Board, and
    // the bar for adding it was a decision, not a convenience: the
    // proposal on backlog 04c5bbc0 was a tab inside an existing family,
    // citing this very count, and David answered "Port the Crew Board as
    // a new sidebar page in IT" with `accepted_as_proposed: false`. The
    // count still exists and still bites — the consolidation's point was
    // that a family belongs behind one row — so a new row needs the same
    // kind of answer, not an edit to this line.
    //
    // The eighth is the Codebase, and it has one: David's feedback
    // 9827c699 (2026-09-14), "Let's add a page to the IT department
    // showing the Code base stats", filed while the trend was a tab on
    // Design. The tab is gone; the row is the page.
    //
    // The ninth and tenth are the Receiving Yard and the Marshalling
    // Yard, and they have one too: David's feedback 92921c2f
    // (2026-09-18), "Let's graduate Receiving Yard and Marshalling Yard
    // to the left navbar. We can have the three yards plus the Crew
    // Board as the top 4." Design 55417146 settled the order (the test
    // below). The tabs STAY: the row is a second door onto the same page.
    const rowRefs = (groups.match(/ROUTE_CATALOG(\.\w[\w-]*|\['[^']+'\])/g) ?? []).length;
    expect(rowRefs, 'the IT sidebar must hold exactly ten rows').toBe(10);

    const tabs = readFileSync(
      new URL('../it/ItTabs.svelte', import.meta.url),
      'utf8',
    );
    // Doors reached from a parent page rather than sidebar or tabs.
    const DOCUMENTED_DOORS: ReadonlyArray<string> = [
      'system-dispatcher-rules', // reached from the cascade
      'system-dispatcher-rule',
      'auth-admin',              // unlisted by design (1f6d55e0)
      'system-monitoring',       // the permKey behind Operate's gated tabs
    ];
    const unreachable = entries
      .filter(([, v]) => v.app === 'it')
      .map(([k, v]) => [k, v.path] as const)
      .filter(([k]) => !SIDEBAR_ROWS.includes(k) && !DOCUMENTED_DOORS.includes(k))
      .filter(([, path]) => !tabs.includes(`'${path}'`));
    expect(
      unreachable.map(([k]) => k),
      `IT surfaces neither sidebar, tab, nor documented door: ${unreachable.map(([k]) => k).join(', ')}`,
    ).toEqual([]);
  });

  it('the IT sidebar leads with the three yards and the Crew Board, and the IT tab still lands on the Train Yard', () => {
    // Design 55417146 (answers feedback 92921c2f, David 2026-09-18):
    // flow order, upstream to downstream — Receiving Yard, Marshalling
    // Yard, Train Yard, Crew Board — then the department's desk work.
    // The sidebar order is AppShell's IT_GROUPS list; the landing is a
    // DIFFERENT mechanism (the first `app: 'it'` entry in catalog
    // order, `departmentHref`), which is why the Train Yard can be the
    // third row and still the page the IT tab opens on: "the departure
    // board is what an operator opens the department to see".
    const shell = readFileSync(new URL('./AppShell.svelte', import.meta.url), 'utf8');
    const groups = shell.slice(shell.indexOf('const IT_GROUPS'), shell.indexOf('// Home —'));
    const rows = [...groups.matchAll(/ROUTE_CATALOG(?:\.(\w[\w-]*)|\['([^']+)'\])/g)].map(
      (m) => m[1] ?? m[2],
    );
    expect(rows.slice(0, 4)).toEqual([
      'system-receiving',
      'system-marshalling',
      'system-yard',
      'system-crew',
    ]);
    expect(rows.slice(4)).toEqual([
      'system-incidents',
      'workflows',
      'system-design',
      'system-codebase',
      'system-estate',
      'system-kb',
    ]);
    // The labels David used, on the rows he asked for.
    expect(ROUTE_CATALOG['system-receiving'].label).toBe('Receiving Yard');
    expect(ROUTE_CATALOG['system-marshalling'].label).toBe('Marshalling Yard');
    // Since car 4 of design d2154293 both rows are ZOOM LINKS: the
    // yards are regions of the world, and their board mounts under
    // the zoomed territory. The old /it/operate paths still resolve
    // to the same route — one surface, two spellings.
    expect(ROUTE_CATALOG['system-receiving'].path).toBe('/it/yard/receiving');
    expect(ROUTE_CATALOG['system-marshalling'].path).toBe('/it/yard/marshalling');
    expect(parseRoute('/it/yard/receiving')).toEqual({ kind: 'systemYardFloor', region: 'receiving' });
    expect(parseRoute('/it/operate/receiving')).toEqual({ kind: 'systemYardFloor', region: 'receiving' });
    expect(parseRoute('/it/operate/marshalling')).toEqual({ kind: 'systemYardFloor', region: 'marshalling' });
    // And the IT tab still opens on the Train Yard at /it.
    expect(APPS.find((a) => a.id === 'it')?.href).toBe(ROUTE_CATALOG['system-yard'].path);
    expect(ROUTE_CATALOG['system-yard'].path).toBe('/it');
  });

  // A row a fixed-perspective group lists must be one that perspective
  // can render. AppShell's visible() runs inPerspective on every row,
  // which drops any catalog row whose app is not the app being
  // rendered — so a row listed under
  // the wrong app is dead text: no role ever sees it, and nothing says
  // so. Home's Mine group carried `exec` (app executive) that way until
  // backlog e8fe5e5a (2026-09-24), one group down from the Work group's
  // role lists (0f9be7c0). Exec was never reachable through Home; it is
  // the Executive app's row, and every department tab is offered to
  // every role (appsFor takes the departments alone), so removing the
  // dead row takes no route away from anyone.
  //
  // The department groups ride the same check (backlog 72a88031,
  // 2026-09-24): Production's Products row carries permKey `parts`, the
  // gate it shares with Warehouse's Ingredients & parts, and the rule
  // then looked its app up THROUGH that permKey — warehouse — so
  // Production dropped the row for every role. The rule is now the
  // shell's own function, imported here rather than restated, so the
  // pin judges the rows the way the sidebar does.
  it('every row the Home, IT and department sidebars list is one that app renders', () => {
    const shell = readFileSync(new URL('./AppShell.svelte', import.meta.url), 'utf8');
    const between = (from: string, to: string): string => {
      const start = shell.indexOf(from);
      const end = shell.indexOf(to, start);
      // A marker that moved would make the slice empty and the check
      // vacuous; refuse that rather than pass it.
      expect(start, `marker not found: ${from}`).toBeGreaterThanOrEqual(0);
      expect(end, `marker not found: ${to}`).toBeGreaterThan(start);
      return shell.slice(start, end);
    };
    const rowsOf = (src: string): ReadonlyArray<string> =>
      [...src.matchAll(/ROUTE_CATALOG(?:\.(\w[\w-]*)|\['([^']+)'\])/g)].map((m) => (m[1] ?? m[2])!);
    const groups: ReadonlyArray<readonly [AppId, ReadonlyArray<string>]> = [
      ['home', rowsOf(between('const WORK', 'const IT_GROUPS'))],
      ['home', rowsOf(between('const HOME_GROUPS', 'let MAIN'))],
      ['it', rowsOf(between('const IT_GROUPS', '// Home —'))],
    ];
    // Every department group: APP_SURFACES, one `app: ['row', ...]`
    // line per department — the list the shell maps into that app's
    // sidebar.
    const surfaces = between('const APP_SURFACES', '};');
    const departmentGroups = [...surfaces.matchAll(/^\s*([\w-]+|'[^']+'):\s*\[([^\]]*)\]/gm)].map(
      (m) =>
        [
          m[1]!.replace(/'/g, '') as AppId,
          [...m[2]!.matchAll(/'([^']+)'/g)].map((r) => r[1]!),
        ] as const,
    );
    expect(departmentGroups.length, 'APP_SURFACES lists no department').toBeGreaterThan(0);
    const allGroups = [...groups, ...departmentGroups];
    for (const [, rows] of allGroups) expect(rows.length).toBeGreaterThan(0);
    const catalog = ROUTE_CATALOG as Readonly<Record<string, NavItem | undefined>>;
    const dead = allGroups.flatMap(([app, rows]) =>
      rows
        .filter((k) => {
          const item = catalog[k];
          return item === undefined || !inPerspective(item, app);
        })
        .map((k) => `${k} (listed under ${app}, app ${catalog[k]?.app ?? 'none'})`),
    );
    expect(dead, `sidebar rows no role can ever see: ${dead.join(', ')}`).toEqual([]);
  });

  it('nothing from the original System Model set has left the IT app', () => {
    // The half of the pin that matters most: a surface silently
    // changing app is the failure this list was written for.
    const inIt = new Set(entries.filter(([, v]) => v.app === 'it').map(([k]) => k));
    const missing = LEGACY_MODEL_ROUTES.filter((r) => !inIt.has(r));
    expect(missing, `these left the IT app: ${missing.join(', ')}`).toEqual([]);
  });

  it('every surface lands in an app the chrome bar actually offers', () => {
    const tabbed = new Set<AppId>(APPS.map((a) => a.id));
    for (const [name, item] of entries) {
      expect(
        tabbed.has(item.app as AppId),
        `${name} is assigned to app "${item.app}", which has no tab in APPS — ` +
          `it would be unreachable.`,
      ).toBe(true);
    }
  });

  it('no surface is left in the retired catch-all "user" app', () => {
    // `/ux` was one tab holding 24 surfaces. The split exists to end
    // that; a straggler here means a surface nobody re-homed.
    const stragglers = entries
      .filter(([, v]) => (v.app as string) === 'user')
      .map(([k]) => k);
    expect(stragglers).toEqual([]);
  });

  it('no surface is left in the retired "model" app', () => {
    // System Model stopped being a top-level app when the review made
    // IT the department that owns it. A straggler here would render
    // under a tab that no longer exists.
    const stragglers = entries
      .filter(([, v]) => (v.app as string) === 'model')
      .map(([k]) => k);
    expect(stragglers).toEqual([]);
  });

  it('IT is a department app, not a second model-facing tab', () => {
    // The decision (Q2) was that IT is a department like Finance or
    // People. Pinning its presence and Simulator's separateness keeps
    // a later reshuffle from quietly recreating the two-model-tabs
    // shape the review rejected.
    const ids = APPS.map((a) => a.id);
    expect(ids).toContain('it');
    expect(ids).not.toContain('model');
    expect(ids.indexOf('it')).toBeGreaterThan(ids.indexOf('simulator'));
  });

  it('a department app that owns no surface lands on its own jobs view, not All jobs', () => {
    // A tab that renders an empty sidebar is a dead end. Since the tabs
    // are the registry's (ce68f137), a department can own nothing here;
    // its tab then opens the department's jobs view — in / working /
    // out over the packets whose workflow declares it (cc76f755). It
    // used to open All jobs with Home highlighted, which read as "this
    // department has no work". Simulator is exempt: it is a separate
    // SPA with no surfaces in this catalog.
    const owned = new Set(entries.map(([, v]) => v.app));
    let checked = 0;
    for (const app of APPS) {
      if (app.id === 'simulator' || owned.has(app.id)) continue;
      checked += 1;
      expect(app.href, `app "${app.id}" owns no surface and lands on ${app.href}`).toBe(
        departmentJobsPath(app.id),
      );
      expect(app.href).not.toBe(ROUTE_CATALOG.jobs.path);
      // And the router answers that path with the department itself.
      expect(parseRoute(app.href)).toEqual({ kind: 'department', code: app.id });
    }
    expect(checked, 'the seeded registry has surface-less departments to check').toBeGreaterThan(0);
  });
});

describe('appForSection — the App.svelte tab derivation', () => {
  it('resolves surfaces to their app', () => {
    expect(appForSection('system-yard')).toBe('it');
    expect(appForSection('accounts')).toBe('sales');
    expect(appForSection('finance')).toBe('finance');
    // 'All jobs' stays on Home deliberately. It is the cross-cutting
    // queue, and pinning it to one department would be the thing
    // feedback 4b454768 objected to: "I shouldn't be jerked around
    // through apps as I work."
    expect(appForSection('jobs')).toBe('home');
    expect(appForSection('warehouse')).toBe('warehouse');
    expect(appForSection('shipping')).toBe('distribution');
    expect(appForSection('marketing-assets')).toBe('marketing');
    expect(appForSection('people')).toBe('people');
    expect(appForSection('inbox')).toBe('home');
  });

  it('falls back to Home for unknown sections', () => {
    // `me` is App.svelte's terminal fallback in the activeSection
    // ternary and has no catalog entry. Home is where personal
    // surfaces live, so that is the right landing for the fallback.
    expect(appForSection('me')).toBe('home');
    expect(appForSection('definitely-not-a-section')).toBe('home');
  });
});

describe('departments map to apps', () => {
  it('every catalog app names a seeded department', () => {
    // The other half of "apps are departments": the tabs are the
    // registry's now, so a catalog entry assigned to an app the
    // registry does not declare would render under no tab at all.
    const codes = new Set(SEEDED.map((d) => d.code));
    const invented = entries
      .map(([, v]) => v.app)
      .filter((a): a is string => a !== undefined && a !== 'home' && a !== 'simulator')
      .filter((a) => !codes.has(a));
    expect(
      [...new Set(invented)],
      `these catalog apps name no department in 01-registries.sql or the playground seed: ${invented.join(', ')}`,
    ).toEqual([]);
  });

  it('every app is a department, except Home and Simulator', () => {
    // The whole point of the change: "CRM is not a department for
    // example. The only exception to the department-based apps is the
    // Simulator." Home is the second exception — personal work belongs
    // to whoever is doing it, not to a department.
    const codes = new Set(SEEDED.map((d) => d.code));
    const invented = APPS.map((a) => a.id).filter(
      (id) => id !== 'home' && id !== 'simulator' && !codes.has(id),
    );
    expect(
      invented,
      `these apps name no department: ${invented.join(', ')}`,
    ).toEqual([]);
  });

  it('every seeded department has a tab, in registry order', () => {
    // A tab per department the tenant declares (ce68f137). `audit`
    // once mapped to no app at all and nothing failed; a second
    // tenant's `operations` had no tab because a hardcoded list did
    // not know it.
    const tabbed = APPS.filter((a) => a.id !== 'home' && a.id !== 'simulator').map((a) => a.id);
    expect(tabbed).toEqual(SEEDED.map((d) => d.code));
  });

  it('every department tab lands on a real surface', () => {
    // Its first owned surface in catalog order, or its own jobs view
    // when it owns none — never an empty page, and never the
    // catch-all.
    const paths = new Set(entries.map(([, v]) => v.path));
    for (const app of APPS) {
      if (app.id === 'home' || app.id === 'simulator') continue;
      const owned = entries.find(([, v]) => v.app === app.id);
      if (owned) {
        expect(paths.has(app.href), `app "${app.id}" lands on ${app.href}, which no surface answers`).toBe(true);
        expect(app.href).toBe(owned[1].path);
      } else {
        expect(app.href).toBe(departmentJobsPath(app.id));
      }
      expect(parseRoute(app.href).kind, `app "${app.id}" lands on the catch-all`).not.toBe('home');
    }
  });

  it('the department jobs path round-trips through the router, code included', () => {
    // The path is the one spelling both halves share: the tab (and
    // the sidebar row) build it here, the router parses it. A code
    // with a character the URL would eat must survive the trip.
    for (const code of ['sales', 'operations', 'front of house']) {
      expect(parseRoute(departmentJobsPath(code))).toEqual({ kind: 'department', code });
    }
  });

  it('a tenant with no departments gets Home alone, not a crash', () => {
    expect(appsFor([], { simulator: false }).map((a) => a.id)).toEqual(['home']);
  });

  it('the Simulator tab is the sim module, not a fixture of the bar', () => {
    expect(appsFor(SEEDED, { simulator: false }).map((a) => a.id)).not.toContain('simulator');
    expect(appsFor(SEEDED, { simulator: true }).map((a) => a.id)).toContain('simulator');
  });

  it('reports the departments with no surface of their own', () => {
    // Not a failure — a report, so the gap is visible rather than
    // reading as covered. These are real departments with real people
    // and no screen built for them yet; their tab lands on All jobs.
    const bare = [...departmentsWithoutSurfaces(SEEDED)].sort();
    // `refurb` joined this list on 2026-08-28 when the /ux/refurb route
    // was removed from the shared shell (feedback 96c37dbe): it was a
    // device-shop surface every other tenant saw as an empty list, and
    // David's reason was that tenants connect at the boundary through
    // agreed protocols rather than sharing one multi-tenant shell. The
    // DEPARTMENT still exists and its people still exist.
    expect(bare).toEqual(['audit', 'packaging', 'refurb', 'taproom']);
  });
});

describe('every concrete Subject kind is claimed by an app', () => {
  /// The subject-kind registry is a taxonomy, not a flat list: rows
  /// carry a `parent_kind`, and the roots with children (`person`,
  /// `object`, `intangible`) are abstract — `account` specializes
  /// `person`, and nothing is ever of kind `person` itself. So this
  /// exempts roots-with-children structurally rather than naming them,
  /// which means a future abstract root is exempt automatically and a
  /// future concrete kind is not.
  ///
  /// Rows look like:
  ///   ('account', 'Account', 'desc…', 'platform', 10, 'person'),
  /// with the kind first and parent_kind last. Descriptions contain
  /// commas and parentheses, so this anchors on those two positions
  /// rather than splitting fields.
  function taxonomy(): ReadonlyArray<{ kind: string; parent: string | null }> {
    const sql = readFileSync(
      new URL('../../../../infra/postgres/schema/01-registries.sql', import.meta.url),
      'utf8',
    );
    // Walk lines from the INSERT to the statement terminator. Slicing
    // on the first `;` truncated the block at a semicolon INSIDE a
    // description ("one row per tenant; the subject…"), which silently
    // yielded only the six root rows.
    const lines = sql.slice(sql.indexOf('INSERT INTO subject_kinds')).split('\n');
    const rows: Array<{ kind: string; parent: string | null }> = [];
    for (const line of lines) {
      const isLast = line.trimEnd().endsWith(';');
      // Kind is the first quoted token; parent_kind is the last field.
      // Parsed positionally rather than by one big regex — the
      // descriptions carry commas, parens, quotes and em-dashes, and a
      // regex threading past all of them matched only the rows ending
      // in NULL.
      const kind = /^\s*\('([a-z_-]+)'/.exec(line)?.[1];
      if (!kind) {
        if (isLast) break;
        continue;
      }
      // Strip the row's closing `),` FIRST — otherwise the last comma
      // in the line is the trailing one and the field comes back empty.
      const inner = line.trim().replace(/\),?$/, '');
      const last = inner.slice(inner.lastIndexOf(',') + 1).trim();
      const parent = last.startsWith('NULL')
        ? null
        : (/^'([a-z_-]+)'/.exec(last)?.[1] ?? null);
      rows.push({ kind, parent });
      // Terminator checked AFTER parsing: the final row ends `NULL);`,
      // so breaking first silently dropped it — and it was `custom`,
      // one of the two kinds this whole test exists to catch.
      if (isLast) break;
    }
    // A parser that silently matched nothing — or only some rows —
    // would make every assertion below vacuous. It did exactly that
    // once.
    expect(rows.length).toBeGreaterThan(15);
    expect(rows.filter((r) => r.parent !== null).length).toBeGreaterThan(5);
    return rows;
  }

  it('claims every kind that is not an abstract root', () => {
    const rows = taxonomy();
    const hasChildren = new Set(rows.map((r) => r.parent).filter(Boolean) as string[]);
    const claimed = new Set(Object.values(APP_SUBJECT_KINDS).flat());

    const unclaimed = rows
      .filter((r) => !(r.parent === null && hasChildren.has(r.kind)))
      .map((r) => r.kind)
      .filter((k) => !claimed.has(k));

    expect(
      unclaimed,
      `concrete Subject kinds no app claims, so search never floats them: ` +
        unclaimed.join(', '),
    ).toEqual([]);
  });

  it('claims nothing the registry does not define', () => {
    const known = new Set(taxonomy().map((r) => r.kind));
    const stale = [...new Set(Object.values(APP_SUBJECT_KINDS).flat())].filter(
      (k): k is string => k !== undefined && !known.has(k),
    );
    expect(stale, `claimed but not a registered kind: ${stale.join(', ')}`).toEqual([]);
  });

  it('leaves the abstract roots unclaimed', () => {
    // The other direction: claiming `person` would rank a kind that
    // has no instances, which is noise in every result set.
    const claimed = new Set(Object.values(APP_SUBJECT_KINDS).flat());
    for (const root of ['person', 'object', 'intangible']) {
      expect(claimed.has(root), `${root} is abstract and should not be claimed`).toBe(
        false,
      );
    }
  });
});

describe('a surface that lists a department names the department', () => {
  // Backlog 423a531d (2026-09-22). The Service queue and the Sales
  // pipeline were mounted with a hardcoded workflow kind —
  // `field-service` and `sale`, both authored only in a tenant's seed
  // bundle and published by no instance running another tenant — so
  // both rendered a title and a permanent "No jobs match", with
  // nothing holding the literal to anything. What a surface FILTERS on
  // now lives in the catalog beside its path and its app, as a
  // DEPARTMENT: a department's work is several protocols (Sales runs
  // receive-a-sponsorship AND receive-an-inquiry), so no single kind
  // could have expressed it even once corrected.
  const SEEDED_CODES = new Set(registryDepartments());

  it('every declared department code is one the Class registry seeds', () => {
    const declared = entries.filter(([, e]) => e.department !== undefined);
    expect(declared.length).toBeGreaterThan(0);
    for (const [key, e] of declared) {
      expect([key, SEEDED_CODES.has(e.department!)]).toEqual([key, true]);
    }
  });

  it('the two jobs-queue surfaces carry one, so neither needs a kind literal', () => {
    expect(ROUTE_CATALOG.sales.department).toBe('sales');
    expect(ROUTE_CATALOG.service.department).toBe('support');
  });

  // Backlog 044dffa1 (2026-09-23, page audit 63d810aa): /ux/parts made
  // four reads and none was a jobs read, so a warehouse packet — once a
  // protocol declares the department — could never appear on the
  // warehouse's own page. The department rides here, beside the path,
  // for the reason the two queues' do.
  it('the parts surface lists the warehouse department it sits under', () => {
    expect(ROUTE_CATALOG.parts.app).toBe('warehouse');
    expect(ROUTE_CATALOG.parts.department).toBe('warehouse');
  });

  // Backlog 4d4dc204 (2026-09-23, page audit 3f964c57 gap 2): /ux/finance
  // made no jobs read and linked nowhere that did, so a receive-a-payout
  // packet waiting at its post step for 2.6 days was on no finance
  // surface. Same key, same reason as the warehouse's above.
  it('the finance surface lists the finance department it sits under', () => {
    expect(ROUTE_CATALOG.finance.app).toBe('finance');
    expect(ROUTE_CATALOG.finance.department).toBe('finance');
  });
});
