// The opener's plan: order, idempotence, department mapping, and the
// door's contract — against a stubbed API, never the system of record.

import { describe, expect, it } from 'bun:test';
import { ROUTE_CATALOG } from '../src/shell/nav-catalog';
import {
  type Api,
  type ExistingAudit,
  FIRST_DEPARTMENTS,
  HOME_DEPARTMENT,
  actorId,
  catalogRoutes,
  existingAudits,
  marchOrder,
  openAll,
  packetFor,
  plan,
  routeByName,
} from './open-page-audits';

describe('the roster is the catalog', () => {
  it('one audit per catalogued path, once, and nothing the catalog does not list', () => {
    const routes = catalogRoutes();
    const paths = routes.map((r) => r.route);
    expect(new Set(paths).size).toBe(paths.length);
    const catalogued = new Set(
      Object.values(ROUTE_CATALOG)
        .map((e) => e.path)
        .filter((p) => !p.includes(':')),
    );
    expect(new Set(paths)).toEqual(catalogued);
    // The design measured 45 on #451; the number is the catalog's to
    // move, and this line is what makes a move visible in the diff.
    expect(paths.length).toBe(45);
    // The doubled key (two rule-editor ids on one path) collapses.
    expect(paths.filter((p) => p === '/it/registry/rules')).toHaveLength(1);
  });

  it('the department is the app the catalog assigns, and Home is IT', () => {
    const by = new Map(catalogRoutes().map((r) => [r.route, r.department]));
    expect(by.get('/ux/support')).toBe('support');
    expect(by.get('/ux/sales')).toBe('sales');
    expect(by.get('/ux/vendors')).toBe('finance');
    expect(by.get('/it/design')).toBe('it');
    for (const home of ['/ux/jobs', '/ux/inbox', '/ux/views', '/ux/calendar/me', '/manual']) {
      expect(by.get(home), home).toBe(HOME_DEPARTMENT);
    }
    // Every department is a code the catalog's `app` names, so the
    // readiness read has something to answer for — never `home`.
    expect([...new Set(by.values())]).not.toContain('home');
  });
});

describe('the march order', () => {
  it('walks the departments whose first protocols land first, others next, Home, then IT', () => {
    const order = marchOrder();
    const depts = order.map((r) => r.department);
    // The decided head, in the decided order, each department's routes
    // contiguous.
    const firstSeen = (d: string) => depts.indexOf(d);
    const present = FIRST_DEPARTMENTS.filter((d) => depts.includes(d));
    expect(present).toEqual(['support', 'sales', 'finance', 'marketing']);
    for (let i = 1; i < present.length; i += 1) {
      expect(firstSeen(present[i]!)).toBeGreaterThan(firstSeen(present[i - 1]!));
    }
    expect(order[0]!.route).toBe('/ux/support');
    // Nothing from the head appears after a department outside it.
    const lastHead = Math.max(...present.map((d) => depts.lastIndexOf(d)));
    const firstOther = depts.findIndex((d) => !FIRST_DEPARTMENTS.includes(d));
    expect(firstOther).toBeGreaterThan(lastHead);
    // IT's own surfaces are the tail, and Home's sit just before them.
    const its = order.filter((r) => r.route.startsWith('/it'));
    expect(order.slice(-its.length)).toEqual(its);
    const homes = order.filter((r) => r.department === HOME_DEPARTMENT && !r.route.startsWith('/it'));
    expect(order.slice(-its.length - homes.length, -its.length)).toEqual(homes);
    expect(homes.map((h) => h.route)).toContain('/ux/inbox');
  });

  it('is stable within a department: catalog order', () => {
    const sales = marchOrder()
      .filter((r) => r.department === 'sales')
      .map((r) => r.route);
    const catalogSales = Object.values(ROUTE_CATALOG)
      .filter((e) => e.app === 'sales')
      .map((e) => e.path);
    expect(sales).toEqual(catalogSales);
  });
});

