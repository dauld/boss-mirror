# Machine token activation (7fcd78fa phase 1)

**Status**: superseded — do not run. Design 6805c764 (approved
2026-09-25) replaces this ceremony: the credential broker mints and
rotates the token (car 3, which deletes this file), and since its car 1
the gate no longer reads `BOSS_MACHINE_TOKEN` at all. It is
`boss_core::machine_gate`, mounted on every service port, and takes its
mode (`off | report | enforce`, default `off`) from the file named by
`BOSS_MACHINE_GATE_MODE_FILE` and its `current`/`next`/`previous`
slots from the directory named by `BOSS_MACHINE_TOKEN_DIR`. Setting the
env var below on the jobs API therefore arms nothing, and unsetting it
rolls nothing back. The text below is kept for the history of phase 1.

The code side is done and dormant: the jobs API refuses tokenless
writes ONLY once `BOSS_MACHINE_TOKEN` is set in its environment, and
every in-repo writer attaches the same env var's value when it has
one. Activation is therefore a pure ops action, done by David (token
administration is his by standing rule), and rollback is unsetting
the variable on the jobs API.

## What carries the token already (code, this car)

- jobs API: enforcement on POST/PUT/PATCH/DELETE (`machine_gate.rs`);
  GET/HEAD/OPTIONS stay open in phase 1.
- gateway: stamps the token on session-authenticated proxied traffic;
  the edge strip kills any client-supplied copy.
- dispatcher, dispatcher handlers (shared `api_client()`), conductor
  (`boss train`), sim workforce, brewery bootstrap: default header
  when the env var is set.
- every folded `boss-*-client` (via `http_client::base`).
- maintenance shell writers (`boss-step.sh`,
  `boss-maintenance-wrap.sh`): `${BOSS_MACHINE_TOKEN:+-H …}`.

## Activation steps (David)

1. Generate, without echoing into a transcript:
   `openssl rand -hex 32 > /some/offline/place`
2. Cluster: create the secret and add `BOSS_MACHINE_TOKEN` (from that
   secret) to the env of: jobs-api, gateway, dispatcher, brewery
   engine, sim, and the maintenance CronJobs. One secret, one key.
3. boss-gcp: add `BOSS_MACHINE_TOKEN` to the conductor's timer/unit
   environment (systemd drop-in), and write the value to
   `/etc/boss/machine-token` (mode 0600, David-owned) for the wrapper
   patch below.
4. Patch `~/bin/boss-api` (David's file, outside the repo) so agent
   sessions keep working without the token ever reaching the Mac —
   the token is read ON boss-gcp at call time:

   ```
   -CURL="curl -sS -X $METHOD -H 'Content-Type: application/json' -H 'X-Boss-User: $ACTOR' -w '\n%{http_code}' '$BASE$API_PATH'"
   +CURL="TOK=\$(cat /etc/boss/machine-token 2>/dev/null || true); curl -sS -X $METHOD -H 'Content-Type: application/json' -H 'X-Boss-User: $ACTOR' \${TOK:+-H \"X-Boss-Machine-Token: \$TOK\"} -w '\n%{http_code}' '$BASE$API_PATH'"
   ```

5. Restart / roll the deployments. The jobs-api log line
   `machine token configured: writes require x-boss-machine-token`
   confirms the door is armed; the `no BOSS_MACHINE_TOKEN configured`
   warning is the dormant state.

## Verifying, and what a miss looks like

A writer missed by this list fails loudly, not silently: its log
shows `401 … machine door: writes require the x-boss-machine-token
header … this caller sent no token`. That message is the work order —
attach the env var to that process. Reads are untouched, so
dashboards and lenses keep rendering while a missed writer is fixed.

Rollback: unset `BOSS_MACHINE_TOKEN` on the jobs API and restart it.
Everything else may keep the variable; attaching to an open door is
harmless.

## Phase 2 (not this car)

Reads join the gate once every legitimate caller demonstrably carries
the token (watch for 401s at zero for a week). Phase 3, mTLS, only if
the fabric grows callers outside the WireGuard net.
