// EVERY UNIT TEST FILE PASSES ALONE (backlog cb211b39).
//
// `bun test src/ scripts/` runs every file in ONE process, so a global
// one file plants is still there for every file after it. On 2026-09-25
// nav-catalog.test.ts passed that way on the cluster gate and failed
// alone: parseRoute read `window.location.search`, and router.test.ts
// had planted a window and never taken it down. GitHub's runner ordered
// the files differently and PR #244's gate went red at
// nav-catalog.test.ts:534, on an assertion that had been green on every
// cluster gate since it landed. A verdict that depends on the order the
// runner picked is not a verdict about the file.
//
// So `test:unit` runs this — apps/web's and libs/web-kit's both, each
// naming its own directories:
//
//   bun scripts/each-test-file-alone.ts src scripts            (apps/web)
//   bun ../../apps/web/scripts/each-test-file-alone.ts src     (libs/web-kit)
//
// Each file runs in a process of its own, nothing carried between them.
// A file that leans on another file's global fails here on every run,
// on every host, naming itself — not only on the host whose ordering
// happens to expose it. It REPLACES the shared run rather than adding
// to it: every file still runs, and each verdict is the file's own.
//
// Cost, measured on the dev pod 2026-09-25: apps/web's 122 files take
// 3.9s wall 4 at a time (7.7s one at a time) against 6.1s for the
// shared run — one file, the mocked runner's leak pin, is 4.2s of it —
// and web-kit's 14 take 0.1s. The runner prints its own time on every
// run. JOBS is bounded, not one per CPU, because the gate pod requests
// 4 CPUs and several gates share a node.
import { availableParallelism } from 'node:os';

/// Bun's own test-file names (`bun test` collects `*.test.*`,
/// `*_test.*`, `*.spec.*` and `*_spec.*`), so this runs the files that
/// `bun test <roots>` would.
export const TEST_FILE_GLOB = '**/*{.test,_test,.spec,_spec}.{ts,tsx,js,jsx,mts,cts,mjs,cjs}';

export const JOBS = Math.max(1, Math.min(4, availableParallelism()));

/// The per-test budget every file runs under, passed to bun on its own
/// command line (backlog 75335234). A per-test timeout exists to catch
/// a HUNG test, and a gate pod is not a quiet machine: gates run in
/// parallel since 2026-09-05, several to a node, and a starved pod
/// stretches every test's wall clock with the code unchanged.
///
/// Measured stalls on the gate, each red against bun's 5 000 ms
/// default on a test that is milliseconds when quiet:
///   2026-09-08  a synchronous 1 ms test took 8 167 ms while a second
///               gate compiled on the node (gate-run 6de49582, 8cbe1b7d)
///   2026-09-25  dev-tree.test.ts took 5 720 ms with three gates on the
///               node (gate-run 2c7c7ac1; its re-gate 9c388d9d green)
/// and on the dev pod, twelve copies of dev-tree.test.ts plus 200
/// busy loops pinned to one CPU: its slowest test 6.4 s, three of
/// twelve copies red at 5 000 ms. 30 s is three and a half times the
/// worst stall a gate has recorded and still reads a hang as a hang.
///
/// It lived in bunfig.toml as `[test] timeout = 30000` from 2026-09-08,
/// and bun 1.3.14 IGNORES that key — it loads the same file's preload,
/// so the file is read, and a 6.5 s test still timed out at 5 000 ms
/// (measured 2026-09-25). For seventeen days the budget was a comment.
/// `--timeout` on the command line is the spelling bun honours, and
/// each-test-file-alone.test.ts runs a test past the default to prove
/// this one reaches it.
export const TEST_TIMEOUT_MS = 30_000;

export type FileRun = Readonly<{ file: string; code: number; ms: number; output: string }>;

/// Every test file under `roots` (directories relative to `cwd`), as a
/// `./`-prefixed path bun reads as a FILE rather than as a name filter,
/// sorted so two runs list the same order. A root that does not exist
/// throws: a renamed directory must not quietly shrink the run.
export function testFiles(cwd: string, roots: ReadonlyArray<string>): string[] {
  const glob = new Bun.Glob(TEST_FILE_GLOB);
  return roots
    .flatMap((root) =>
      [...glob.scanSync({ cwd: `${cwd}/${root}`, onlyFiles: true })]
        .filter((f) => !f.split('/').includes('node_modules'))
        .map((f) => `./${root}/${f}`),
    )
    .sort();
}

async function runOne(cwd: string, file: string): Promise<FileRun> {
  const started = performance.now();
  const proc = Bun.spawn([process.execPath, 'test', '--timeout', String(TEST_TIMEOUT_MS), file], {
    cwd,
    stdout: 'pipe',
    stderr: 'pipe',
    env: process.env,
  });
  const [out, err, code] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ]);
  return { file, code, ms: Math.round(performance.now() - started), output: out + err };
}

/// Run each file in its own `bun test` process, `jobs` at a time.
/// Results come back in `files` order whatever order they finish in.
export async function runEachAlone(
  cwd: string,
  files: ReadonlyArray<string>,
  jobs: number = JOBS,
): Promise<FileRun[]> {
  const results: FileRun[] = new Array(files.length);
  let next = 0;
  const worker = async () => {
    for (let i = next++; i < files.length; i = next++) {
      results[i] = await runOne(cwd, files[i]!);
    }
  };
  await Promise.all(Array.from({ length: Math.min(jobs, files.length) }, worker));
  return results;
}

async function main(roots: ReadonlyArray<string>): Promise<number> {
  const cwd = process.cwd();
  if (roots.length === 0) {
    console.error('usage: bun each-test-file-alone.ts <dir>... (run from the package root)');
    return 2;
  }
  const files = testFiles(cwd, roots);
  // An empty list passes vacuously — a moved directory or a glob that
  // stopped matching would turn this whole check off in silence.
  if (files.length === 0) {
    console.error(`each-test-file-alone: found no test files under ${roots.join(', ')} in ${cwd}`);
    return 1;
  }
  const started = performance.now();
  const results = await runEachAlone(cwd, files);
  const secs = ((performance.now() - started) / 1000).toFixed(1);
  const failed = results.filter((r) => r.code !== 0);
  // A failing file's WHOLE output, not a tail: the reason is usually
  // near the top (an import that threw), and this is the only copy.
  for (const r of failed) {
    console.error(`\n===== ${r.file} — exit ${r.code}, run alone =====\n${r.output}`);
  }
  const slowest = [...results].sort((a, b) => b.ms - a.ms).slice(0, 3);
  // Bun's own per-file "N pass" line, summed, so a green run still says
  // how many tests it ran — the one number the shared run printed.
  const passed = results.reduce((n, r) => n + Number(r.output.match(/^\s*(\d+) pass$/m)?.[1] ?? 0), 0);
  console.log(
    `each-test-file-alone: ${results.length - failed.length}/${results.length} files passed ` +
      `(${passed} tests), each in its own process, ${JOBS} at a time, in ${secs}s ` +
      `(slowest: ${slowest.map((r) => `${r.file} ${(r.ms / 1000).toFixed(1)}s`).join(', ')})`,
  );
  if (failed.length > 0) {
    console.error(
      `each-test-file-alone: ${failed.length} file(s) FAIL when run alone: ` +
        `${failed.map((r) => r.file).join(', ')}\n` +
        `A file that passes in the shared run but not alone depends on a global another ` +
        `file left behind — make it set up (and take down) everything it reads.`,
    );
    return 1;
  }
  return 0;
}

if (import.meta.main) process.exit(await main(process.argv.slice(2)));
