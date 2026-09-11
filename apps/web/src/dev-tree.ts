// WHICH TREE IS THAT SERVER SERVING?
//
// The mocked Playwright suite (`bun run test:mocked` → tests/run-mocked.ts)
// reuses a dev-server that is already up, because a warm server saves the
// ~30s SPA bundle on every local re-run. Until 2026-09-09 "already up"
// meant nothing more than "something answers 2xx on :5174" — and on a pod
// where several worktrees share one network namespace, that is not the
// same question as "already serving MY tree".
//
// It bit for real (backlog eaca07e1). On 2026-09-08 ~03:00 UTC an
// operator dev-server was serving another checkout for screenshots; a
// builder's two background runs in its own worktree reused it and
// reported 94 passing tests against a bundle the builder had never
// touched. Nothing looked wrong: green is green. It surfaced only
// because the builder happened to re-run in the foreground on another
// port. That is a false green of exactly the family CLAUDE.md names —
// "a green gate only covers what it runs".
//
// THE RULE: a dev-server names the tree it serves at TREE_PATH, and the
// runner reuses a server only when that name equals its own. Anything
// else — a foreign tree, a server too old to answer, a port held by an
// unrelated process — is not reused; the runner takes a free port and
// starts its own. Under CI there is no reuse at all, matching tree or
// not: a gate must exercise the tree it was handed and nothing else.
//
// ONE DEFINITION, NOT TWO (CLAUDE.md §9a). Both sides of the handshake —
// the dev-server that answers and the runner that asks — import TREE_ID
// from this file, and TREE_ID is derived from this file's own location.
// There is no manifest to keep in sync and nothing to pass on a command
// line, so the two sides cannot disagree about what tree they are in.
//
// WHY THE PATH AND NOT THE GIT HEAD. The failure is "a different
// checkout", and two checkouts always differ by path. HEAD would cost a
// git subprocess on every probe and answer a question nobody asked: a
// server in THIS tree serves this tree's files whatever HEAD says, and
// dev-servers in two different trees at the same commit are still the
// wrong server to test against — the interesting case is precisely an
// uncommitted change.

// NODE-SAFE ON PURPOSE — no `bun` import, no `import.meta.dir`.
// playwright.mocked.config.ts imports DEFAULT_PORT from here and
// Playwright loads its config under NODE, which cannot resolve the
// `bun` package: an import of it here fails the whole suite after the
// server is already up (measured, 2026-09-09). Everything below uses
// APIs both runtimes have — `node:net` for ports, `import.meta.url` for
// the module's own location.
import { realpathSync } from 'node:fs';
import { createServer } from 'node:net';
import { dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

// The endpoint a dev-server answers with its identity. Kept obscure
// enough to never collide with an app route (the SPA catch-all owns
// everything else).
export const TREE_PATH = '/__boss_tree';

// The port the suite prefers, shared by the dev-server, the runner and
// the Playwright config so the default lives in one place.
export const DEFAULT_PORT = 5174;

// This tree's identity: the resolved absolute path of the web app root
// (the parent of src/, where this module lives). Resolved through
// symlinks so two spellings of one directory are one identity.
export const TREE_ID: string = realpathSync(
  dirname(dirname(fileURLToPath(import.meta.url))),
);

// CI markers, the same triple infra/gate.sh and infra/dev-shared-target.sh
// test. Forgejo Actions sets CI and GITHUB_ACTIONS on every job.
const CI_MARKERS = ['CI', 'GITHUB_ACTIONS', 'FORGEJO_ACTIONS'] as const;

export function isCi(env: Record<string, string | undefined>): boolean {
  return CI_MARKERS.some((marker) => {
    const value = env[marker];
    return value !== undefined && value !== '';
  });
}

// The dev-server's half of the handshake.
export function treeResponse(): Response {
  return Response.json({ tree: TREE_ID });
}

// The runner's half: what tree is the server on this origin serving?
// null covers every not-mine answer — nothing listening, a connection
// error, a non-2xx, a dev-server too old to know TREE_PATH, or some
// unrelated process that happens to hold the port.
export async function probeTree(
  origin: string,
  timeoutMs: number,
): Promise<string | null> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const res = await fetch(`${origin}${TREE_PATH}`, { signal: controller.signal });
    if (!res.ok) {
      await res.body?.cancel();
      return null;
    }
    const body: unknown = await res.json();
    const tree = (body as { tree?: unknown } | null)?.tree;
    return typeof tree === 'string' ? tree : null;
  } catch {
    return null;
  } finally {
    clearTimeout(timer);
  }
}

// Bind `port` (0 = "any free one"), then let it go: resolves the port
// actually bound, or null if something already holds it.
function tryBind(port: number): Promise<number | null> {
  return new Promise((resolve) => {
    const probe = createServer();
    probe.once('error', () => resolve(null));
    probe.listen(port, () => {
      const address = probe.address();
      const bound =
        typeof address === 'object' && address !== null ? address.port : null;
      probe.close(() => resolve(bound));
    });
  });
}

// An ephemeral port the OS just told us was free. Used only when the
// preferred port is held by a server we must not reuse, so the brief
// bind/close race with another process is not worth guarding.
export async function freePort(): Promise<number> {
  const port = await tryBind(0);
  if (port === null) {
    throw new Error('could not obtain a free port from the OS');
  }
  return port;
}

async function portIsFree(port: number): Promise<boolean> {
  return (await tryBind(port)) !== null;
}

export type Target = {
  // Where the suite will point: the preferred port, or a free one.
  readonly port: number;
  // True only for a server already serving THIS tree, off CI.
  readonly reuse: boolean;
  // Human-readable justification, logged by the runner so a run says
  // out loud which server it tested.
  readonly reason: string;
};

// The whole decision, in one call, so it can be tested against a real
// server on a real port.
export async function chooseTarget(
  preferredPort: number,
  env: Record<string, string | undefined>,
  probeTimeoutMs = 2_000,
): Promise<Target> {
  const origin = `http://127.0.0.1:${preferredPort}`;

  if (isCi(env)) {
    // No reuse under CI, matching tree or not. If something already
    // holds the port, step aside rather than dying on EADDRINUSE.
    return (await portIsFree(preferredPort))
      ? { port: preferredPort, reuse: false, reason: 'CI: starting our own server' }
      : {
          port: await freePort(),
          reuse: false,
          reason: `CI: :${preferredPort} is occupied, starting our own on a free port`,
        };
  }

  const tree = await probeTree(origin, probeTimeoutMs);
  if (tree === TREE_ID) {
    return { port: preferredPort, reuse: true, reason: `reusing the server serving ${TREE_ID}` };
  }
  if (tree !== null) {
    return {
      port: await freePort(),
      reuse: false,
      reason: `:${preferredPort} is serving a DIFFERENT tree (${tree}) — not reusing it`,
    };
  }
  if (await portIsFree(preferredPort)) {
    return { port: preferredPort, reuse: false, reason: 'nothing listening — starting our own' };
  }
  return {
    port: await freePort(),
    reuse: false,
    reason: `:${preferredPort} is held by something that does not name its tree — not reusing it`,
  };
}
