// The mocked suite's worker count follows the CPUs the process may USE,
// not the CPUs the node HAS (backlog 55ca0748).
//
// Playwright's default is half of `os.cpus().length`, and in a pod that
// is the NODE's count: 32 on w-1, where every gate pod and the dev pod
// run. Measured 2026-09-25: `Running 719 tests using 16 workers` on the
// dev pod under a 16-CPU cgroup quota, and red gate 153d17e0 (quota 20)
// ran the same 16, then stalled ~15 s across every in-flight test while
// a train gate shared the node.

import { describe, expect, test } from 'bun:test';

import { mockedWorkers, usableCpus } from './dev-workers';

describe('usableCpus', () => {
  test('a cgroup v2 quota bounds the node count', () => {
    // The gate pod: limits cpu 20 on a 32-CPU node.
    expect(usableCpus('2000000 100000\n', 32)).toBe(20);
    // The dev pod: limits cpu 16.
    expect(usableCpus('1600000 100000\n', 32)).toBe(16);
  });

  test('a fractional quota rounds down, and never below one CPU', () => {
    expect(usableCpus('250000 100000', 32)).toBe(2);
    expect(usableCpus('50000 100000', 32)).toBe(1);
  });

  test('a quota above the node count is bounded by the node', () => {
    expect(usableCpus('6400000 100000', 32)).toBe(32);
  });

  test('no quota, no file, or a line it cannot read falls back to the node count', () => {
    expect(usableCpus('max 100000\n', 32)).toBe(32);
    expect(usableCpus(null, 12)).toBe(12);
    expect(usableCpus('', 12)).toBe(12);
    expect(usableCpus('garbage', 12)).toBe(12);
    expect(usableCpus('100000 0', 12)).toBe(12);
  });
});

describe('mockedWorkers', () => {
  test("is Playwright's own half, taken of the CPUs the cgroup allows", () => {
    // Before: floor(32 / 2) = 16 in both pods. After:
    expect(mockedWorkers('2000000 100000', 32)).toBe(10); // gate
    expect(mockedWorkers('1600000 100000', 32)).toBe(8); // dev pod
    expect(mockedWorkers(null, 10)).toBe(5); // a laptop
  });

  test('never asks for fewer than one worker', () => {
    expect(mockedWorkers('100000 100000', 32)).toBe(1);
    expect(mockedWorkers(null, 1)).toBe(1);
  });
});
