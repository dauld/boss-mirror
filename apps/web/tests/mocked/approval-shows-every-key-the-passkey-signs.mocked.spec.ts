// Design f623e425 D3 (backlog 6c9183de, extends b and c, 2026-09-25):
// ApprovalSurface rendered `decision` and `comment` and nothing else, while
// the passkey it asked for binds step_shape_hash(title, metadata) — EVERY
// metadata key. So an ops-request's `plan`, `verb`, `host`, `args` and
// `rendered_plan_sha256`, or a key someone planted, was signed on one tap
// and never seen (review of car 66de0e4b reproduced it with PLAN wipe).
//
// This is the equality pin CLAUDE.md §9a asks of a fact that lives twice:
// the keys ON SCREEN at the instant the ceremony begins — read off the
// rendered page, not off the component's state — must be exactly the keys
// the begin asks the passkey to sign, value for value, with a planted key
// among them.

import { expect, test, type Page, type Route } from '@playwright/test';
import { scrollNote, signedText } from '../../src/steps/presence';
import { servePeopleRows } from './_smokeMocks';

const JOB_ID = 'job-askp-1';
const TICKET = 'ticket-askp';

const EMP = { id: 'emp-001', name: 'David', email: 'd@a', role: 'platform-admin',
  department: 'it', hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [] };

const json = (r: Route, b: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

const PRESENCE_REFUSAL = {
  error: 'step requires stronger assurance than this request carries',
  required: 'presence',
  produced: 'session',
};

// The ops-request approve step as the runner leaves it, plus one key the
// runner never writes — the planted one the review named.
const METADATA = {
  plan: 'PLAN wipe target-a on forge\n  /dev/sdb  by-id/ata-X  1.8T\nargv: wipe target-a\n',
  verb: 'wipe',
  host: 'forge',
  args: ['target-a'],
  rendered_plan_sha256: 'ab'.repeat(32),
  authority_role: 'platform-admin',
  planted: 'decision approved by someone else',
};

type OnScreen = { title: string; keys: string[]; values: string[] };
type Seen = { begins: { shown: { title: string; metadata: Record<string, unknown> } }[];
  onScreenAtBegin: OnScreen[]; writes: string[] };

// A value as the surface draws it: its bytes, quoted and escaped wherever
// a reader could misread them (backlog 6093cf13) — the app's own function,
// so this spec checks the page against the rendering, not a copy of it.
const text = signedText;

async function opsApproval(
  page: Page,
  overrides: { title?: string; metadata?: Record<string, unknown> } = {},
): Promise<Seen> {
  await page.addInitScript(() => {
    setInterval(() => document.querySelector('bun-hmr')?.remove(), 200);
    const buf = () => new Uint8Array([1, 2, 3]).buffer;
    Object.defineProperty(navigator, 'credentials', {
      configurable: true,
      value: {
        get: async () => ({
          id: 'cred-1', rawId: buf(), type: 'public-key',
          response: { authenticatorData: buf(), clientDataJSON: buf(), signature: buf(),
            userHandle: null },
        }),
      },
    });
  });
  const step = {
    id: 's1', job_id: JOB_ID, title: overrides.title ?? 'Approve the plan: wipe on forge',
    kind: 'sign-off',
    status: 'ready', assignee_id: null, sort_order: 0, blocked_by: [],
    sign_offs_required: ['platform-admin'], sign_offs: [] as unknown[],
    assurance_required: 'presence',
    metadata: { ...(overrides.metadata ?? METADATA) } as Record<string, unknown>, notes: null,
  };
  const job = {
    id: JOB_ID, kind: 'ops-request', title: 'wipe a disk on forge', status: 'open',
    opened_on: '2026-09-25', due_on: null, closed_on: null, owner_id: EMP.id,
    priority: 'standard', simulated: false, tags: [],
    subject: { subject_kind: 'custom', id: 'forge' }, metadata: {},
  };
  const seen: Seen = { begins: [], onScreenAtBegin: [], writes: [] };
  const surface = page.locator('.sg-detail');

  await page.route('**/api/**', (r) => json(r, []));
  await page.route(/\/api\/people$/, (r) => json(r, [EMP]));
  await servePeopleRows(page, [EMP]);
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: EMP.id, role: 'platform-admin' }));
  await page.route(/\/api\/jobs\/live$/, (r) =>
    json(r, { counts: {}, open_total: 0, recent: [], sim_clock: {} }));
  await page.route(/\/api\/jobs\/step-types$/, (r) => json(r, [
    { kind: 'sign-off', label: 'Sign-off', category: 'approval', ux: 'inline',
      description: '', surface: 'approval' },
  ]));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}$`), (r) => json(r, { ...job, steps: [step] }));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/s1/metadata$`), (r) => {
    seen.writes.push('PATCH metadata');
    for (const [k, v] of Object.entries(JSON.parse(r.request().postData() ?? '{}'))) {
      if (v === null) delete step.metadata[k];
      else step.metadata[k] = v;
    }
    return json(r, step);
  });
  await page.route(/\/api\/auth\/passkey\/assert\/begin$/, async (r) => {
    seen.begins.push(JSON.parse(r.request().postData() ?? '{}'));
    // What the approver has in front of them at the instant the passkey
    // is asked — read off the page.
    seen.onScreenAtBegin.push({
      title: (await surface.locator('h3').textContent()) ?? '',
      keys: await surface.locator('.step-signed-key').allTextContents(),
      values: await surface.locator('.step-signed-value').allTextContents(),
    });
    return json(r, {
      challenge_id: 'chal-1',
      publicKey: { challenge: 'AAAA', rpId: 'localhost',
        allowCredentials: [{ type: 'public-key', id: 'AAAA' }],
        userVerification: 'required', timeout: 60000 },
    });
  });
  await page.route(/\/api\/auth\/passkey\/assert\/finish$/, (r) => json(r, { ticket: TICKET }));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/s1/sign-offs$`), async (r) => {
    const ticket = await r.request().headerValue('x-presence-ticket');
    seen.writes.push(`POST sign-offs ${ticket ?? '-'}`);
    if (ticket !== TICKET) return json(r, PRESENCE_REFUSAL, 422);
    step.sign_offs = [{ role: 'platform-admin', authority_id: EMP.id, shape_hash: 'h',
      assurance: 'presence', presence_nonce: 'n' }];
    return json(r, step);
  });
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/s1$`), async (r) => {
    const ticket = await r.request().headerValue('x-presence-ticket');
    seen.writes.push(`PUT ${ticket ?? '-'}`);
    if (ticket !== TICKET) return json(r, PRESENCE_REFUSAL, 422);
    step.status = 'completed';
    return json(r, step);
  });
  return seen;
}

