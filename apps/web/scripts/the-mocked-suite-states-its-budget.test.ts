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
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import ts from 'typescript';

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

test('the shared mount waits under the stated budget, not a number of its own', () => {
  // Every mocked spec mounts through mountPage, so a cap it writes for
  // itself overrides the stated budget for the whole suite. It wrote
  // 10 000 ms for the shell and the h1 while the config stated 15 000,
  // and on 2026-09-24 the workflow authoring workspace — whose h1 paints
  // only once its first read answers — missed that cap under gate load
  // on a car that did not touch it (gate 2ab44d1d, backlog e614c5de).
  // Forced there by holding the design Job's read back 11 s: red under
  // the 10 000 ms cap with the gate's exact "element(s) not found", green
  // under the stated budget. Holding a read back 11 s on every run is too
  // dear to keep, so this holds the cause instead.
  const helpers = readFileSync(join(import.meta.dir, '../tests/mocked/_helpers.ts'), 'utf8');
  const at = helpers.indexOf('export async function mountPage');
  expect(at, 'tests/mocked/_helpers.ts has no mountPage to hold').toBeGreaterThanOrEqual(0);
  // To the function's closing brace, which is the first one in column 0.
  const end = helpers.indexOf('\n}\n', at);
  const mount = helpers.slice(at, end < 0 ? undefined : end);
  const caps = mount.match(/timeout:\s*[\d_]+/g) ?? [];
  expect(
    caps,
    'mountPage carries a timeout of its own — it runs under '
      + `playwright.mocked.config.ts's stated expect budget (${config.expect?.timeout} ms), `
      + 'and a tighter cap here is a tighter cap for every spec',
  ).toEqual([]);
});

// NO SPEC WRITES A WAIT SHORTER THAN THE ONE ITS CALL WOULD INHERIT
// (backlog de205627).
//
// The pin above holds the one helper every spec mounts through; the
// same shape sat in the specs themselves. The builder of e614c5de
// found nine more after fixing job-kind-authoring — a 10 000 ms
// `toHaveText` on the design exhibit, a 10 000 ms `expect.poll` on the
// review stream, 10 000 ms `waitForURL`s after a sign-in, a 401 bounce
// and a "+ New rule" click, 12 000 ms polls on the audit tail, and the
// interaction crawl's 10 000 ms best-effort waits — each one a tighter
// cap than the suite states, written by hand, and each the exact shape
// of the flake that had just redded a gate. A number of one's own that
// is LOWER than the stated budget is a bet that this page, alone in
// the suite, paints faster under load; the load table above says no
// page does.
//
// So every `timeout:` written under tests/mocked is read, its call
// named, and its value held to the budget that call would inherit:
// `navigationTimeout` for a goto and the waits that are navigations,
// `expect.timeout` for an assertion or a poll, `actionTimeout` for
// everything else. A longer number is allowed — a crawl that retries
// its goto may state its own ceiling. A value the pin cannot read (an
// expression, an unresolved name) is refused rather than guessed.
//
// ONE WAY OUT, NAMED. A wait whose TIMING OUT is the expected answer —
// the crawl's 250 ms race for a navigation a click may not cause — is
// not a budget at all, and raising it would only make every click
// slower. Such a line says so, with its reason, on the line itself or
// the line above: `short on purpose: <why>`. The pin reads the words,
// so the exception is written where the next reader of the wait is.
const MOCKED_DIR = join(import.meta.dir, '../tests/mocked');
const NAVIGATIONS = new Set([
  'goto', 'goBack', 'goForward', 'reload', 'waitForURL', 'waitForLoadState', 'waitForNavigation',
]);
const SHORT_ON_PURPOSE = /short on purpose:\s*\S/;

type Wait = Readonly<{ file: string; line: number; call: string; value: number | null; budget: number }>;

function inheritedBudget(call: string): number {
  if (NAVIGATIONS.has(call)) return config.use?.navigationTimeout ?? 0;
  if (call === 'poll' || /^to[A-Z]/.test(call)) return config.expect?.timeout ?? PLAYWRIGHT_DEFAULT_EXPECT_MS;
  return config.use?.actionTimeout ?? 0;
}

