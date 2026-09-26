// Backlog d82b5f60 (review of car 66de0e4b, 2026-09-25): ApprovalSurface's
// decide() read `step.id`, `step.title` and `step.metadata` AFTER its
// awaits. The surface instance is reused when the rail switches steps, so
// a click on another step while the first write was in flight re-aimed
// every later write of the gesture — the stamp, the ceremony's `shown`,
// and the completion PUT — at the step now on screen. A presence step
// would refuse that PUT 412/422; an ordinary one was COMPLETED by a
// request its approver never aimed at it.
//
// The mocks below hold the gesture's first write (the decision's metadata
// merge on s1) until the test has switched the surface to s2, then let it
// go. Every write the gesture makes afterwards must still name s1, and s2
// must receive none.

import { expect, test, type Page, type Route } from '@playwright/test';
import { answerRead, recordPageRequests } from './_helpers';

const JOB_ID = 'job-apsw-1';

const EMP = { id: 'emp-001', name: 'David', email: 'd@a', role: 'platform-admin',
  department: 'it', hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [] };

const json = (r: Route, b: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

type Write = { method: string; step: string; path: string; ticket?: string };
type Seen = { writes: Write[]; begins: unknown[]; finishes: number; release: () => void };

/** Where the gesture is held until the test lets it go: its first write
 *  (the decision's metadata merge), or the passkey begin (7c53b1bf). With
 *  `promptWaits`, the passkey prompt stays up until its signal aborts it,
 *  the way a browser's does. */
type Hold = { at?: 'metadata' | 'begin'; promptWaits?: boolean };

/** How many times the page asked the passkey, and whether a prompt was aborted. */
const passkey = (page: Page) =>
  page.evaluate(() => (window as unknown as { __passkey: { asked: number; aborted: boolean } }).__passkey);

async function twoApprovals(page: Page, presence: boolean, hold: Hold = {}): Promise<Seen> {
  const holdAt = hold.at ?? 'metadata';
  await recordPageRequests(page);
  await page.addInitScript((promptWaits: boolean) => {
    setInterval(() => document.querySelector('bun-hmr')?.remove(), 200);
    const buf = () => new Uint8Array([1, 2, 3]).buffer;
    const record = { asked: 0, aborted: false };
    (window as unknown as { __passkey: typeof record }).__passkey = record;
    Object.defineProperty(navigator, 'credentials', {
      configurable: true,
      value: {
        get: (opts?: { signal?: AbortSignal }) => {
          record.asked += 1;
          if (promptWaits) {
            return new Promise((_, reject) => {
              opts?.signal?.addEventListener('abort', () => {
                record.aborted = true;
                reject(new DOMException('The operation was aborted.', 'AbortError'));
              });
            });
          }
          return Promise.resolve({
            id: 'cred-1', rawId: buf(), type: 'public-key',
            response: { authenticatorData: buf(), clientDataJSON: buf(), signature: buf(),
              userHandle: null },
          });
        },
      },
    });
  }, hold.promptWaits ?? false);
  const mk = (id: string, title: string, order: number, plan: string) => ({
    id, job_id: JOB_ID, title, kind: 'sign-off', status: 'ready', assignee_id: null,
    sort_order: order, blocked_by: [],
    sign_offs_required: presence ? ['platform-admin'] : [], sign_offs: [] as unknown[],
    ...(presence ? { assurance_required: 'presence' } : {}),
    metadata: { plan } as Record<string, unknown>, notes: null,
  });
  const steps = [
    mk('s1', 'Approve the budget', 0, 'PLAN budget'),
    mk('s2', 'Approve the hire', 1, 'PLAN hire'),
  ];
  const job = {
    id: JOB_ID, kind: 'ops-request', title: 'two approvals', status: 'open',
    opened_on: '2026-09-25', due_on: null, closed_on: null, owner_id: EMP.id,
    priority: 'standard', simulated: false, tags: [],
    subject: { subject_kind: 'custom', id: 'forge' }, metadata: {},
  };
  let release: () => void = () => {};
  const held = new Promise<void>((r) => {
    release = r;
  });
  const seen: Seen = { writes: [], begins: [], finishes: 0, release: () => release() };

  await page.route('**/api/**', (r) => json(r, []));
  await page.route(/\/api\/people$/, (r) => json(r, [EMP]));
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: EMP.id, role: 'platform-admin' }));
  await page.route(/\/api\/jobs\/live$/, (r) =>
    json(r, { counts: {}, open_total: 0, recent: [], sim_clock: {} }));
  await page.route(/\/api\/jobs\/step-types$/, (r) => json(r, [
    { kind: 'sign-off', label: 'Sign-off', category: 'approval', ux: 'inline',
      description: '', surface: 'approval' },
  ]));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}$`), (r) => json(r, { ...job, steps }));
  await page.route(/\/api\/auth\/passkey\/assert\/begin$/, async (r) => {
    seen.begins.push(JSON.parse(r.request().postData() ?? '{}'));
    if (holdAt === 'begin') await held;
    return json(r, {
      challenge_id: 'chal-1',
      publicKey: { challenge: 'AAAA', rpId: 'localhost',
        allowCredentials: [{ type: 'public-key', id: 'AAAA' }],
        userVerification: 'required', timeout: 60000 },
    });
  });
  await page.route(/\/api\/auth\/passkey\/assert\/finish$/, (r) => {
    seen.finishes += 1;
    return json(r, { ticket: 'ticket-1' });
  });
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/(s1|s2)(/.*)?$`), async (r) => {
    const m = new RegExp(`/steps/(s1|s2)(/.*)?$`).exec(r.request().url());
    const id = m?.[1] ?? '?';
    const path = m?.[2] ?? '';
    const method = r.request().method();
    const ticket = (await r.request().headerValue('x-presence-ticket')) ?? undefined;
    seen.writes.push({ method, step: id, path, ticket });
    const step = steps.find((s) => s.id === id)!;
    // The gesture's first write is held until the test has switched steps.
    if (holdAt === 'metadata' && id === 's1' && path === '/metadata') await held;
    if (path === '/metadata') {
      Object.assign(step.metadata, JSON.parse(r.request().postData() ?? '{}'));
      return json(r, step);
    }
    if (path === '/sign-offs') {
      if (presence && !ticket) {
        return json(r, { error: 'presence', required: 'presence', produced: 'session' }, 422);
      }
      step.sign_offs = [{ role: 'platform-admin', authority_id: EMP.id, shape_hash: 'h' }];
      return json(r, step);
    }
    if (presence && !ticket) {
      return json(r, { error: 'presence', required: 'presence', produced: 'session' }, 422);
    }
    step.status = 'completed';
    return json(r, step);
  });
  return seen;
}

