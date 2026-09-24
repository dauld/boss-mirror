// A MOCKED FIXTURE READS THE CLOCK WHEN IT ANSWERS, NOT WHEN ITS FILE
// LOADS (backlog de205627).
//
// estate-page built its fixture stamps as module-level constants —
// `const CLOSED = { … closed_at: ago(9) … }` — so every stamp was taken
// the moment the worker loaded the file. The page rounds an age to the
// nearest minute, so "answered 9m ago" read "10m ago" once 30 s had
// passed between that load and the read: a worker that ran the file's
// tests for longer than that (gate load, or --repeat-each) failed on
// the clock, not the page. Measured 4 in 120 runs by the builder of
// a9c76cf7, who moved the stamps into the route handler. Nothing held
// the next fixture to that shape; this does.
//
// WHAT COUNTS AS LOAD TIME. A clock read (`Date.now()`, `new Date()`
// with no argument, `performance.now()`), or a call to a function in
// the same file that reads one, that runs before any test does: at the
// top level of the file, inside a `describe` body (Playwright runs it
// while collecting), or inside a `beforeAll` (once per worker, however
// long the worker then runs). Inside a test, a route handler, or a
// helper called from one, the read happens when it is asked for, which
// is the point. A FIXED date — `new Date('2026-09-23T15:00:00Z')` — is
// not a clock read and is left alone.
//
// It reads the TypeScript syntax tree rather than grepping, because the
// shape that flaked was a call inside an object literal at the top
// level, and only the tree can say which braces are a function.
import { expect, test } from 'bun:test';
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import ts from 'typescript';

const MOCKED_DIR = join(import.meta.dir, '../tests/mocked');
/// Callbacks Playwright runs before the tests they hold: a clock read
/// in one is as early as one at the top of the file.
const RUNS_BEFORE_TESTS = /(^|\.)(describe|beforeAll)(\.\w+)*$/;

function clockReadsAtLoad(file: string, source: string): ReadonlyArray<string> {
  const sf = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true);
  const isClockRead = (n: ts.Node): boolean =>
    (ts.isCallExpression(n) && ['Date.now', 'performance.now'].includes(n.expression.getText(sf)))
    || (ts.isNewExpression(n) && n.expression.getText(sf) === 'Date' && (n.arguments?.length ?? 0) === 0);

  // Named functions, and which of them read the clock — directly, or by
  // calling one that does (to a fixpoint, so a builder of builders is seen).
  const bodies = new Map<string, ts.Node>();
  const name = (n: ts.Node): void => {
    if (ts.isFunctionDeclaration(n) && n.name) bodies.set(n.name.text, n);
    if (ts.isVariableDeclaration(n) && ts.isIdentifier(n.name) && n.initializer
      && (ts.isArrowFunction(n.initializer) || ts.isFunctionExpression(n.initializer))) {
      bodies.set(n.name.text, n.initializer);
    }
    ts.forEachChild(n, name);
  };
  name(sf);
  const readers = new Set<string>();
  const callsReader = (n: ts.Node): boolean =>
    ts.isCallExpression(n) && ts.isIdentifier(n.expression) && readers.has(n.expression.text);
  const reads = (n: ts.Node): boolean => isClockRead(n) || callsReader(n) || !!ts.forEachChild(n, (c) => reads(c) || undefined);
  for (let grew = true; grew;) {
    grew = false;
    for (const [fn, body] of bodies) {
      if (!readers.has(fn) && reads(body)) {
        readers.add(fn);
        grew = true;
      }
    }
  }

  const runsBeforeTests = (fn: ts.Node): boolean =>
    ts.isCallExpression(fn.parent) && fn.parent.arguments.includes(fn as ts.Expression)
    && RUNS_BEFORE_TESTS.test(fn.parent.expression.getText(sf));
  const atLoad = (n: ts.Node): boolean => {
    for (let p = n.parent; p && p !== sf; p = p.parent) {
      if (ts.isFunctionLike(p) && !runsBeforeTests(p)) return false;
    }
    return true;
  };

  // The outermost early read is named, once: `packet(…, ago(7))` is one
  // finding, not two.
  const found: string[] = [];
  const visit = (n: ts.Node): void => {
    if ((isClockRead(n) || callsReader(n)) && atLoad(n)) {
      const line = sf.getLineAndCharacterOfPosition(n.getStart(sf)).line + 1;
      found.push(`${file}:${line} ${n.getText(sf).replace(/\s+/g, ' ').slice(0, 80)}`);
      return;
    }
    ts.forEachChild(n, visit);
  };
  visit(sf);
  return found;
}

test('no mocked spec takes a fixture clock before its tests run', () => {
  const early = readdirSync(MOCKED_DIR)
    .filter((f) => f.endsWith('.ts'))
    .flatMap((f) => clockReadsAtLoad(f, readFileSync(join(MOCKED_DIR, f), 'utf8')));
  expect(
    early,
    'a mocked spec reads the clock when its file loads, so every age built from it grows '
      + 'while the worker runs — move the read into the handler that answers, or build '
      + 'the fixture in a function the handler calls',
  ).toEqual([]);
});

test('the scanner sees the shape that flaked estate-page, and passes the shape that fixed it', () => {
  // The two halves of a9c76cf7's diff, cut down: the stamp helper is the
  // same; what differs is whether the table holding its calls is a
  // constant or a function.
  const ago = 'const ago = (m: number) => new Date(Date.now() - m * 60_000).toISOString();';
  const flaked = [ago, "const CLOSED = { forge: { closed_at: ago(9) } };"].join('\n');
  const fixed = [ago, "const closed = () => ({ forge: { closed_at: ago(9) } });"].join('\n');
  expect(clockReadsAtLoad('flaked.ts', flaked)).toEqual(['flaked.ts:2 ago(9)']);
  expect(clockReadsAtLoad('fixed.ts', fixed)).toEqual([]);

  // A describe body and a beforeAll run before the tests; a test body, a
  // route handler and a fixed date do not read the clock early.
  const early = [
    "test.describe('x', () => { const t = Date.now(); });",
    'test.beforeAll(() => { stamp = new Date(); });',
    "test('y', async () => { const t = Date.now(); });",
    "page.route('**', (r) => json(r, { at: ago(1) }));",
    "const NOW = new Date('2026-09-23T15:00:00Z');",
  ].join('\n');
  expect(clockReadsAtLoad('early.ts', [ago, early].join('\n'))).toEqual([
    'early.ts:2 Date.now()',
    'early.ts:3 new Date()',
  ]);
});
