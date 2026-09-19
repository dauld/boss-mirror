// The pin for backlog 82b87a09: in mocked mode the dev-server answers
// an `/api/**` request the in-browser mock did not catch LOCALLY, with a
// 404 that says so, instead of proxying it to a service port nothing
// listens on. Until 2026-09-19 every such miss cost six lines of bun's
// connect-refusal block in every passing run (260 on one baseline run
// of 104 specs), and three readers grew filters for it.

import { describe, expect, test } from 'bun:test';

import { MOCKED_FLAG, apiHandler, isMissSummary, isMocked, missRefusal, missSummary } from './dev-mocked';

const url = (path: string): URL => new URL(`http://127.0.0.1:5174${path}`);

// A proxy that records whether it was reached — the thing mocked mode
// must never do.
function countingProxy() {
  const calls: string[] = [];
  const proxy = (_req: Request, path: string): Response => {
    calls.push(path);
    return new Response('from upstream', { status: 200 });
  };
  return { proxy, calls };
}

describe('isMocked', () => {
  test('reads the flag the mocked runner sets, and nothing else', () => {
    expect(isMocked({ [MOCKED_FLAG]: '1' })).toBe(true);
    expect(isMocked({})).toBe(false);
    expect(isMocked({ [MOCKED_FLAG]: '0' })).toBe(false);
    expect(isMocked({ BOSS_SCRATCH: '0' })).toBe(false);
  });
});

describe('apiHandler', () => {
  test('mocked: a miss answers 404 locally and the proxy is never reached', async () => {
    const { proxy, calls } = countingProxy();
    const misses = new Map<string, number>();
    const handle = apiHandler(true, proxy, misses);

    const u = url('/api/jobs/live?limit=5');
    const res = await handle(new Request(u, { method: 'GET' }), u.pathname, u);

    expect(res.status).toBe(404);
    expect(await res.json()).toEqual({ mock: 'unanswered', path: '/api/jobs/live' });
    expect(calls).toEqual([]);
    expect([...misses.entries()]).toEqual([['GET /api/jobs/live', 1]]);
  });

  test('not mocked: the proxy path is taken and nothing is recorded', async () => {
    const { proxy, calls } = countingProxy();
    const misses = new Map<string, number>();
    const handle = apiHandler(false, proxy, misses);

    const u = url('/api/people');
    const res = await handle(new Request(u), u.pathname, u);

    expect(res.status).toBe(200);
    expect(await res.text()).toBe('from upstream');
    expect(calls).toEqual(['/api/people']);
    expect(misses.size).toBe(0);
  });

  test('mocked: repeats of one path count on one key, the query string dropped', async () => {
    const { proxy } = countingProxy();
    const misses = new Map<string, number>();
    const handle = apiHandler(true, proxy, misses);

    for (const q of ['?status=open', '?status=closed', '']) {
      const u = url(`/api/jobs${q}`);
      await handle(new Request(u), u.pathname, u);
    }
    const p = url('/api/surface-opens');
    await handle(new Request(p, { method: 'POST', body: '{}' }), p.pathname, p);

    expect([...misses.entries()]).toEqual([
      ['GET /api/jobs', 3],
      ['POST /api/surface-opens', 1],
    ]);
  });
});

describe('missSummary', () => {
  test('is null when every request was answered by the mock', () => {
    expect(missSummary(new Map())).toBeNull();
  });

  test('is ONE line naming every distinct miss with its count, most frequent first', () => {
    const line = missSummary(
      new Map([
        ['GET /api/jobs', 3],
        ['POST /api/surface-opens', 1],
        ['GET /api/people', 42],
      ]),
    );
    expect(line).not.toBeNull();
    expect(line?.includes('\n')).toBe(false);
    expect(line).toContain('46 /api/** request(s)');
    expect(line).toContain('3 distinct');
    expect(line).toMatch(/GET \/api\/people x42.*GET \/api\/jobs x3.*POST \/api\/surface-opens x1/);
  });
});

// THE RUNNER'S REFUSAL (backlog 06038ed8). Until 2026-09-19 the summary
// line was only echoed: tests/run-mocked.ts exited on Playwright's code
// alone, so a spec that mounted without installApiFloor leaked its
// reads to the dev-server again and the run still read green — the
// 403-misses-per-run measurement that car f88e7908 drove to zero had
// nothing mechanical holding it there. These two functions are the
// decision the runner makes, pure: what the summary line looks like,
// and what a run that printed one is told.
describe('isMissSummary', () => {
  test('recognises the line the dev-server prints at exit', () => {
    const line = missSummary(new Map([['GET /api/jobs', 3]]));
    expect(line).not.toBeNull();
    expect(isMissSummary(line ?? '')).toBe(true);
  });

  test('is not any other line the dev-server prints', () => {
    for (const other of [
      'boss-web dev server: http://127.0.0.1:5174',
      '  api proxy → OFF (mocked mode: an /api/** miss answers 404 locally, summarised at exit)',
      'Bundled page in 5966ms: index.html',
      '',
    ]) {
      expect(isMissSummary(other)).toBe(false);
    }
  });
});

describe('missRefusal', () => {
  test('is null when no summary was printed — the mock answered everything', () => {
    expect(missRefusal(null)).toBeNull();
  });

  test('names the paths that leaked and the floor that answers them', () => {
    const line = missSummary(new Map([['GET /api/workflows', 12]])) ?? '';
    const refusal = missRefusal(line);
    expect(refusal).not.toBeNull();
    expect(refusal).toContain('GET /api/workflows x12');
    expect(refusal).toContain('installApiFloor');
  });
});
