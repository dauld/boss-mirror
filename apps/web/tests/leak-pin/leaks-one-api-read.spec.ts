// THE FIXTURE THE LEAK PIN DRIVES — deliberately floorless, and
// deliberately NOT under tests/mocked (backlog 2847f813).
//
// Every real mocked spec starts from installApiFloor(page); this one
// does the opposite on purpose, so the dev-server sees an /api read the
// in-browser mock did not answer. A spec of this shape inside
// tests/mocked would red the suite by design, which is why the runner's
// refusal was pinned only as pure functions plus a hand rehearsal the
// builder of 06038ed8 deliberately never committed. It lives in its own
// directory and the normal suite never collects it:
// playwright.mocked.config.ts reads testDir from BOSS_MOCKED_TEST_DIR,
// which only the pin sets (scripts/mocked-runner-refuses-a-leak.test.ts).
//
// PLAYWRIGHT MUST PASS HERE. The pin's claim is that the RUNNER refuses
// a run Playwright called green — so this spec asserts the local mock
// 404 rather than failing on it. An exit 1 that came from a failing spec
// would prove nothing about the refusal.
import { expect, test } from '@playwright/test';

// The path this run leaks. The pin greps the runner's output for it, so
// it is distinctive enough that no real route can supply it.
const LEAKED_PATH = '/api/leak-pin/2847f813';

// page.request, not page.goto + fetch, on purpose: an APIRequestContext
// call does not pass through page.route at all. Measured while running
// this pin's own control (2026-09-22) — adding a catch-all
// page.route('**/api/**') here changed nothing, the read still reached
// the dev-server. So the leak this fixture produces cannot be
// accidentally mocked away by a later edit, which is what a fixture
// whose whole job is to leak wants.

test('leaks one unmocked /api read and is otherwise green', async ({ page }) => {
  const res = await page.request.get(LEAKED_PATH);
  // src/dev-mocked.ts apiHandler: in mocked mode a miss is answered
  // locally, 404, with a body that says what it is — and counted for the
  // shutdown summary the runner reads off stdout.
  expect(res.status()).toBe(404);
  expect(await res.json()).toEqual({ mock: 'unanswered', path: LEAKED_PATH });
});
