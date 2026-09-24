// Router resolution snapshot. Pin every canonical path to its
// expected `Route` so a future router edit can't silently let a
// wildcard eclipse a specific case (a wildcard-before-specific
// ordering bug is the recurring failure mode; the `/finance`
// wildcard catches everything tail-shaped as `invoice`).
//
// Add an entry here every time `router.ts` learns a new path.
// Run via `bun test`.

import { beforeAll, describe, expect, test } from 'bun:test';
import { notFoundBack, parseRoute } from './router';

// `/jobs` reads `window.location.search` for filter query params.
// Stub a minimal window shape so the test runs in bun's
// non-DOM context.
beforeAll(() => {
  if (typeof (globalThis as { window?: unknown }).window === 'undefined') {
    (globalThis as { window: unknown }).window = {
      location: { search: '' },
    };
  }
});

describe('parseRoute — every specific path matches its specific case', () => {
  // Each entry: [path, expected partial-Route shape].
  // We don't check fields that come from URL-decoding (ids); only
  // that the discriminator `kind` is right + any positional param
  // is captured.
  const cases: Array<[string, Record<string, unknown>]> = [
    // User Experiences home — bare / and the canonical /ux both land on
    // My Day (the actor's personal work view) by default.
    ['/', { kind: 'me' }],
    ['/ux', { kind: 'me' }],
    // Browse / domain dashboards — User Experiences perspective (/ux/*).
    ['/ux/jobs', { kind: 'jobs' }],
    ['/ux/jobs/abc-123', { kind: 'jobDetail', jobId: 'abc-123' }],
    ['/ux/accounts', { kind: 'accounts' }],
    ['/ux/people', { kind: 'people' }],
    ['/ux/people/emp-aa-004', { kind: 'employee', empId: 'emp-aa-004' }],
    ['/ux/parts', { kind: 'parts' }],
    ['/ux/parts/SKU-1', { kind: 'part', partSku: 'SKU-1' }],
    ['/ux/products', { kind: 'products' }],
    ['/ux/products/FP-IPA-1-2-BBL', { kind: 'product', productSku: 'FP-IPA-1-2-BBL' }],
    // Finance — the regression nest. Specific cases MUST win over
    // the catch-all `/finance/(.+)` → invoice route.
    ['/ux/finance', { kind: 'finance' }],
    ['/ux/finance/new', { kind: 'newInvoice' }],
    ['/ux/finance/journal-entries/new', { kind: 'newJournalEntry' }],
    ['/ux/finance/inv-step-12345678', { kind: 'invoice', invoiceId: 'inv-step-12345678' }],
    // Shipping / shipments / support
    ['/ux/shipping', { kind: 'shipping' }],
    ['/ux/shipments/ship-1', { kind: 'shipmentDetail', shipmentId: 'ship-1' }],
    ['/ux/support', { kind: 'support' }],
    // Calendar / scheduling
    // The launch calendar retired with the second example tenant
    // (design 2ea444f5, backlog a8991c86): /ux/calendar is an unknown
    // path now and takes the catch-all; /ux/calendar/me is untouched.
    ['/ux/calendar', { kind: 'notFound', path: '/ux/calendar' }],
    // The landing page (the System Model live view) was reachable ONLY
    // through the catch-all until design ee3a3a2f gave it a door.
    ['/ux/system-model', { kind: 'home' }],
    ['/ux/calendar/me', { kind: 'myCalendar' }],
    ['/ux/service/schedule', { kind: 'schedule' }],
    // Exec (User Experiences)
    ['/ux/exec', { kind: 'exec' }],
    // A department's own jobs view — the tab landing for a department
    // that declares no surface (cc76f755). The code is registry data,
    // so the route carries it rather than the router knowing it.
    ['/ux/departments/sales', { kind: 'department', code: 'sales' }],
    ['/ux/departments/operations', { kind: 'department', code: 'operations' }],
    // System Model perspective — IT surfaces re-rooted under /system/*.
    // The consolidated IT department (1f6d55e0): six surfaces,
    // families as tabs, /system gone.
    ['/it/operate/perf', { kind: 'systemMonitoringPerf' }],
    ['/it/operate/audit', { kind: 'systemMonitoringEvents' }],
    ['/it/operate/atlas', { kind: 'systemMonitoringAtlas' }],
    ['/it/operate/bottlenecks', { kind: 'systemFleet' }],
    // The two queue boards retired as pages on car 4 of design
    // d2154293: each is a region of the world, and the old path
    // resolves to that region rather than 404ing.
    ['/it/operate/marshalling', { kind: 'systemYardFloor', region: 'marshalling' }],
    ['/it/operate/receiving', { kind: 'systemYardFloor', region: 'receiving' }],
    ['/it/yard/receiving', { kind: 'systemYardFloor', region: 'receiving' }],
    ['/it/operate/conductor', { kind: 'systemMonitoringConductor' }],
    ['/it/kb', { kind: 'systemKb' }],
    ['/it/registry/subjects', { kind: 'systemSubjects' }],
    // The Drift tab (4ae9969e): declared before the workflow-detail
    // wildcard, which would otherwise read 'drift' as a kind slug.
    ['/it/registry/drift', { kind: 'systemRegistryDrift' }],
    // /it/* is the canonical spelling for IT surfaces (0fc8b216); the
    // /system/* rows above stay because bookmarks, the station
    // registry's upstream hrefs and the docs all still use them.
    ['/it/design', { kind: 'systemDesign' }],
    ['/it/design/experiments', { kind: 'experiments' }],
    ['/it/design/feedback', { kind: 'systemFeedback' }],
    ['/it/design/codebase', { kind: 'systemCodebase' }],
    ['/it', { kind: 'systemYard' }],
    // The yard's FLOORS (design 0524fc95, car 2): /it is the map of
    // eight region cards; each yard card opens the Train Yard focused
    // on its panel, and the bare /it/yard is the yard on its default
    // selection, the track.
    ['/it/yard', { kind: 'systemYardFloor', region: 'track' }],
    ['/it/yard/dock', { kind: 'systemYardFloor', region: 'dock' }],
    ['/it/yard/shed', { kind: 'systemYardFloor', region: 'shed' }],
    ['/it/crew', { kind: 'systemCrew' }],
    ['/it/estate', { kind: 'systemEstate' }],
    
    ['/it/operate', { kind: 'incidents' }],
    
    
    
    ['/it/registry/step-plugins', { kind: 'systemStepPlugins' }],
    ['/it/registry/step-plugins/pour-quality-check', { kind: 'systemStepPluginDetail', pluginSlug: 'pour-quality-check' }],
    ['/it/registry/dispatcher', { kind: 'dispatcherRules' }],
    ['/it/registry/rules', { kind: 'dispatcherRulesList' }],
    ['/it/registry/rules/restock-on-low', { kind: 'dispatcherRuleEdit', ruleName: 'restock-on-low' }],
    // Warehouse / catalog / assets
    ['/ux/warehouse', { kind: 'warehouse' }],
    ['/ux/catalog', { kind: 'catalog' }],
    ['/ux/catalog/some-sku', { kind: 'device', sku: 'some-sku' }],
    ['/ux/assets', { kind: 'assets' }],
    ['/ux/assets/asset-1', { kind: 'asset', assetId: 'asset-1' }],
    ['/ux/marketing-assets', { kind: 'marketingAssets' }],
    ['/ux/marketing-assets/mkt-1', { kind: 'marketingAsset', assetId: 'mkt-1' }],
    // Manual + workflows + watchlist + shop
    ['/ux/manual', { kind: 'manual' }],
    ['/it/registry', { kind: 'workflows' }],
    ['/ux/manual/intro', { kind: 'manualSection', slug: 'intro' }],
    ['/ux/watchlist', { kind: 'watchlist' }],
    ['/ux/shop', { kind: 'shop' }],
    ['/ux/shop/FP-IPA-1-2-BBL', { kind: 'shopProduct', sku: 'FP-IPA-1-2-BBL' }],
    // PO + purchase orders
    ['/ux/purchase-orders/po-1', { kind: 'po', poId: 'po-1' }],
    ['/ux/vendor-invoices/vi-1', { kind: 'vendorInvoice', vendorInvoiceId: 'vi-1' }],
    // HR + QA + ops
    ['/ux/hr', { kind: 'hr' }],
    ['/ux/qa', { kind: 'qa' }],
    // Policy + Workflow authoring (System Model). The workflows
    // `/authoring/<jobId>` route is the wildcard-precedence trap: it MUST
    // resolve before the catch-all `/workflows/(.+)` detail route.
    ['/it/registry/policy', { kind: 'policy' }],
    ['/it/auth-admin', { kind: 'authAdmin' }],
    
    ['/it/registry/new', { kind: 'workflowNew' }],
    ['/it/registry/authoring/job-abc-123', { kind: 'workflowDesign', jobId: 'job-abc-123' }],
    ['/it/registry/seasonal-release', { kind: 'workflowDetail', kindSlug: 'seasonal-release' }],
  ];

  for (const [path, expected] of cases) {
    test(`${path}`, () => {
      const actual = parseRoute(path);
      for (const [key, value] of Object.entries(expected)) {
        expect((actual as Record<string, unknown>)[key]).toBe(value);
      }
    });
  }
});