/// Every hand-written `timeout:` in one source file, with the call it
/// is passed to and each value it can take (both arms of a ternary).
function handWrittenWaits(file: string, source: string): ReadonlyArray<Wait> {
  const sf = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true);
  const lines = source.split('\n');
  const consts = new Map<string, ts.Expression>();
  const collect = (n: ts.Node): void => {
    if (ts.isVariableDeclaration(n) && ts.isIdentifier(n.name) && n.initializer) consts.set(n.name.text, n.initializer);
    ts.forEachChild(n, collect);
  };
  collect(sf);
  const values = (e: ts.Expression, seen: number): Array<number | null> => {
    if (ts.isNumericLiteral(e)) return [Number(e.text.replace(/_/g, ''))];
    if (ts.isParenthesizedExpression(e)) return values(e.expression, seen);
    if (ts.isConditionalExpression(e)) return [...values(e.whenTrue, seen), ...values(e.whenFalse, seen)];
    const bound = ts.isIdentifier(e) ? consts.get(e.text) : undefined;
    return bound && seen < 8 ? values(bound, seen + 1) : [null];
  };
  const found: Wait[] = [];
  const visit = (n: ts.Node): void => {
    if (ts.isPropertyAssignment(n) && n.name.getText(sf) === 'timeout') {
      const line = sf.getLineAndCharacterOfPosition(n.getStart(sf)).line;
      const exempt = SHORT_ON_PURPOSE.test(lines[line] ?? '') || SHORT_ON_PURPOSE.test(lines[line - 1] ?? '');
      const holder = n.parent.parent;
      const callee = holder && ts.isCallExpression(holder) ? holder.expression : undefined;
      const call = callee && ts.isPropertyAccessExpression(callee) ? callee.name.text : callee?.getText(sf) ?? '(an options object)';
      if (!exempt) {
        for (const value of values(n.initializer, 0)) {
          found.push({ file, line: line + 1, call, value, budget: inheritedBudget(call) });
        }
      }
    }
    ts.forEachChild(n, visit);
  };
  visit(sf);
  return found;
}

test('no mocked spec hand-writes a wait shorter than the budget its call inherits', () => {
  const short = readdirSync(MOCKED_DIR)
    .filter((f) => f.endsWith('.ts'))
    .flatMap((f) => handWrittenWaits(f, readFileSync(join(MOCKED_DIR, f), 'utf8')))
    .filter((w) => w.value === null || w.value < w.budget)
    .map((w) => `${w.file}:${w.line} ${w.call} ${w.value ?? '(unreadable)'} ms < ${w.budget} ms`);
  expect(
    short,
    'a mocked spec caps a wait below the budget playwright.mocked.config.ts states for it. '
      + 'Drop the number and inherit the budget; if timing out IS the answer the wait '
      + 'looks for, say so on the line: `short on purpose: <why>`',
  ).toEqual([]);
});

// NO SPEC SLEEPS WITHOUT SAYING WHY THE SLEEP IS THE ANSWER (backlog
// 840c5a76).
//
// A `timeout:` is a ceiling on a wait for something; `waitForTimeout` is
// a wait for NOTHING, and every one the builder of de205627 found was a
// bet in the same shape — 1 600 ms that a slow answer had landed, 2 000
// ms that a plugin had mounted, 6 000 ms that a 5 000 ms timer would
// have fired, 300 and 500 ms that a request the page never sent would
// have arrived by now. Under load each loses: the thing it stood for
// happens later than the number, and the assertion after it reads a page
// mid-change. The interaction crawl lost the same bet on a request EVENT
// (a Reset click judged silent with its POST on the wire). Each is now a
// wait on the event it stood for: a locator, the page's own record of
// the requests it opened (tests/mocked/_helpers.ts), the page's clock
// run forward, an answer the page has read, a frame.
//
// What is left is a window whose ELAPSING is the answer — a runaway read
// loop only shows itself by counting over time, a quiescence loop needs
// a pace. Such a line says so, the same way a short `timeout:` does:
// `short on purpose: <why>` on the line or the line above. Every other
// `waitForTimeout` under tests/mocked is refused, named by file and line.
function bareSleeps(file: string, source: string): ReadonlyArray<string> {
  const sf = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true);
  const lines = source.split('\n');
  const found: string[] = [];
  const visit = (n: ts.Node): void => {
    if (
      ts.isCallExpression(n)
      && ts.isPropertyAccessExpression(n.expression)
      && n.expression.name.text === 'waitForTimeout'
    ) {
      const line = sf.getLineAndCharacterOfPosition(n.getStart(sf)).line;
      const said = SHORT_ON_PURPOSE.test(lines[line] ?? '') || SHORT_ON_PURPOSE.test(lines[line - 1] ?? '');
      if (!said) found.push(`${file}:${line + 1} ${n.getText(sf)}`);
    }
    ts.forEachChild(n, visit);
  };
  visit(sf);
  return found;
}