test('an open approve step shows every key its passkey would sign, planted ones included', async ({ page }) => {
  await opsApproval(page);
  await page.goto(`/ux/jobs/${JOB_ID}`);
  const surface = page.locator('.sg-detail');

  await expect(surface.locator('.step-signed-key')).toHaveText(Object.keys(METADATA).sort());
  for (const k of ['plan', 'verb', 'host', 'args', 'rendered_plan_sha256', 'planted']) {
    await expect(surface.locator('.step-signed-key', { hasText: k }).first()).toBeVisible();
  }
  // The plan is shown as its bytes: quoted, each newline drawn as \n and
  // then broken, so its trailing newline is visible too (6093cf13).
  const values = await surface.locator('.step-signed-value').allTextContents();
  expect(values[Object.keys(METADATA).sort().indexOf('plan')]).toBe(signedText(METADATA.plan));
  expect(signedText(METADATA.plan).endsWith('\\n\n"')).toBe(true);
});

test('Approve: the keys on screen when the passkey is asked are exactly the keys it signs', async ({ page }) => {
  const seen = await opsApproval(page);
  await page.goto(`/ux/jobs/${JOB_ID}`);
  const surface = page.locator('.sg-detail');
  await surface.getByRole('button', { name: 'Approve' }).click();
  await expect(surface.locator('.step-status')).toHaveText('completed');

  expect(seen.begins.length).toBe(1);
  const shown = seen.begins[0]!.shown;
  const onScreen = seen.onScreenAtBegin[0]!;
  const signedKeys = Object.keys(shown.metadata).sort();
  // Every signed key, the gesture's own decision and time among them, and
  // the planted key, is on screen — and nothing signed is missing.
  expect(onScreen.keys).toEqual(signedKeys);
  expect(onScreen.values).toEqual(signedKeys.map((k) => text(shown.metadata[k])));
  expect(onScreen.title).toBe(shown.title);
  for (const k of ['plan', 'verb', 'host', 'args', 'rendered_plan_sha256', 'planted', 'decision',
    'decided_at']) {
    expect(signedKeys).toContain(k);
  }
  expect(seen.writes).toEqual([
    'PATCH metadata', 'POST sign-offs -', `POST sign-offs ${TICKET}`, `PUT ${TICKET}`,
  ]);
});