// An unmatched path says so, and names the path (design ee3a3a2f,
// backlog c4f2ae24). The catch-all used to return the landing page for
// /ux and the yard for /it, so a dead link rendered a real, working,
// plausible page and the reader concluded they had misremembered.
describe('an unmatched path is notFound, naming the path', () => {
  test('an unknown path is notFound, not the landing page', () => {
    expect(parseRoute('/no-such-route')).toEqual({ kind: 'notFound', path: '/no-such-route' });
    expect(parseRoute('/ux/no-such-route')).toEqual({ kind: 'notFound', path: '/ux/no-such-route' });
  });

  test('an unknown /it path is notFound, not the yard', () => {
    expect(parseRoute('/it/no-such')).toEqual({ kind: 'notFound', path: '/it/no-such' });
    // /system retired with the IT consolidation; it is unknown like any other.
    expect(parseRoute('/it/system/yard').kind).toBe('notFound');
  });

  test('the path named is the one asked for, mount and all', () => {
    expect(parseRoute('/dashboard/ux/nope')).toEqual({ kind: 'notFound', path: '/dashboard/ux/nope' });
  });

  test('the deliberate /it aliases above the catch-all still answer', () => {
    expect(parseRoute('/it/operate/marshalling').kind).toBe('systemYardFloor');
    expect(parseRoute('/it/design/codebase').kind).toBe('systemCodebase');
  });

  test('the one back link goes to the department the path was under', () => {
    expect(notFoundBack('/it/no-such')).toEqual({ href: '/it', label: 'Back to the IT yard' });
    expect(notFoundBack('/dashboard/it/no-such/')).toEqual({ href: '/it', label: 'Back to the IT yard' });
    expect(notFoundBack('/ux/no-such')).toEqual({ href: '/ux', label: 'Back to My Day' });
    expect(notFoundBack('/items')).toEqual({ href: '/ux', label: 'Back to My Day' });
  });
});

