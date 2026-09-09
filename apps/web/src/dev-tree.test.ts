// The regression test for the false green of 2026-09-08 (backlog
// eaca07e1): `bun run test:mocked` reused ANY server answering on
// :5174, so two background runs in one worktree passed 94 tests
// against a DIFFERENT worktree's bundle.
//
// Each case stands up a real HTTP server on a real (free) port and
// asks chooseTarget what it would do. No fixed port is bound here —
// a test that hardcodes 5174 would itself collide with a dev-server.

import { afterEach, describe, expect, test } from 'bun:test';
import { serve } from 'bun';

import { TREE_ID, TREE_PATH, chooseTarget, freePort } from './dev-tree';

type StandingServer = ReturnType<typeof serve>;

const running: StandingServer[] = [];

// Stand up a server that claims `tree` at TREE_PATH (or 404s there,
// the way a dev-server from before this fix does) and serves a 200
// shell everywhere else — the shape the old probe accepted blindly.
function stand(tree: string | null, port: number): StandingServer {
  const server = serve({
    port,
    fetch(req: Request): Response {
      const path = new URL(req.url).pathname;
      if (path === TREE_PATH) {
        return tree === null
          ? new Response('not found', { status: 404 })
          : Response.json({ tree });
      }
      return new Response('<!doctype html><title>shell</title>', {
        headers: { 'content-type': 'text/html' },
      });
    },
  });
  running.push(server);
  return server;
}

afterEach(() => {
  while (running.length > 0) running.pop()?.stop(true);
});

describe('chooseTarget', () => {
  test('refuses to reuse a server serving a different tree', async () => {
    const port = await freePort();
    stand('/work/some-other-worktree/apps/web', port);

    const target = await chooseTarget(port, {});

    expect(target.reuse).toBe(false);
    expect(target.port).not.toBe(port);
  });

  test('refuses to reuse a server that cannot name its tree', async () => {
    const port = await freePort();
    stand(null, port);

    const target = await chooseTarget(port, {});

    expect(target.reuse).toBe(false);
    expect(target.port).not.toBe(port);
  });

  test('reuses a server serving THIS tree', async () => {
    const port = await freePort();
    stand(TREE_ID, port);

    const target = await chooseTarget(port, {});

    expect(target.reuse).toBe(true);
    expect(target.port).toBe(port);
  });

  test.each([['CI'], ['GITHUB_ACTIONS'], ['FORGEJO_ACTIONS']])(
    'never reuses under %s, even a matching tree',
    async (marker: string) => {
      const port = await freePort();
      stand(TREE_ID, port);

      const target = await chooseTarget(port, { [marker]: '1' });

      expect(target.reuse).toBe(false);
      expect(target.port).not.toBe(port);
    },
  );

  test('keeps the preferred port when nothing is listening', async () => {
    const port = await freePort();

    const target = await chooseTarget(port, {});

    expect(target.reuse).toBe(false);
    expect(target.port).toBe(port);
  });
});
