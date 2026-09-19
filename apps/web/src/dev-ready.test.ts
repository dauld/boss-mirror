// The mocked runner's wait for the dev-server's own "I am listening"
// line (backlog aa828f3e, the connect-before-ready class 63a242ca).
// Each case feeds the wait a scripted stdout and reads what it did:
// no server, no port, no clock but the one timeout under test.

import { describe, expect, test } from 'bun:test';

import { readyLine, waitForReadyLine } from './dev-ready';

const PORT = 5174;

// A stdout that yields these chunks, one per turn of the event loop.
async function* chunks(...parts: string[]): AsyncGenerator<Uint8Array> {
  for (const part of parts) {
    await Bun.sleep(1);
    yield new TextEncoder().encode(part);
  }
}

// A stdout that never says anything and never ends.
async function* silence(): AsyncGenerator<Uint8Array> {
  await new Promise(() => undefined);
}

describe('waitForReadyLine', () => {
  test('resolves on the ready line and echoes every line, before and after it', async () => {
    const echoed: string[] = [];
    const out = chunks('warming\n', `${readyLine(PORT)}\n  HMR: enabled\n`, 'Bundled page in 5966ms: index.html\n');
    await waitForReadyLine(out, PORT, 1_000, (l) => echoed.push(l));
    expect(echoed.slice(0, 2)).toEqual(['warming', readyLine(PORT)]);
    // The drain outlives the resolve: the lines after the ready line
    // are the run's own measurement and must still reach the log.
    await Bun.sleep(20);
    expect(echoed).toEqual(['warming', readyLine(PORT), '  HMR: enabled', 'Bundled page in 5966ms: index.html']);
  });

  // The runner reads the dev-server's exit-time miss summary off this
  // drain and REFUSES the run when there is one (backlog 06038ed8), so
  // it needs to know when the last line has been read — not merely that
  // the process is gone. `drained` is that statement.
  test('hands back a drained promise that resolves once stdout has ended', async () => {
    const echoed: string[] = [];
    const out = chunks(`${readyLine(PORT)}\n`, 'boss-web dev server (mocked): 1 /api/** request(s)\n');
    const { drained } = await waitForReadyLine(out, PORT, 1_000, (l) => echoed.push(l));
    await drained;
    expect(echoed).toEqual([readyLine(PORT), 'boss-web dev server (mocked): 1 /api/** request(s)']);
  });

  test('a ready line split across two chunks is still one line', async () => {
    const line = readyLine(PORT);
    const out = chunks(line.slice(0, 12), `${line.slice(12)}\n`);
    await waitForReadyLine(out, PORT, 1_000, () => undefined);
  });

  test("another port's ready line is not this port's — :51740 is not :5174", async () => {
    const out = chunks(`${readyLine(51740)}\n`);
    await expect(waitForReadyLine(out, PORT, 1_000, () => undefined)).rejects.toThrow(`:${PORT}`);
  });

  test('stdout ending without the line rejects, naming the port', async () => {
    const out = chunks('error: port in use\n');
    await expect(waitForReadyLine(out, PORT, 1_000, () => undefined)).rejects.toThrow(
      `dev-server stdout ended before it reported listening on :${PORT}`,
    );
  });

  test('a silent server rejects at the timeout, naming the port and the bound', async () => {
    await expect(waitForReadyLine(silence(), PORT, 50, () => undefined)).rejects.toThrow(
      `dev-server did not report listening on :${PORT} within 50ms`,
    );
  });
});
