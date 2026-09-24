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
import { once } from 'node:events';
import { type Server, type Socket, connect } from 'node:net';

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

  // Backlog e3470b9a. The free-port probe binds the port for a moment and
  // waited for close() to call back — and close() calls back only once
  // every connection the probe ACCEPTED has ended. A client that knocks
  // on the port while the probe is up (on the pod, another worktree's
  // runner or browser retrying :5174) and holds its connection open made
  // the probe wait forever. Measured before the fix: with this knocker,
  // chooseTarget never returned. Raced against a bound well inside the
  // test timeout so a hang reads as a hang, not as a 5s timeout.
  //
  // The knock is made FROM INSIDE the probe's listening window, through
  // the whileBound seam, and the seam returns only once the probe has
  // ACCEPTED it — so the connection close() used to wait on exists on
  // every run. Until 2026-09-24 four knockers retried from outside and
  // the test asserted that one had landed; on the GitHub runner of
  // publish PR #243 the probe opened and closed before any did, and the
  // precondition failed with the fix intact (backlog 2c7559cb).
  test('a client knocking on the free port cannot hang the probe', async () => {
    const port = await freePort();
    const held: Socket[] = [];
    let landed = 0;
    const knockAndWaitForTheAccept = async (probe: Server): Promise<void> => {
      const accepted = once(probe, 'connection');
      const socket = connect(port, '127.0.0.1');
      held.push(socket);
      await Promise.all([once(socket, 'connect'), accepted]);
      landed += 1;
    };

    try {
      const outcome = await Promise.race([
        chooseTarget(port, { CI: '1' }, { whileBound: knockAndWaitForTheAccept }),
        Bun.sleep(1_500).then(() => 'hung' as const),
      ]);

      expect(outcome).not.toBe('hung');
      expect(landed).toBe(1);
    } finally {
      for (const socket of held) socket.destroy();
    }
  });
});
