// THE DEV-SERVER'S READY LINE, AND THE WAIT FOR IT.
//
// The mocked runner (tests/run-mocked.ts) starts `bun src/dev-server.ts`
// and must not hand the port to Playwright before the server is
// listening. Until 2026-09-18 its only readiness signal was an HTTP
// probe of `/`: correct once the server is up, but blind to the window
// before it, and a gate went red with 261 `Unable to connect` lines on
// web-suite, green on re-gate (backlog aa828f3e, class 63a242ca). The
// server SAYS when it is listening — the line below is the first thing
// it prints after `serve()` returns — so the runner now reads that
// statement off the server's own stdout first, with a bound that fails
// loudly naming the port, and only then probes `/` for the bundle.
//
// ONE DEFINITION (CLAUDE.md §9a): dev-server.ts prints readyLine(PORT)
// and run-mocked.ts waits for readyLine(port). Neither spells the text.
//
// Node-safe like dev-tree.ts: no `bun` import, so a config loaded
// under Node could import it too.

export const readyLine = (port: number): string => `boss-web dev server: http://127.0.0.1:${port}`;

/// Resolves when a line of `output` is exactly readyLine(port); rejects
/// when `output` ends first or `timeoutMs` passes first, each naming
/// the port. Every line is handed to `echo` as it completes — before
/// AND after the ready line, because the drain outlives the resolve:
/// the server's later lines (`Bundled page in Xms`, and the exit-time
/// miss summary) are the run's own measurement, and a pipe nobody reads
/// would stall the server.
///
/// The resolved value carries `drained`, which settles when stdout has
/// ENDED and every line has reached `echo`. The runner waits on it
/// before judging the miss summary (backlog 06038ed8): the process
/// having exited does not mean its last line has been read, and the
/// summary is the last line the server prints.
export function waitForReadyLine(
  output: AsyncIterable<Uint8Array | string>,
  port: number,
  timeoutMs: number,
  echo: (line: string) => void,
): Promise<{ drained: Promise<void> }> {
  const wanted = readyLine(port);
  // Never rejects: a drain that ends in an error has still stopped
  // producing lines, which is all a waiter on it wants to know. The
  // ready-line promise below is where a failure is reported.
  let drainDone = (): void => undefined;
  const drained = new Promise<void>((resolve) => {
    drainDone = resolve;
  });
  return new Promise<{ drained: Promise<void> }>((resolve, reject) => {
    let settled = false;
    const settle = (outcome: () => void): void => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      outcome();
    };
    const timer = setTimeout(
      () => settle(() => reject(new Error(`dev-server did not report listening on :${port} within ${timeoutMs}ms`))),
      timeoutMs,
    );
    const drain = async (): Promise<void> => {
      const decoder = new TextDecoder();
      let pending = '';
      for await (const chunk of output) {
        pending += typeof chunk === 'string' ? chunk : decoder.decode(chunk, { stream: true });
        const lines = pending.split('\n');
        pending = lines.pop() ?? '';
        for (const line of lines) {
          echo(line);
          if (line.trim() === wanted) settle(() => resolve({ drained }));
        }
      }
      if (pending) echo(pending);
      drainDone();
      settle(() => reject(new Error(`dev-server stdout ended before it reported listening on :${port}`)));
    };
    drain().catch((err: unknown) => {
      drainDone();
      settle(() => reject(err instanceof Error ? err : new Error(String(err))));
    });
  });
}