// The single-id wildcards were greedy `(.+)`, so a deeper path under a
// list became a convincing "missing X": /ux/accounts/agreements/<id>
// rendered the ACCOUNT page for accountId "agreements/<id>", and
// /ux/sales/opportunities/<id> the JOB page. Narrowed to one segment
// (design ee3a3a2f Q6); an id holding a slash arrives percent-encoded.
describe('a single-id wildcard takes one segment', () => {
  test('the two motivating dead links are notFound, not a missing entity', () => {
    expect(parseRoute('/ux/accounts/agreements/x').kind).toBe('notFound');
    expect(parseRoute('/ux/sales/opportunities/x').kind).toBe('notFound');
  });

  const families = [
    '/ux/accounts', '/ux/vendors', '/ux/people', '/ux/parts', '/ux/products', '/ux/finance',
    '/ux/shipments', '/ux/catalog', '/ux/assets', '/ux/marketing-assets', '/ux/purchase-orders',
    '/ux/vendor-invoices', '/ux/shop', '/ux/service', '/ux/sales', '/ux/jobs',
    '/it/registry', '/it/registry/authoring', '/it/registry/step-plugins', '/it/registry/rules',
  ];
  for (const f of families) {
    test(`${f}/a/b is notFound`, () => {
      expect(parseRoute(`${f}/a/b`)).toEqual({ kind: 'notFound', path: `${f}/a/b` });
    });
  }

  test('an encoded slash is one segment, and arrives decoded', () => {
    expect(parseRoute('/ux/accounts/a%2Fb')).toEqual({ kind: 'account', accountId: 'a/b' });
    expect(parseRoute('/ux/people/a%2Fb')).toEqual({ kind: 'employee', empId: 'a/b' });
    expect(parseRoute('/ux/jobs/a%2Fb')).toEqual({ kind: 'jobDetail', jobId: 'a/b' });
    expect(parseRoute('/ux/service/a%2Fb')).toEqual({ kind: 'jobDetail', jobId: 'a/b' });
    expect(parseRoute('/ux/sales/a%2Fb')).toEqual({ kind: 'jobDetail', jobId: 'a/b' });
  });

  test('a malformed escape does not throw; the segment arrives as typed', () => {
    expect(parseRoute('/ux/accounts/%E0')).toEqual({ kind: 'account', accountId: '%E0' });
  });

  test('a manual section slug may hold a slash — the content API routes {*slug}', () => {
    expect(parseRoute('/ux/manual/ops/brewing')).toEqual({ kind: 'manualSection', slug: 'ops/brewing' });
  });

  test('a job id followed by a lone steps segment is not a job page', () => {
    expect(parseRoute('/ux/jobs/job-1/steps').kind).toBe('notFound');
  });
});