// Backlog 6093cf13 (adversarial review of car 30674304): the check above
// compared bytes to the render's source, and the page drew a string byte
// for byte — so a bidi override reordered what the approver read, a
// Cyrillic "о" passed for a Latin one, '42' drew like 42, and a key name
// with a zero-width space drew like the plain one. Read off the page: what
// is drawn names every such byte.
test('a hostile step is drawn as its bytes: title, key names and values', async ({ page }) => {
  const title = ' Approve  the plan: wipe on fоrge ';
  const metadata = {
    ...METADATA,
    host: 'fоrge‮xcod.exe',
    'verb​': 'wipe',
    count: '42',
  };
  await opsApproval(page, { title, metadata });
  await page.goto(`/ux/jobs/${JOB_ID}`);
  const surface = page.locator('.sg-detail');
  const keys = Object.keys(metadata).sort();

  await expect(surface.locator('.step-signed-key')).toHaveText(keys.map(signedText));
  const values = await surface.locator('.step-signed-value').allTextContents();
  expect(values).toEqual(keys.map((k) => signedText(metadata[k as keyof typeof metadata])));
  expect(values[keys.indexOf('host')]).toBe('"f\\u{043E}rge\\u{202E}xcod.exe"');
  expect(values[keys.indexOf('count')]).toBe('"42"');
  // The title keeps both its edge spaces and its double space AS LAID OUT:
  // innerText follows the CSS white-space rule, where textContent and a
  // string toHaveText (which normalises whitespace) would not.
  expect(await surface.locator('.step-signed-title').innerText()).toBe(
    '" Approve  the plan: wipe on f\\u{043E}rge "',
  );
});

// Backlog 7c53b1bf (review of car fcda5f8b): the key column had no
// white-space rule, so two keys the passkey signs apart — 'plan b' and
// 'plan  b' — were laid out alike, and the toHaveText above could not
// see it, because a string toHaveText normalises whitespace. innerText
// follows the laid-out text, so it reads what the approver reads.
test('two key names that differ only in spaces are laid out apart', async ({ page }) => {
  const metadata = { ...METADATA, 'plan b': 'one', 'plan  b': 'two' };
  await opsApproval(page, { metadata });
  await page.goto(`/ux/jobs/${JOB_ID}`);
  const surface = page.locator('.sg-detail');
  const keys = Object.keys(metadata).sort();
  await expect(surface.locator('.step-signed-key')).toHaveCount(keys.length);
  const laidOut = await surface
    .locator('.step-signed-key')
    .evaluateAll((els) => els.map((e) => (e as HTMLElement).innerText));
  expect(laidOut).toEqual(keys.map(signedText));
  expect(laidOut).toContain('plan  b');
  // A value keeps its inner spaces the same way.
  const values = await surface
    .locator('.step-signed-value')
    .evaluateAll((els) => els.map((e) => (e as HTMLElement).innerText));
  expect(values[keys.indexOf('plan')]).toBe(signedText(METADATA.plan));
});

test('a value that scrolls in its box says so; one that fits does not', async ({ page }) => {
  const plan = Array.from({ length: 80 }, (_, i) => `  line ${i} of the plan`).join('\n');
  await opsApproval(page, { metadata: { ...METADATA, plan } });
  await page.goto(`/ux/jobs/${JOB_ID}`);
  const surface = page.locator('.sg-detail');
  await expect(surface.locator('.step-signed-overflow')).toHaveText([scrollNote(signedText(plan))]);
});
