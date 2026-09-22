// THE PIN ON THE MOCKED SUITE'S LOAD BUDGET (backlog e6bc776b).
//
// Until this file playwright.mocked.config.ts stated exactly one
// budget — `timeout: 30_000` — so every assertion, action and
// navigation that did not carry its own number inherited Playwright's
// defaults: 5 000 ms for `expect`, and "no cap, use the test timeout"
// for clicks and gotos. Nobody chose 5 000 ms for this pod, and it is
// the number three of the four load-flaky specs were failing at
// (false-empty declares no budget anywhere; guest-signin and
// incident-review declare one for the mount and then assert on the
// default).
//
// WHAT WAS MEASURED, 2026-09-22, on the dev pod (16-CPU cgroup quota,
// 32 host CPUs), running the four named specs unchanged:
//
//   quiet                      16/16, light tests 0.45-0.73 s
//   1.5x CPU oversubscription  16/16, light tests 1.7-2.7  s
//   4x                         16/16, light tests 3.7-5.6  s
//   10x                        16/16, light tests 4.8-7.8  s
//   full suite (129) at 2x     129/129, false-empty 5.9 s
//
// A ten-fold inflation of wall-clock with the app unchanged, against
// an unstated 5 000 ms. That is the whole mechanism: nothing about the
// page is different under load, only how long it takes to paint, and
// the budget it is judged against was never written down.
//
// SO THE CLAIM THIS PIN HOLDS is not any particular number — it is
// that the numbers are STATED. A budget the config declares is one an
// operator can read and change in one place; a budget inherited from a
// framework default is one that gets discovered a red at a time, which
// is how the per-spec constants grew (CLICK_TIMEOUT_MS 3 000 -> 15 000
// on 2026-09-18, after train #461; mountPage's 10 000; outage-crawl's
// 20 000 — five different numbers in five files, none of them readable
// as this suite's budget).
//
// It runs in `bun run test:unit`, beside the runner's leak pin, for
// the same reason that one lives here: it is the first web phase with
// node_modules.
import { expect, test } from 'bun:test';

import config from '../playwright.mocked.config';

// Playwright's own defaults, the ones an unstated budget falls back
// to. Spelled here so the assertion below can say WHICH number it is
// refusing, not just that something is missing.
const PLAYWRIGHT_DEFAULT_EXPECT_MS = 5_000;

test('the mocked suite states an expect budget instead of inheriting 5 000 ms', () => {
  const declared = config.expect?.timeout;
  expect(
    declared,
    'playwright.mocked.config.ts declares no expect.timeout, so every '
      + '`toBeVisible()` in the suite is judged against Playwright\'s '
      + `${PLAYWRIGHT_DEFAULT_EXPECT_MS} ms default — a number nobody chose `
      + 'for a pod that runs three gate bays and twelve builders',
  ).toBeNumber();
  expect(declared).toBeGreaterThan(PLAYWRIGHT_DEFAULT_EXPECT_MS);
});

test('the mocked suite states an action and a navigation budget', () => {
  expect(
    config.use?.actionTimeout,
    'an unstated actionTimeout means a click is bounded only by the test '
      + 'timeout, so a starved renderer reads as a dead control',
  ).toBeNumber();
  expect(
    config.use?.navigationTimeout,
    'an unstated navigationTimeout means a goto is bounded only by the '
      + 'test timeout, so a slow bundle consumes the whole test',
  ).toBeNumber();
});

test('the per-test budget leaves room for the assertions inside it', () => {
  // A test that spends its expect budget twice must still be allowed to
  // finish: a per-test cap below the sum of its own waits turns a slow
  // paint into a timeout with no assertion named.
  const perTest = config.timeout ?? 0;
  const perExpect = config.expect?.timeout ?? PLAYWRIGHT_DEFAULT_EXPECT_MS;
  expect(
    perTest,
    `timeout ${perTest} ms must exceed twice the expect budget `
      + `(${perExpect} ms) — otherwise the test dies before its second `
      + 'assertion can report what it was waiting for',
  ).toBeGreaterThan(perExpect * 2);
});