describe('parseRoute — wildcard does not shadow specific cases', () => {
  // Pins the canonical fix for the `/finance/(.+)` wildcard
  // precedence bug: specific cases MUST be declared before the
  // wildcard or they resolve as `{ kind: 'invoice', invoiceId: ... }`.
  // Any new `/finance/X` case the SPA introduces should be added to
  // the table above + a regression assertion here.
  test('/ux/finance/new → newInvoice, NOT invoice', () => {
    const r = parseRoute('/ux/finance/new');
    expect(r.kind).toBe('newInvoice');
  });
  test('/ux/finance/journal-entries/new → newJournalEntry, NOT invoice', () => {
    const r = parseRoute('/ux/finance/journal-entries/new');
    expect(r.kind).toBe('newJournalEntry');
  });
  test('/it/registry/authoring/X → workflowDesign, NOT workflowDetail', () => {
    const r = parseRoute('/it/registry/authoring/job-abc-123');
    expect(r.kind).toBe('workflowDesign');
  });
});

// The /ux/products search box writes its query back as `q` (backlog
// 1c2db4c2), so the route must hand it to the page on a reload or a
// shared link; absent and empty both mean no query.
describe('products list search from the query string', () => {
  const at = (search: string) => {
    (globalThis as { window?: { location: { search: string; pathname: string } } }).window = {
      location: { search, pathname: '/ux/products' },
    };
    return parseRoute('/ux/products') as { kind: string; q?: string };
  };

  test('a query is carried through', () => {
    const r = at('?q=pale+ale');
    expect(r.kind).toBe('products');
    expect(r.q).toBe('pale ale');
  });

  test('no query is the empty string, and is still the list route', () => {
    expect(at('')).toEqual({ kind: 'products', q: '' });
    expect(at('?q=')).toEqual({ kind: 'products', q: '' });
  });

  test('a product page does not read the list query', () => {
    const r = parseRoute('/ux/products/FP-IPA-1-2-BBL');
    expect(r).toEqual({ kind: 'product', productSku: 'FP-IPA-1-2-BBL' });
  });
});

