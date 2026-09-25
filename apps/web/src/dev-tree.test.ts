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

// For a case whose server is UP and answers: no probe deadline a
// starved pod could reach (backlog 75335234). chooseTarget's 2 s
// default is production's defence against a port that never answers;
// here the server always answers, and under twelve copies of this file
// and 200 busy loops on one CPU the probe of a server that 404s ran
// into that 2 s abort again and again (slowest 2.18 s at 80 loops) —
// harmless for a case whose answer is "not reused" either way, and a
// false red for "reuses THIS tree", whose answer the abort flips. Ten
// minutes outlasts the runner's per-test budget, so the probe ends on
// the server's answer and a real hang is that budget's to report.
const ANSWERED = { probeTimeoutMs: 10 * 60_000 } as const;

describe('chooseTarget', () => {
  test('refuses to reuse a server serving a different tree', async () => {
    const port = await freePort();
    stand('/work/some-other-worktree/apps/web', port);

    const target = await chooseTarget(port, {}, ANSWERED);

    expect(target.reuse).toBe(false);
    expect(target.port).not.toBe(port);
  });

  test('refuses to reuse a server that cannot name its tree', async () => {
    const port = await freePort();
    stand(null, port);

    const target = await chooseTarget(port, {}, ANSWERED);

    expect(target.reuse).toBe(false);
    expect(target.port).not.toBe(port);
  });

  test('reuses a server serving THIS tree', async () => {
    const port = await freePort();
    stand(TREE_ID, port);

    const target = await chooseTarget(port, {}, ANSWERED);

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
  // chooseTarget never returned.
  //
  // The knock is made FROM INSIDE the probe's listening window, through
  // the whileBound seam, and the seam returns only once the probe has
  // ACCEPTED it — so the connection close() used to wait on exists on
  // every run. Until 2026-09-24 four knockers retried from outside and
  // the test asserted that one had landed; on the GitHub runner of
  // publish PR #243 the probe opened and closed before any did, and the
  // precondition failed with the fix intact (backlog 2c7559cb).
  //
  // NO CLOCK DECIDES IT (backlog 75335234). Until 2026-09-25 the verdict
  // was a race against Bun.sleep(1_500): a probe slower than 1.5 s read
  // as "hung", so a starved gate pod could red it with the fix intact —
  // under twelve copies and 200 busy loops on one CPU this test took
  // 3.0 s. The verdict is now the EVENT the hang is made of. The rule in
  // dev-tree.ts is that the probe drops every connection it accepts, and
  // its drop is the first 'connection' listener, so by the time this
  // seam's own listener sees the accept the socket is destroyed — or it
  // never will be, and close() will never call back. So the seam reports
  // "kept open" the moment it sees an accepted socket still up, the race
  // resolves on that and fails naming the hang, and the finally below
  // ends the client side, which lets the probe close.
  test('a client knocking on the free port cannot hang the probe', async () => {
    const port = await freePort();
    const held: Socket[] = [];
    let landed = 0;
    let reportKeptOpen: () => void = () => {};
    const keptOpen = new Promise<'the probe kept the knock open'>((resolve) => {
      reportKeptOpen = () => resolve('the probe kept the knock open');
    });
    const knockAndWaitForTheAccept = async (probe: Server): Promise<void> => {
      const accepted = once(probe, 'connection') as Promise<[Socket]>;
      const socket = connect(port, '127.0.0.1');
      held.push(socket);
      const [, [serverSide]] = await Promise.all([once(socket, 'connect'), accepted]);
      landed += 1;
      if (!serverSide.destroyed) reportKeptOpen();
    };

    try {
      const outcome = await Promise.race([
        chooseTarget(port, { CI: '1' }, { whileBound: knockAndWaitForTheAccept }),
        keptOpen,
      ]);

      expect(outcome).not.toBe('the probe kept the knock open');
      expect(landed).toBe(1);
    } finally {
      for (const socket of held) socket.destroy();
    }
  });
});
