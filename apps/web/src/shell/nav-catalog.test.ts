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
import { DEPARTMENTS, type AppId } from '@boss/web-kit/nav';
import {
  APPS,
  APP_SUBJECT_KINDS,
  ROUTE_CATALOG,
  appForDepartment,
  appForSection,
  departmentsWithoutSurfaces,
  type NavItem,
} from './nav-catalog';
import { readFileSync } from 'node:fs';

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
    // The train yard — the departure board over the pipeline's queues
    // and the IT app's guest-visible landing (departure-board.md Q1).
    // Its car landed without this line; added when the map arrived.
    'system-yard',
    // The network map — every registry station as a node
    // (stations.md: priority queues, stations, and network nodes are
    // one concept). No edges until motion is evented.
    // Incidents — active incident-post-mortem packets to respond to,
    // plus the closed ones rendered as a durable archive (David:
    // "both where we respond to active incidents and document post
    // mortems for posterity").
    'system-incidents',
    // The hardware registry page — declared beside observed, plus the
    // dev-workspace ssh door (59ef456a).
    'system-estate',
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
  it('the IT sidebar holds exactly the six consolidated rows, and every other IT surface is a tab or a documented door', () => {
    // The 2026-08-31 consolidation (packet 1f6d55e0): David — "we do
    // have too many IT pages though. We should consolidate." The
    // sidebar is EXACTLY six rows; every remaining IT catalog entry
    // must be reachable as a tab on one of them (ItTabs.svelte) or be
    // on the short documented list of parent-reached doors. This test
    // is also the executable "17 pages became 6" claim.
    const shell = readFileSync(
      new URL('./AppShell.svelte', import.meta.url),
      'utf8',
    );
    const groups = shell.slice(
      shell.indexOf('const IT_GROUPS'),
      shell.indexOf('// Home —'),
    );
    const SIDEBAR_SIX: ReadonlyArray<string> = [
      'system-yard',      // /it — the landing
      'system-incidents', // Operate
      'workflows',        // Registry
      'system-design',    // Design
      'system-estate',    // Estate
      'system-kb',        // Knowledge Base
    ];
    for (const k of SIDEBAR_SIX) {
      expect(
        groups.includes(`'${k}'`) || groups.includes(`ROUTE_CATALOG.${k}`),
        `sidebar row missing: ${k}`,
      ).toBe(true);
    }
    // No seventh row: count the catalog references inside IT_GROUPS.
    const rowRefs = (groups.match(/ROUTE_CATALOG(\.\w[\w-]*|\['[^']+'\])/g) ?? []).length;
    expect(rowRefs, 'the IT sidebar must hold exactly six rows').toBe(6);

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
      .filter(([k]) => !SIDEBAR_SIX.includes(k) && !DOCUMENTED_DOORS.includes(k))
      .filter(([, path]) => !tabs.includes(`'${path}'`));
    expect(
      unreachable.map(([k]) => k),
      `IT surfaces neither sidebar, tab, nor documented door: ${unreachable.map(([k]) => k).join(', ')}`,
    ).toEqual([]);
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

  it('every domain app owns at least one surface', () => {
    // A tab that renders an empty sidebar is a dead end. Simulator is
    // exempt: it is a separate SPA with no surfaces in this catalog.
    const owned = new Set(entries.map(([, v]) => v.app));
    for (const app of APPS) {
      if (app.id === 'simulator') continue;
      expect(owned.has(app.id), `app "${app.id}" has a tab but owns no surface`).toBe(true);
    }
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
  /// Department Classes, read from the files that seed them rather
  /// than restated here — restating is the drift this test exists to
  /// catch.
  ///
  /// They come from TWO places, which is itself worth knowing: the
  /// platform ships twelve (`01-registries.sql`), and the tenant adds
  /// its own (`examples/brewery/seeds/classes.json` adds production,
  /// packaging, taproom, maintenance, distribution, it, admin, audit).
  /// So `apps/web` — which is core — has to map departments a tenant
  /// invented. That works while one tenant ships in-tree; a second
  /// tenant with its own departments needs a real extension point.
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

  it('the DEPARTMENTS vocabulary matches the Class registry exactly', () => {
    // web-kit hardcodes the department list because the chrome bar
    // cannot wait on a fetch to know what tabs exist. This is the
    // equality test that keeps the copy honest in BOTH directions
    // (CLAUDE.md §9a) — it is what named `admin` the moment that
    // department was folded into finance.
    const registry: string[] = [...registryDepartments()].sort();
    const vocabulary: string[] = DEPARTMENTS.map((d) => d.code as string).sort();
    expect(
      vocabulary,
      'DEPARTMENTS in libs/web-kit/src/nav.ts has drifted from the Class registry ' +
        '(infra/postgres/schema/01-registries.sql + examples/brewery/seeds/classes.json)',
    ).toEqual(registry);
  });

  it('every app is a department, except Home and Simulator', () => {
    // The whole point of the change: "CRM is not a department for
    // example. The only exception to the department-based apps is the
    // Simulator." Home is the second exception — personal work belongs
    // to whoever is doing it, not to a department.
    const codes = new Set(DEPARTMENTS.map((d) => d.code));
    const invented = APPS.map((a) => a.id).filter(
      (id) => id !== 'home' && id !== 'simulator' && !codes.has(id as never),
    );
    expect(
      invented,
      `these apps name no department: ${invented.join(', ')}`,
    ).toEqual([]);
  });

  it('every department app has at least one surface behind it', () => {
    for (const app of APPS) {
      if (app.id === 'home' || app.id === 'simulator') continue;
      const owns = Object.values(ROUTE_CATALOG).some((e) => e.app === app.id);
      expect(owns, `app "${app.id}" has a tab but owns no surface`).toBe(true);
    }
  });

  it('an employee of any seeded department lands somewhere real', () => {
    // `audit` once mapped to no app at all and nothing failed. Every
    // department must resolve — to its own app if it owns a surface,
    // to Home if it does not.
    const tabbed = new Set(APPS.map((a) => a.id));
    for (const d of registryDepartments()) {
      expect(tabbed.has(appForDepartment(d)), `department "${d}" lands nowhere`).toBe(true);
    }
  });

  it('reports the departments with no surface of their own', () => {
    // Not a failure — a report, so the gap is visible rather than
    // reading as covered. These are real departments with real people
    // and no screen built for them yet; they land on Home.
    const bare = [...departmentsWithoutSurfaces()].sort();
    // `refurb` joined this list on 2026-08-28 when the /ux/refurb route
    // was removed from the shared shell (feedback 96c37dbe): it was a
    // device-shop surface every other tenant saw as an empty list, and
    // David's reason was that tenants connect at the boundary through
    // agreed protocols rather than sharing one multi-tenant shell. The
    // DEPARTMENT still exists and its people still exist — they just
    // land on Home, which is what this report is for.
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
      (k) => !known.has(k),
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