// /ux/finance's tab, entry and fact ride in the query (backlog
// 2ab44d55): NewJournalEntryPage lands on ?entry=<id> after a post, and
// the route must hand that to the page rather than drop it.
describe('finance view from the query string', () => {
  const at = (search: string) => {
    (globalThis as { window?: { location: { search: string; pathname: string } } }).window = {
      location: { search, pathname: '/ux/finance' },
    };
    return parseRoute('/ux/finance');
  };

  test('a posted entry is carried through, onto the Trial Balance', () => {
    expect(at('?entry=ent-1')).toEqual({
      kind: 'finance',
      view: { tab: 'trial-balance', entry: 'ent-1', fact: '' },
    });
  });

  test('a fact and a tab are carried through', () => {
    expect(at('?fact=f-1')).toEqual({
      kind: 'finance',
      view: { tab: 'trial-balance', entry: '', fact: 'f-1' },
    });
    expect(at('?tab=invoices')).toEqual({
      kind: 'finance',
      view: { tab: 'invoices', entry: '', fact: '' },
    });
  });

  test('a bare /ux/finance is the Overview', () => {
    expect(at('')).toEqual({ kind: 'finance', view: { tab: 'overview', entry: '', fact: '' } });
  });

  test('an invoice page does not read the finance query', () => {
    expect(at('?entry=ent-1') && parseRoute('/ux/finance/inv-1')).toEqual({
      kind: 'invoice',
      invoiceId: 'inv-1',
    });
  });
});

describe('global search results route', () => {
  test('/search carries the query through', () => {
    (globalThis as { window?: { location: { search: string; pathname: string } } }).window = {
      location: { search: '?q=cascade', pathname: '/ux/search' },
    };
    const r = parseRoute('/ux/search');
    expect(r.kind).toBe('search');
    expect((r as { q: string }).q).toBe('cascade');
  });

  test('/search with no query is still the search route, not the catch-all', () => {
    (globalThis as { window?: { location: { search: string; pathname: string } } }).window = {
      location: { search: '', pathname: '/ux/search' },
    };
    const r = parseRoute('/ux/search');
    expect(r.kind).toBe('search');
    expect((r as { q: string }).q).toBe('');
  });
});

// An ABSENT status and an EMPTY one are two different requests. With
// no `status` the page defaults to open; `status=` is a deep link
// asking for every status, the same thing the page's own All button
// sets. EmployeePage links "View this employee's owned jobs" as
// `/ux/jobs?owner_id=…&status=`, and the router's truthiness check
// dropped the empty value, so that link showed open jobs only (backlog
// 03e198e5, found by page-audit 473f4f92 GAP 7).
describe('jobs list status filter from the query string', () => {
  const at = (search: string) => {
    (globalThis as { window?: { location: { search: string; pathname: string } } }).window = {
      location: { search, pathname: '/ux/jobs' },
    };
    return parseRoute('/ux/jobs') as { kind: string; jobStatus?: string };
  };

  test('an explicit empty status is carried as the empty string (all statuses)', () => {
    const r = at('?owner_id=emp-1&status=');
    expect(r.kind).toBe('jobs');
    expect(r.jobStatus).toBe('');
  });

  test('an absent status is left unset, so the page defaults to open', () => {
    const r = at('?owner_id=emp-1');
    expect('jobStatus' in r).toBe(false);
  });

  test('a named status is carried through', () => {
    expect(at('?status=closed').jobStatus).toBe('closed');
  });
});