async function approveThenSwitch(page: Page, seen: Seen): Promise<void> {
  await page.goto(`/ux/jobs/${JOB_ID}`);
  const surface = page.locator('.sg-detail');
  await expect(surface.locator('h3')).toHaveText('Approve the budget');
  await surface.getByRole('button', { name: 'Approve' }).click();
  await expect.poll(() => seen.writes.length).toBe(1);
  // Mid-gesture: the approver opens the other step on the rail.
  await page.locator('.sg-rail .rail-row', { hasText: 'Approve the hire' }).click();
  await expect(surface.locator('h3')).toHaveText('Approve the hire');
  seen.release();
}

test('a step switched mid-gesture: the completion lands on the step clicked, never the one now shown', async ({ page }) => {
  const seen = await twoApprovals(page, false);
  await approveThenSwitch(page, seen);

  await expect
    .poll(() => seen.writes.filter((w) => w.method === 'PUT').length)
    .toBe(1);
  expect(seen.writes.filter((w) => w.step === 's2')).toEqual([]);
  expect(seen.writes.map((w) => `${w.method} ${w.step}${w.path}`)).toEqual([
    'PATCH s1/metadata',
    'PUT s1',
  ]);
});

// Design f623e425 D3 (backlog 6c9183de, 2026-09-25) tightened the presence
// half of this: the passkey signs only what is on screen. Once the rail
// shows s2, s1's plan is no longer in front of the approver, so the
// gesture that was aimed at s1 refuses its ceremony and signs nothing —
// where it used to sign s1 unseen. d82b5f60's claim stands unchanged:
// no write of the gesture reaches s2.
test('a presence step switched mid-gesture: nothing is signed, and nothing reaches the step now shown', async ({ page }) => {
  const seen = await twoApprovals(page, true);
  await approveThenSwitch(page, seen);

  const surface = page.locator('.sg-detail');
  await expect(surface.locator('.step-write-error')).toContainText('Nothing was signed');
  expect(seen.writes.filter((w) => w.step === 's2')).toEqual([]);
  // The decision landed on s1 (the write the test held); the stamp that
  // asked for presence was refused, and no ceremony, second stamp or
  // completion followed.
  expect(seen.writes.map((w) => `${w.method} ${w.step}${w.path}`)).toEqual([
    'PATCH s1/metadata',
    'POST s1/sign-offs',
  ]);
  expect(seen.begins).toEqual([]);
});