test('no mocked spec sleeps unless the sleep is the answer, and says so', () => {
  const sleeps = readdirSync(MOCKED_DIR)
    .filter((f) => f.endsWith('.ts'))
    .flatMap((f) => bareSleeps(f, readFileSync(join(MOCKED_DIR, f), 'utf8')));
  expect(
    sleeps,
    'a mocked spec sleeps for a fixed time. Wait for the event the sleep stands for — '
      + 'a locator, the page\'s own request record (recordPageRequests / openedRequests / '
      + 'readsSettled / answerRead in tests/mocked/_helpers.ts), page.clock.runFor for a '
      + 'timer, nextFrame for a paint. If the window elapsing IS the answer, say so on the '
      + 'line: `short on purpose: <why>`',
  ).toEqual([]);
});

test('the sleep floor reads what a spec actually writes', () => {
  // A scanner that reads nothing passes the test above on any tree. The
  // shapes: a bare sleep, a named one, the exception on the line above
  // and on the line itself, a sleep inside a comment (not a call), and a
  // setTimeout inside a route handler — a held answer, which is the
  // fixture's own event and not a sleep in the spec.
  const src = [
    'await page.waitForTimeout(1600);',
    'await page.waitForTimeout(SETTLE_MS);',
    '// short on purpose: a loop only shows itself over a window',
    'await page.waitForTimeout(1_000);',
    'await page.waitForTimeout(25); // short on purpose: the poll interval',
    '// it used to be `await page.waitForTimeout(500)`',
    'await page.route(x, async (r) => { await new Promise((s) => setTimeout(s, 3_000)); });',
  ].join('\n');
  expect(bareSleeps('fixture.ts', src)).toEqual([
    'fixture.ts:1 page.waitForTimeout(1600)',
    'fixture.ts:2 page.waitForTimeout(SETTLE_MS)',
  ]);
});

test('the floor reads what a spec actually writes', () => {
  // The scanner itself, on the shapes it must see: a literal, a named
  // constant, both arms of a ternary, a navigation's larger budget, and
  // the one written exception. A scanner that reads nothing passes the
  // test above on any tree.
  const src = [
    'const SETTLE = 250;',
    "await expect(a).toHaveText('x', { timeout: 10_000 });",
    'await page.waitForURL(/x/, { timeout: 20_000 });',
    'await t.click({ timeout: tag ? CLICK : 3_000 });',
    '// short on purpose: a race the click may lose',
    'await page.waitForURL(f, { timeout: SETTLE });',
    'await expect.poll(() => n, { timeout: 12_000 }).toBe(1);',
  ].join('\n');
  const got = handWrittenWaits('fixture.ts', src).map((w) => [w.line, w.call, w.value, w.budget]);
  const nav = inheritedBudget('goto');
  const exp = inheritedBudget('toHaveText');
  const act = inheritedBudget('click');
  expect([nav, exp, act], 'the config states each of the three budgets').toEqual([
    config.use?.navigationTimeout as number,
    config.expect?.timeout as number,
    config.use?.actionTimeout as number,
  ]);
  expect(got).toEqual([
    [2, 'toHaveText', 10_000, exp],
    [3, 'waitForURL', 20_000, nav],
    [4, 'click', null, act],
    [4, 'click', 3_000, act],
    [7, 'poll', 12_000, exp],
  ]);
});
