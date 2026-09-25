// HOW MANY PLAYWRIGHT WORKERS THE MOCKED SUITE RUNS (backlog 55ca0748).
//
// Playwright's default is `workers: '50%'`, resolved as half of
// `os.cpus().length` — and inside a pod that reads the NODE, not the
// pod's cgroup. Every gate pod and the dev pod land on w-1, which has 32
// CPUs, so the suite ran 16 Chromium workers everywhere: measured
// 2026-09-25, `Running 719 tests using 16 workers` on the dev pod under
// a 16-CPU quota (node's os.cpus() 32; bun's availableParallelism() 16),
// and the gate pod's quota is 20 (infra/gate-runner/gate-runner.yaml,
// limits cpu "20", requests "4"). Sixteen renderers plus the one bun
// dev-server that serves them all took the node's load average from 8
// to 42 on their own, and in red gate 153d17e0 — a train gate sharing
// the node — every in-flight test stalled ~15 s at once (eight
// unrelated specs at 15.5-17.5 s against a ~2 s norm), one of them past
// its 15 s action budget.
//
// So the suite keeps Playwright's own policy — half the CPUs — and takes
// it of the CPUs the cgroup lets this process USE: the CFS quota in
// cgroup v2's `cpu.max`, bounded by the node's count. Where there is no
// quota (a laptop, `max`, an unreadable file) it is exactly Playwright's
// default. `--workers` on the command line still overrides the config.
//
// The unit runner does not need this: scripts/each-test-file-alone.ts
// caps itself at 4 and runs under bun, whose availableParallelism()
// already reads the quota.
//
// NODE-SAFE ON PURPOSE, like dev-tree.ts: playwright.mocked.config.ts
// imports it, and Playwright loads its config under node.

import { readFileSync } from 'node:fs';

/// cgroup v2's CPU limit for this process: `<quota> <period>` or
/// `max <period>`.
export const CPU_MAX_PATH = '/sys/fs/cgroup/cpu.max';

/// The CPUs a process may use: the cgroup quota (quota / period, rounded
/// down, at least one) bounded by the host's count — or the host's count
/// when there is no quota to read.
export function usableCpus(cpuMax: string | null, hostCpus: number): number {
  const [quota, period] = (cpuMax ?? '').trim().split(/\s+/).map(Number);
  if (quota === undefined || period === undefined) return hostCpus;
  if (!Number.isFinite(quota) || !Number.isFinite(period) || quota <= 0 || period <= 0) {
    return hostCpus;
  }
  return Math.min(hostCpus, Math.max(1, Math.floor(quota / period)));
}

/// Playwright's own default (half the CPUs, at least one), taken of the
/// CPUs this process may use rather than the CPUs the node has.
export function mockedWorkers(cpuMax: string | null, hostCpus: number): number {
  return Math.max(1, Math.floor(usableCpus(cpuMax, hostCpus) / 2));
}

/// The `cpu.max` line, or null where there is none to read (no cgroup
/// v2, not Linux).
export function readCpuMax(path: string = CPU_MAX_PATH): string | null {
  try {
    return readFileSync(path, 'utf8');
  } catch {
    return null;
  }
}