// Backlog 7c53b1bf (adversarial review of car fcda5f8b, 2026-09-25): the
// ceremony read what was on screen ONCE, before the begin. The switch
// above lands before that read, so it refused; a switch or a navigation
// DURING the begin round trip did not, and the passkey prompt came up
// over the step now shown to sign the one that was not. The ceremony now
// reads the screen again after the begin, after the prompt and after the
// finish; a surface that is destroyed answers that nothing of it is on
// screen; and a prompt still up when its step leaves is aborted.

const BEGIN = /\/api\/auth\/passkey\/assert\/begin$/;

async function approveHeldAtBegin(page: Page, seen: Seen): Promise<void> {
  await page.goto(`/ux/jobs/${JOB_ID}`);
  const surface = page.locator('.sg-detail');
  await expect(surface.locator('h3')).toHaveText('Approve the budget');
  await surface.getByRole('button', { name: 'Approve' }).click();
  await expect.poll(() => seen.begins.length).toBe(1);
}

test('control: a begin held and let go with the step still shown asks the passkey and completes', async ({ page }) => {
  const seen = await twoApprovals(page, true, { at: 'begin' });
  await approveHeldAtBegin(page, seen);
  seen.release();
  // The completion is the last write; the page then moves to the next step.
  await expect.poll(() => seen.writes.filter((w) => w.method === 'PUT').length).toBe(1);
  expect((await passkey(page)).asked).toBe(1);
  expect(seen.writes.map((w) => `${w.method} ${w.step}${w.path} ${w.ticket ?? '-'}`)).toEqual([
    'PATCH s1/metadata -', 'POST s1/sign-offs -', 'POST s1/sign-offs ticket-1', 'PUT s1 ticket-1',
  ]);
});

test('the rail moves on while the begin is in flight: the passkey is never asked', async ({ page }) => {
  const seen = await twoApprovals(page, true, { at: 'begin' });
  await approveHeldAtBegin(page, seen);
  const surface = page.locator('.sg-detail');
  await page.locator('.sg-rail .rail-row', { hasText: 'Approve the hire' }).click();
  await expect(surface.locator('h3')).toHaveText('Approve the hire');
  seen.release();

  await expect(surface.locator('.step-write-error')).toContainText('Nothing was signed');
  expect((await passkey(page)).asked).toBe(0);
  expect(seen.finishes).toBe(0);
  expect(seen.writes.map((w) => `${w.method} ${w.step}${w.path}`)).toEqual([
    'PATCH s1/metadata',
    'POST s1/sign-offs',
  ]);
});

test('the page moves on while the begin is in flight: the passkey is never asked', async ({ page }) => {
  const seen = await twoApprovals(page, true, { at: 'begin' });
  await approveHeldAtBegin(page, seen);
  // The app's own navigate(): pushState, then popstate — the surface is
  // destroyed while the gesture it started still awaits the begin.
  await page.evaluate(() => {
    window.history.pushState({}, '', '/ux/jobs');
    window.dispatchEvent(new PopStateEvent('popstate'));
  });
  await expect(page.locator('.sg-detail')).toHaveCount(0);
  seen.release();
  // The page has read the begin's answer and run what follows it — which
  // is where the prompt would be asked.
  await answerRead(page, BEGIN);

  expect((await passkey(page)).asked).toBe(0);
  expect(seen.finishes).toBe(0);
  expect(seen.writes.filter((w) => w.ticket)).toEqual([]);
});

test('the rail moves on while the passkey prompt is up: the prompt is aborted and nothing is sent', async ({ page }) => {
  const seen = await twoApprovals(page, true, { at: 'begin', promptWaits: true });
  seen.release();
  await page.goto(`/ux/jobs/${JOB_ID}`);
  const surface = page.locator('.sg-detail');
  await expect(surface.locator('h3')).toHaveText('Approve the budget');
  await surface.getByRole('button', { name: 'Approve' }).click();
  await expect.poll(async () => (await passkey(page)).asked).toBe(1);
  await page.locator('.sg-rail .rail-row', { hasText: 'Approve the hire' }).click();
  await expect(surface.locator('h3')).toHaveText('Approve the hire');

  await expect.poll(async () => (await passkey(page)).aborted).toBe(true);
  await expect(surface.locator('.step-write-error')).toContainText('Nothing was signed');
  expect(seen.finishes).toBe(0);
  expect(seen.writes.map((w) => `${w.method} ${w.step}${w.path}`)).toEqual([
    'PATCH s1/metadata',
    'POST s1/sign-offs',
  ]);
});
