# boss-web

Svelte 5 + Runes frontend for BOSS. Shipped 2026-04-23 via the
four-phase Svelte migration; replaces the earlier React app.
Bundled by Bun + `bun-plugin-svelte`.

## Dev loop

```bash
bun install
bun run dev           # http://127.0.0.1:5174
```

`bun run dev` spawns `src/dev-server.ts` with `--hot`, which boots
Bun's Fullstack Dev Server: `index.html` and everything it imports
is bundled on-demand, HMR is attached automatically, and changes to
`.svelte` / `.ts` / `.css` propagate to the browser without a
manual rebuild. The dev server also:

- Proxies `/api/*` directly to each backend service's loopback port
  (bypasses the gateway's auth cookie gate so demo-mode + tests
  work without a session).
- Serves `/plugins/*` from `/var/lib/boss/step-plugins/` (prod
  gateway does this; dev reads from disk).
- Synthesises `x-boss-user` on proxied API calls from the
  `boss-persona` cookie written by `PersonaSwitcher` so backend
  policy scoping reflects the "viewing as" persona.

HMR is registered in `bunfig.toml` (`[serve.static].plugins =
["bun-plugin-svelte"]`).

## Prod build + deploy

```bash
bun run build         # → apps/web/dist/
```

The container image (`infra/oss-quickstart/Dockerfile`) runs this
build and carries `dist/` to where the gateway serves it; a merged
train's converge rolls it. There is no host-side web deploy.

The gateway serves from `BOSS_STATIC_DIR` (default
`/var/lib/boss-web/dist`). No service restart is needed — the
browser pulls the new chunk filenames from `index.html` on next
load. A hard refresh may be required if `index.html` itself is in
the browser cache.

## Typecheck + tests

```bash
bun run typecheck                # svelte-check — 0 errors required
bun run test:unit                # bun test src/
bun run test:mocked              # Playwright against an in-browser mocked backend (CI-gated)
bun run gate                     # all of the above plus the build — what the gate runs
```

There is no live-backend suite in the tree. The one that was here
(`tests/smoke/`, 35 specs against a scratch stack) ran under no CI
job, gate check or chore from 2026-06-18 until it was deleted on
2026-09-18 (design 0e07ce64). What only a real backend can show — a
page rendering the playground's real data — is the nightly
`maintenance-playground-crawl` chore: `bun run test:live` with
`BOSS_E2E_BASE_URL` pointed at the playground, run in-cluster by
`infra/cluster/manifests/boss-playground-crawl.yaml`.

## Layout

```
apps/web/
  index.html              # Bun entrypoint — <script> points at src/main.ts
  bunfig.toml             # registers bun-plugin-svelte for the dev server
  src/
    main.ts               # mount + installStepPluginHost()
    App.svelte            # route dispatcher — big {:if route.kind === '…'} chain
    build.ts              # prod bundler (bun-plugin-svelte configured here too)
    dev-server.ts         # Bun.serve with fullstack routes + api proxy
    router.ts             # parseRoute + href + navigate
    session/              # session.svelte.ts + PersonaSwitcher + permissions
    shell/                # AppShell.svelte (sidebar + topbar)
    debug/                # DebugGear + shared debug-mode state
    steps/                # StepSurface dispatcher + plugin host (plain-DOM)
    <domain>/             # per-domain pages + types + api helpers
  tests/mocked/           # Playwright specs, every /api call mocked in-browser
  tests/live/             # the playground crawl (BOSS_E2E_BASE_URL, nightly chore)
```

Type definitions that would otherwise be shared across domains live
under each domain folder — there is no shared types package. HTTP
boundary translation happens at each fetch call site.

See [`docs/architecture-decisions.md`](../../docs/architecture-decisions.md)
for the load-bearing decisions that shaped this repo layout
(build system, Svelte version, router, CSS reuse, Playwright
reuse, flip-day strategy, abort criterion).