// `filter_subject_kind` was parsed into the route and handed to the
// page, which captured it and never sent it: no link in the tree
// produced it and the jobs API's ListJobsQuery has no subject_kind
// filter, so it narrowed nothing. Deleted rather than implemented just
// in case (backlog 45ca0f89, found by page-audit 473f4f92 GAP 8).
describe('jobs list subject filter from the query string', () => {
  const at = (search: string) => {
    (globalThis as { window?: { location: { search: string; pathname: string } } }).window = {
      location: { search, pathname: '/ux/jobs' },
    };
    return parseRoute('/ux/jobs') as Record<string, unknown>;
  };

  test('filter_subject_kind is not a route field: nothing downstream reads it', () => {
    const r = at('?filter_subject_kind=account&subject_id=account-00001');
    expect(r.kind).toBe('jobs');
    expect('jobSubjectKind' in r).toBe(false);
  });

  test('subject_id still filters the list, the one subject filter the server takes', () => {
    expect(at('?subject_id=account-00001').jobSubjectId).toBe('account-00001');
  });
});

describe('personal Views route', () => {
  test('/views resolves to the Home Views surface', () => {
    expect(parseRoute('/ux/views').kind).toBe('views');
  });

  test('the bare alias resolves too', () => {
    // Every /ux/* surface is reachable without the prefix; the router
    // strips it before matching, and Views should be no exception.
    expect(parseRoute('/views').kind).toBe('views');
  });
});

describe('full-page step route', () => {
  test('parses /jobs/{job}/steps/{step} to stepFocus', () => {
    const r = parseRoute('/ux/jobs/job-123/steps/step-456');
    expect(r.kind).toBe('stepFocus');
    expect((r as { jobId: string }).jobId).toBe('job-123');
    expect((r as { stepId: string }).stepId).toBe('step-456');
  });

  test('carries the lens Back target through, with its label', () => {
    // David, 40fe7291: Back from a design review landed on the job
    // page instead of the queue he came from. Only the lens knows
    // where back is, so it says so on the URL.
    (globalThis as { window?: { location: { search: string } } }).window = {
      location: { search: '?from=%2Fit%2Fdesign&from_label=Design%20Review' },
    };
    const r = parseRoute('/ux/jobs/job-123/steps/step-456');
    expect(r.kind).toBe('stepFocus');
    expect((r as { from?: string }).from).toBe('/it/design');
    expect((r as { fromLabel?: string }).fromLabel).toBe('Design Review');
    (globalThis as { window: { location: { search: string } } }).window = {
      location: { search: '' },
    };
  });

  test('refuses a Back target that leaves the app', () => {
    // A `from` naming another origin would turn the Back button into
    // an open redirect. Protocol-relative `//host` is the one that
    // looks in-app at a glance, which is why it is tested by name.
    for (const hostile of ['//evil.example', 'https://evil.example', 'evil']) {
      (globalThis as { window?: { location: { search: string } } }).window = {
        location: { search: `?from=${encodeURIComponent(hostile)}` },
      };
      const r = parseRoute('/ux/jobs/job-123/steps/step-456');
      expect(r.kind).toBe('stepFocus');
      expect((r as { from?: string }).from).toBeUndefined();
    }
    (globalThis as { window: { location: { search: string } } }).window = {
      location: { search: '' },
    };
  });

  test('does not steal the plain job-detail route', () => {
    // The greedy /jobs/(.+) branch sits right after this one; if the
    // step pattern were looser (or ordered later) one of these two
    // would swallow the other.
    expect(parseRoute('/ux/jobs/job-123').kind).toBe('jobDetail');
    expect((parseRoute('/ux/jobs/job-123') as { jobId: string }).jobId).toBe('job-123');
  });

  test('a job id containing "steps" still resolves to jobDetail', () => {
    const r = parseRoute('/ux/jobs/steps');
    expect(r.kind).toBe('jobDetail');
  });
});
