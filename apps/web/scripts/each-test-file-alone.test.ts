// The pin on each-test-file-alone.ts (backlog cb211b39): that it lists
// the files `test:unit` means, and that a file which leans on another
// file's global FAILS it — the order dependence it exists to refuse.
import { afterAll, describe, expect, test } from 'bun:test';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { TEST_TIMEOUT_MS, runEachAlone, testFiles } from './each-test-file-alone';

// apps/web — this file is in scripts/.
const WEB_ROOT = new URL('..', import.meta.url).pathname;
const WEB_KIT_ROOT = new URL('../../../libs/web-kit/', import.meta.url).pathname;

/// The roots a package's `test:unit` hands this runner, read out of its
/// package.json — the command the gate's web checks run — so the pin
/// judges what runs, not a copy of it.
function unitRoots(pkgRoot: string): string[] {
  const pkg = JSON.parse(readFileSync(`${pkgRoot}/package.json`, 'utf8')) as {
    scripts: Record<string, string>;
  };
  const m = (pkg.scripts['test:unit'] ?? '').match(/^bun \S*scripts\/each-test-file-alone\.ts((?: [\w/.-]+)+)$/);
  expect(m, `${pkgRoot}package.json test:unit must run each-test-file-alone.ts`).not.toBeNull();
  return m![1]!.trim().split(' ');
}

describe('which files test:unit runs', () => {
  const files = testFiles(WEB_ROOT, unitRoots(WEB_ROOT));

  test('the two files of the incident, and this pin itself', () => {
    expect(files).toContain('./src/router.test.ts');
    expect(files).toContain('./src/shell/nav-catalog.test.ts');
    expect(files).toContain('./scripts/each-test-file-alone.test.ts');
  });

  test('not the mocked Playwright specs, which have a runner of their own', () => {
    expect(files.filter((f) => f.startsWith('./tests/'))).toEqual([]);
  });

  test('not a helper that merely sits beside a test', () => {
    expect(files).not.toContain('./scripts/each-test-file-alone.ts');
    expect(files).not.toContain('./src/router.ts');
  });

  test('web-kit runs its files alone too', () => {
    // The gate's `web-kit unit` check is libs/web-kit's test:unit, and a
    // global planted across ITS files hides the same way.
    const kit = testFiles(WEB_KIT_ROOT, unitRoots(WEB_KIT_ROOT));
    expect(kit.length).toBeGreaterThan(0);
    expect(kit.every((f) => f.startsWith('./src/'))).toBe(true);
  });
});

describe('what it refuses', () => {
  // Two files in a directory of this run's own: one plants a global,
  // the other passes only if that global is there. In ONE process that
  // pair passes or fails by the order the runner picks — the exact
  // shape of router.test.ts's window and nav-catalog.test.ts.
  const dir = mkdtempSync(join(tmpdir(), 'each-alone-'));
  mkdirSync(join(dir, 'src'));
  afterAll(() => rmSync(dir, { recursive: true, force: true }));
  writeFileSync(
    join(dir, 'src', 'plants.test.ts'),
    "import { test } from 'bun:test';\n" +
      "test('plants', () => { (globalThis as Record<string, unknown>).__plantedByAnother = 1; });\n",
  );
  writeFileSync(
    join(dir, 'src', 'leans.test.ts'),
    "import { expect, test } from 'bun:test';\n" +
      "test('leans', () => { expect((globalThis as Record<string, unknown>).__plantedByAnother).toBe(1); });\n",
  );

  test('a file that passes only after another file ran fails, and is named', async () => {
    const results = await runEachAlone(dir, testFiles(dir, ['src']), 2);
    const byFile = Object.fromEntries(results.map((r) => [r.file, r.code]));
    expect(Object.keys(byFile).sort()).toEqual(['./src/leans.test.ts', './src/plants.test.ts']);
    expect(byFile['./src/plants.test.ts']).toBe(0);
    expect(byFile['./src/leans.test.ts']).not.toBe(0);
    expect(results.find((r) => r.file === './src/leans.test.ts')?.output).toContain('(fail) leans');
  }, 60_000);
});

describe('the per-test budget it states', () => {
  // Backlog 75335234. bunfig.toml's `[test] timeout = 30000` never
  // reached bun: 1.3.14 loads that file's preload and ignores that key,
  // so every file ran at bun's 5 000 ms default, and dev-tree.test.ts
  // went red at 5.7 s on a gate pod shared with two other gates. The
  // runner now passes the budget on bun's own command line — so this
  // pin runs a file that sleeps PAST the 5 000 ms default and asks
  // whether it passed. A budget that does not reach bun fails it.
  const dir = mkdtempSync(join(tmpdir(), 'each-alone-budget-'));
  mkdirSync(join(dir, 'src'));
  afterAll(() => rmSync(dir, { recursive: true, force: true }));
  const BUN_DEFAULT_TIMEOUT_MS = 5_000;
  writeFileSync(
    join(dir, 'src', 'slow.test.ts'),
    "import { test } from 'bun:test';\n" +
      `test('outlasts the default', async () => { await Bun.sleep(${BUN_DEFAULT_TIMEOUT_MS + 250}); });\n`,
  );

  test('is above the bun default, or it states nothing', () => {
    expect(TEST_TIMEOUT_MS).toBeGreaterThan(BUN_DEFAULT_TIMEOUT_MS + 250);
  });

  test('reaches bun: a test slower than the 5 000 ms default passes', async () => {
    const [run] = await runEachAlone(dir, testFiles(dir, ['src']), 1);
    expect(run?.output).not.toContain('timed out');
    expect(run?.code).toBe(0);
  }, 60_000);
});