describe('idempotence', () => {
  const routes = [
    { route: '/ux/support', department: 'support', label: 'Support' },
    { route: '/ux/sales', department: 'sales', label: 'Sales pipeline' },
    { route: '/it', department: 'it', label: 'Train Yard' },
  ] as const;
  const existing = (status: string, route: string, outcome?: string): ExistingAudit => ({
    status,
    metadata: outcome === undefined ? { route } : { route, outcome },
  });

  it('skips a route with an open packet and one closed audited; a withdrawn one does not block', () => {
    const p = plan(routes, [
      existing('open', '/ux/support'),
      existing('closed', '/ux/sales', 'audited'),
      existing('closed', '/it', 'withdrawn'),
    ]);
    expect(p.open.map((r) => r.route)).toEqual(['/it']);
    expect(p.skipped).toEqual([
      { route: '/ux/support', why: 'a page-audit is open' },
      { route: '/ux/sales', why: 'already audited' },
    ]);
  });

  it('opens everything on an empty system of record, in order', () => {
    const p = plan(routes, []);
    expect(p.open.map((r) => r.route)).toEqual(['/ux/support', '/ux/sales', '/it']);
    expect(p.skipped).toEqual([]);
  });

  it('ignores a packet whose metadata names no route', () => {
    const p = plan(routes, [{ status: 'open', metadata: {} }]);
    expect(p.open).toHaveLength(3);
  });

  it('reads every page of existing packets, not the first', async () => {
    const all = [
      existing('open', '/ux/support'),
      existing('open', '/ux/sales'),
      existing('closed', '/it', 'audited'),
    ];
    const asked: string[] = [];
    const api: Api = {
      get: async (path) => {
        asked.push(path);
        const offset = Number(new URL(`http://x${path}`).searchParams.get('offset'));
        return { data: all.slice(offset, offset + 2), total: all.length };
      },
      post: async () => {
        throw new Error('not a write');
      },
    };
    const rows = await existingAudits(api, 2);
    expect(rows).toEqual(all);
    expect(asked).toEqual([
      '/api/jobs?kind=page-audit&limit=2&offset=0',
      '/api/jobs?kind=page-audit&limit=2&offset=2',
    ]);
  });
});

describe('one route by name', () => {
  it('answers a catalogued path, and a department jobs view by its code', () => {
    expect(routeByName('/ux/support')).toEqual({
      route: '/ux/support',
      department: 'support',
      label: 'Support',
    });
    expect(routeByName('/ux/departments/sales')).toEqual({
      route: '/ux/departments/sales',
      department: 'sales',
      label: 'sales jobs view',
    });
    expect(routeByName('/ux/departments/')).toBeUndefined();
    expect(routeByName('/ux/nowhere')).toBeUndefined();
  });
});

describe('the packet and the door', () => {
  it('the body carries the route as Subject and as metadata beside its department, owned by the actor', () => {
    const body = packetFor({ route: '/ux/support', department: 'support', label: 'Support' }, 'emp-david');
    expect(body).toEqual({
      kind: 'page-audit',
      title: 'Page audit: /ux/support — Support (support)',
      tags: [],
      subject: { id: '/ux/support', subject_kind: 'custom' },
      owner_id: 'emp-david',
      status: 'open',
      priority: 'standard',
      metadata: { route: '/ux/support', department: 'support' },
    });
  });

  it('opens in order, reads each packet back, and refuses to call a create with no id opened', async () => {
    const posted: string[] = [];
    const api: Api = {
      get: async (path) => ({ id: path.replace('/api/jobs/', '') }),
      post: async (_path, body) => {
        posted.push((body as { metadata: { route: string } }).metadata.route);
        return { id: `id-${posted.length}` };
      },
    };
    const lines: string[] = [];
    const ids = await openAll(
      api,
      [
        { route: '/ux/support', department: 'support', label: 'Support' },
        { route: '/it', department: 'it', label: 'Train Yard' },
      ],
      'emp-david',
      (l) => lines.push(l),
    );
    expect(ids).toEqual(['id-1', 'id-2']);
    expect(posted).toEqual(['/ux/support', '/it']);
    expect(lines[0]).toContain('/ux/support');

    const hollow: Api = { get: api.get, post: async () => ({}) };
    await expect(
      openAll(hollow, [{ route: '/it', department: 'it', label: 'Train Yard' }], 'emp-david', () => {}),
    ).rejects.toThrow('returned no id');
  });

  it('names the actor by the door\'s rule: BOSS_ACTOR, else the actor file, and blank is not an answer', () => {
    expect(actorId({ BOSS_ACTOR: ' emp-david ' })).toBe('emp-david');
    expect(actorId({ BOSS_ACTOR: '  ', HOME: '/h' }, (p) => (p === '/h/.config/boss/actor' ? 'agent:x\n' : undefined))).toBe('agent:x');
    expect(actorId({ HOME: '/h' }, () => undefined)).toBeUndefined();
    expect(actorId({ HOME: '/h' }, () => '\n')).toBeUndefined();
    expect(actorId({ BOSS_ACTOR_FILE: '/f' }, (p) => (p === '/f' ? 'emp-a' : undefined))).toBe('emp-a');
  });
});
