# Operating the forge host

The forge host (`forge`, 10.20.0.15) runs the repository, the OCI
registry, CI dispatch, and the loop that deploys the cluster. Until
2026-09-05 its operational knowledge was written down nowhere, and the
measured cost was a night of guessing: on 2026-09-03 a full disk wedged
Forgejo's Actions dispatcher, zero workflow runs were created for eight
hours, three train PRs starved, and the diagnosis needed four human
commands because every layer was guessed — two unit names wrong before
one was right (packet 4d5f158a). This file is the roster and the
failure modes, read off the host itself, so no operator or agent guesses
a unit name again. **When the host and this file disagree, the host is
the fact and the disagreement is a car.**

Every number below was measured on 2026-09-05 unless dated otherwise.
Read them again before acting on them; the estate registry
(`GET /api/estate/nodes`, id `forge`) and the host's own journal are
the current truth, this file is the map.

## What runs here

The host is one machine: 16 CPU, 30 GB memory, one 228 GB NVMe with a
single root filesystem. There is no separate build volume. `df /` is
the only filesystem that matters, and every consumer below shares it.

| process | how it runs | what it does | where its state lives |
|---|---|---|---|
| Forgejo | docker container named `forgejo` on the **system** docker daemon, managed by the compose file at `/opt/forgejo/docker-compose.yml` | the repository (`david/boss`), the web UI and API on `:3000`, the OCI package registry, and the Actions dispatcher that turns a push into workflow runs | `/opt/forgejo/data` (33 GB) — repositories, packages, Actions logs and artifacts |
| CI runner | `forgejo-runner.service` (act runner) | polls Forgejo for tasks and runs each CI job as a container on the **system** docker daemon; its journal names each task (`task 1746 repo is david/boss`) and each job's network cleanup | job containers and the per-train `boss-ci:<sha>` images, under the system daemon's store (`/var/lib/containerd`, 81 GB on 2026-09-05 before the sweep learned to prune it) |
| system docker daemon | `docker.service` + `containerd.service` | the daemon CI jobs and Forgejo run on | `/var/lib/containerd` |
| rootless docker daemon | David's user daemon (data-root `/home/david/.local/share/docker`) | the daemon `cluster-deploy-runner` builds the cluster image on | its own image and build cache |
| WireGuard | `wg-quick@wg0.service` | the tunnel to boss-gcp and the hub; the pod reaches this host over the LAN, not the tunnel | — |
| journal gateway | `systemd-journal-gatewayd` on `:19531` | the read door: any unit's journal over HTTP from the pod, plus anything a script logs with `\| systemd-cat -t <tag>` | hand-installed 2026-09-03; **not in `install.sh`** — see Residue |

## The BOSS units

All BOSS units run from the host's checkout at `/home/david/boss` (a
detached checkout of forge `main`, never `git pull`), and every one is
installed by `infra/forge/install.sh`. **Every git user of that
checkout takes one lock first** — `.git/boss-converge.lock`, via
`infra/forge/checkout-lock.sh` (`checkout_git`), which also sweeps a
stale `index.lock` nobody holds and retries the two git lock errors
with a short backoff (5 tries, 6 s apart). Two units fetch the same
checkout on the same tick, and the merge-triggered `converge` starts
one of them a second after any merge; without the lock they collided
on 2026-09-07 22:01 (failure mode 8 below). A hand-run git in the
checkout should take the same lock:
`flock /home/david/boss/.git/boss-converge.lock git fetch -q forgejo main`. Since 2026-09-04 `forge-converge`
runs that installer every ten minutes, so a unit that lands on main is
installed on its next tick — the "landed but never installed" class is
closed for everything in the installer's `UNITS` list.

| unit | cadence | does | fails loudly how |
|---|---|---|---|
| `forge-converge` | 10 min (boot +4) | fetch forge main and check it out under the checkout lock (as the owner), run `install.sh` — the host adopts its own units | journal; a broken install leaves the previous units running |
| `cluster-deploy-runner` | 10 min (boot +3), and on every merge via the `converge` ops verb | fetch and check out forge main under the checkout lock, build the cluster image on the rootless daemon, push it to the registry, roll the cluster, then verify every manifest under `infra/cluster/manifests` is applied and not drifted | exit 1 on drift or an unreadable manifest; the conductor's converge step reads the result; a run started by a `converge` ops-request PATCHes that packet with `converged: <sha>`, `converge_held: <reason>` or `converge_failed: <stage> (exit N)` when it ends |
| `disk-floor-sweep` | hourly (boot +5) | keep free disk above the floor, `BOSS_DISK_FLOOR_GB=100` in the service; prunes the **system** daemon's CI images first (`until=24h`), then the rootless caches, in a fixed order; regenerable caches only, never volumes | exits non-zero with `FLOOR UNMET — a human decides next` rather than deleting harder |
| `reap-dead-ci-jobs` | daily (boot +15) | remove the containers and volumes of crashed CI jobs | journal |
| `estate-observe-host` | 15 min (boot +3) | record this host's disk, load and units into the estate as observations; the conductor's boarding refuses on a positive "host is short" reading | journal; a stale series reads as unverifiable, and boarding proceeds with one loud line |
| `boss-ops-runner` | ~1 min | answer `ops-request` packets filed against `forge` with a verb from `infra/ops/verbs.json` | `refused` outcome on the packet; installed by `install.sh` since 2026-09-05 (a drop-in carries this host's identity) |
| `cluster-watchdog` | 5 min (boot +2) | know the cluster is working from outside it; roll to the last converged build after three dark checks | its own journal line every tick, `hands needed` when it cannot act |

Two disk floors, deliberately different: the locomotive refuses a CI
run below **70 GB** free at run start, and the sweep keeps **100 GB**
free, so the sweep buys the headroom a consist consumes mid-flight. A
sweep floor equal to the CI floor never bought anything (2026-09-05,
train #204).

**Every run ends in its verdict.** Each unit above opens (or reuses)
its `maintenance-*` packet from ExecStartPre and records the verdict
from `ExecStopPost` — the one phase systemd runs whether ExecStart
succeeded or not. `boss-step.sh` reads `$SERVICE_RESULT`: a run that
succeeded closes its packet *Maintenance completed*; one that died
closes it *Maintenance failed* carrying `result` (`exit-code`,
`timeout`, `signal`) and `exit_status`. A packet still open at "Run to
completion" therefore means the run is genuinely still running, or the
host rebooted mid-run — and the next run's wrap adopts and completes
it. Before 2026-09-05 a failed run recorded nothing, and the open
packet looked exactly like a run in progress.

## Reading the host from the pod

No ssh from the pod. Three doors, all read-only:

- **The journal gateway.**
  `http://10.20.0.15:19531/entries?_SYSTEMD_UNIT=<unit>` for any unit
  above; `?SYSLOG_IDENTIFIER=<tag>` for a script's own output;
  `/fields/_SYSTEMD_UNIT` lists every unit that has ever logged. Ask
  for `Accept: application/json` and read line by line.
- **An ops-request packet.** `boss job file --kind ops-request
  --metadata '{"host":"forge","verb":"df"}'`; the runner answers within
  about a minute with the output on the packet's `execute` step. Verbs:
  `df`, `uptime`, `timer-list`, `unit-status <unit>`, `journal-tail
  <unit> [n]`, `disk-report` (what is consuming disk — both daemons,
  Forgejo's data, the checkout), `reach <ipv4> <port>` (one TCP
  connect from this host's vantage — the WireGuard overlay and the
  LAN the pod cannot route to; nothing sent), and the mutating verbs, each
  authorized by name in `verbs.json`: `reclaim-disk <floor>` (the
  sweep, with a floor), `rollback-to <sha>` (roll deploy/boss to a
  named build, verified Ready), `hold-converge <reason>` and
  `release-converge` (the runner builds and rolls nothing while a
  hold stands), `mirror-base-images`, and `publish-github-pr` (the
  machine step of publish-to-github v6: snapshot forge main onto the
  public mirror as a PR from the dauld fork; reads the dauld token at
  `/etc/boss-publish/github.token`, credentials registry
  `dauld-github-token`, and refuses loudly without it; `--check`
  validates its inputs with no network), and `run-car-probe <car-uuid>`
  (the machine half of `boss prove`: runs the probe a landed car
  recorded at park time — `boss gate --park-probe/--park-expect` — as
  david, never root, and completes the car's `proven` step with the
  proof record or stamps `proof_attempt`; filed per car by the
  dispatcher when its train arrives). The verb list is the tree's,
  never the packet's.
- **The Forgejo API** with the repo-scoped token at `/etc/forge/token`
  on the pod: runs at `/api/v1/repos/david/boss/actions/tasks`, a job's
  log at `/api/v1/repos/david/boss/actions/jobs/<jobId>/logs`, a
  commit's statuses at `/commits/<sha>/status`.

  **Getting `<jobId>` right — the trap (live-confirmed 2026-09-06).**
  The log endpoint answers, silently, for the WRONG noun. Two ids look
  interchangeable and are not:
  - `.../actions/tasks/<id>/logs` → **404** (wrong noun; there is no
    per-task log route).
  - `.../actions/jobs/<jobId>/logs` → the log, but ONLY when `<jobId>`
    is the **job id** from `GET /actions/runs/<run>/jobs`. That payload
    is a plain array; each job carries both an `id` (the log key) and a
    `task_id`. **Passing the `task_id` returns a DIFFERENT job's log**
    — a success log for a failed run — with a 200, not an error.

  So resolve, never guess:
  1. A failing check's combined-status `target_url` is
     `…/actions/runs/<run>/jobs/<index>` — it carries the run and the
     job's POSITION in that run.
  2. `GET /actions/runs/<run>/jobs` → index into the array at `<index>`;
     read that entry's `id` (cross-check its `name` against the check),
     **not** its `task_id`.
  3. `GET /actions/jobs/<id>/logs` with `Range: bytes=-16384` → the tail
     only (the forge honours the Range with `206 Partial Content`; a
     `test` job's log runs to megabytes). Read the tail; never grep for
     `test result: FAILED`.

  Worked example: run 462's failing `web` was job `id` 2008 /
  `task_id` 1939 — `jobs/2008/logs` is the real failure, `jobs/1939/logs`
  an unrelated success. The conductor now does all of this
  automatically for a red train (`attach_failing_logs` in
  `crates/orchestrators/boss-cli/src/train.rs`): the failing job's log
  tail rides on the `ci` step's `check_logs` and in the red-train
  alert's `failing_logs`, so a red verdict names WHY, not just WHICH.

Anything else — a restart, a prune beyond the sweep, a compose action
— is a human on the host, and the command should be handed over ready
to paste with its expected output stated.

## Failure modes, in the order they have actually happened

### 1. The disk fills

The recurring one (2026-08-17, 08-22, 09-02, 09-03, 09-05). Symptoms:

- a train's `CI / locomotive` job ends with `LOCOMOTIVE RED: <n>GB free
  on the workspace filesystem, need 70GB` and posts a failing commit
  status whose description starts with `refused:` — the conductor spares
  the cars on that description; or
- a `test` job goes red **after every test passed**, dying at the web
  install's disk gate — read the job log's tail, never grep for
  `test result: FAILED`.

Consumers, largest first, as measured: the system daemon's per-train
CI images (81 GB, now swept hourly), the rootless daemon's converge
build cache, `/opt/forgejo/data` (33 GB, grows with packages and
Actions logs), and each cold `target/` (about 40 GB since lean builds
landed 2026-09-04). Read with `disk-report`; reclaim with
`reclaim-disk`; beyond the sweep's bound — `docker image prune -a` on
the system daemon freed 54 GB on 2026-09-05 — is a human decision, and
the number to hand over is the free space before and after.

While the host is short: leave a red train standing (a cancel is what
strikes its cars), hold the cars that would board next, and read the
trend from `disk-floor-sweep`'s journal.

### 2. The Actions dispatcher wedges

2026-09-03, about 20:30: disk full, and Forgejo stopped creating
workflow runs for eight hours while its process stayed up. The signal
is **the newest run's age against the newest push's age**: a push with
no run a minute later is a wedge, whatever `unit-status` says. Read
`/actions/tasks?limit=3` and compare with the newest PR's head. Free
the disk first; a dispatcher that wedged on a full disk does not
recover on its own, so the container is restarted from
`/opt/forgejo` with its compose file — the container is `forgejo`,
the data is a bind mount at `/opt/forgejo/data`, and nothing is
removed. Verify by pushing and seeing a run appear.

### 3. The runner stops taking tasks

Jobs queue as waiting and nothing starts. `unit-status forgejo-runner`
and `journal-tail forgejo-runner` say whether it is dead, stuck on a
cleanup, or running tasks for a different reason. A restart of
`forgejo-runner.service` is the remedy and is a human action today.

### 4. The CI runner cannot resolve github.com

2026-09-05 08:13: the locomotive job of train #212 died in eleven seconds
with `could not fetch remote 'origin': … Could not resolve host:
github.com` while fetching `actions/checkout`, one minute after
build-image on the same run had checked out fine. No car's code ran.
Every CI job resolves `actions/checkout@v4` against github.com on
start, so a DNS blip on this host is a red train with no car at fault,
and it carries no `refused:` status, so the conductor would strike the
cars. Read the job log's first lines; cancel naming the cause
(`boss train cancel <id> --reason …`, run in the conductor pod, which
holds the write token) and the cars come back with no strike. The
durable fix is a mirror of the action on this forge (packet
d9a34560).

### 5. A build bricks the cluster's boot

2026-09-05 09:33: train #213 rolled an image whose launcher sourced a
file the image had not copied beside it; the pod crash-looped before
any API started and the system of record was dark until 14:00. Three
things went wrong at once, and each now has its own guard:

- **The converge's rollback rolled to the wrong place.** It targeted the
  revision its own `kubectl apply` had just created from the manifest's
  literal image tag (`b2814ef`, a 2026-08-10 build that cannot boot
  against today's registry). Now (`cluster-deploy-lib.sh`) it rolls back
  to the last CONVERGED build by image name — the sha in
  `~/.boss-last-built` — verifies it Ready, and applies manifests with
  that same image so no placeholder revision exists.
- **Nothing proved the image could boot.** Now the converge runs the
  image's own `boss-launch --check` before anything is applied; a head
  that fails is quarantined with no dark window. The pre-merge half
  (CI builds and boots the product image, the converge pulls it) is
  the next car.
- **The converge could not run while the API was dark**, because its
  packet step (`boss-maintenance-wrap.sh` as ExecStartPre) failed on the
  unreachable API and systemd never started it — the loop that would
  have restored the system of record was waiting on it. Now the wrap
  exits 0 with a loud UNREACHABLE line; the work runs, one run's
  visibility is lost.

The lever, by name, if it is ever needed by hand again:
`rollback-to <sha>` as an ops verb, or on this host
`sudo docker run --rm --network host -v /home/david/kc.yaml:/kc:ro
alpine/k8s:1.33.3 kubectl --kubeconfig=/kc -n boss rollout undo
deployment/boss --to-revision=<revision whose image is the last converged
sha>`; read the revision from `rollout history --revision=N`. And the
converge by hand, bypassing its packet step (the runner takes the
checkout lock itself; only the git you run by hand needs the `flock`):
`cd /home/david/boss && flock .git/boss-converge.lock git fetch -q
forgejo main && flock .git/boss-converge.lock git checkout -qf
forgejo/main && infra/forge/cluster-deploy-runner.sh`.

### 6. The cluster is dark and nobody is awake

`cluster-watchdog` (every 5 minutes, no packet precondition) reads
`/api/jobs/health` from this host, compares what the deployment serves
with the last converged build, and after three dark checks rolls the
deployment to that build by name. Its journal says every five minutes
either `cluster ok: api answers on <sha>, deployment serves <sha>, last
converged <sha>` or why not; `hands needed` means the converged build
itself is dark. Read it over the journal gateway when the API is down —
that is the point of it.

### 7. Landed but never installed

A unit authored in the tree but never on the host. Closed for the
installer's `UNITS` list by `forge-converge`; `timer-list` shows the
five timers and when each fires next, and an absent timer is the
symptom. The two hand-installed pieces below are still open.

### 8. Two converges collide in the shared checkout

2026-09-07 22:01: train #257 merged at 22:01:22, the `converge`
ops-request fired within a second and `cluster-deploy-runner` started at
22:01:23 — on the same tick as `forge-converge`'s fetch of the same
checkout. The runner died on `error: cannot lock ref
'refs/remotes/forgejo/main': is at 1994077 but expected 77152b8` (exit
1); the 22:11 timer retry died on a stale `.git/index.lock` the first
death had left (exit 128); the 22:21 retry built. Two lost cycles, a
twenty-minute converge lag nine minutes short of the 30-minute alarm,
and the next board held on an occupied track. The ops-request read
`answered` throughout, because `systemctl start --no-block` returns 0
when systemd accepts the job, not when the run ends (backlog d66f92b2).

Now: every git user of the checkout goes through
`infra/forge/checkout-lock.sh` — one `flock` on
`.git/boss-converge.lock`, a stale-`index.lock` sweep that removes the
file only when `/proc` shows no live `git` in the checkout (and logs
the removal), and a 5 × 6 s retry on the two lock errors; any other git
error is a verdict and is not retried. The symptom to read in
`journal-tail cluster-deploy-runner` is a `checkout-lock:` line — `hit
a git lock (try N of 5)`, `removed stale …/index.lock`, or `could not
take … within 600s`, the last meaning something outside the protocol
holds the checkout (a hand-run git; `unit-status forge-converge` and
the journal say which). And a run that a `converge` ops-request
started now ends on that packet: `converge_failed: <stage> (exit N)`
on the request's metadata, beside the maintenance packet's
*Maintenance failed*. A request whose run was already active when it
arrived is answered by the next run, usually `converged: <sha>`.

## Residue (measured 2026-09-05)

- `systemd-journal-gatewayd` is hand-installed and not in the tree at
  all. A host rebuild loses the read door — the one door that works
  when the API is dark. One car: the socket unit in the tree, installed
  by `install.sh`.
- The ops runner's residue closed on 2026-09-05 (`install.sh` lands it
  from `infra/ops` with a drop-in for this host).

## Related

- `infra/forge/install.sh` — the installer and its `UNITS` list.
- `infra/forge/disk-floor-sweep.sh` — the one definition of a bounded
  reclaim; `infra/ops/verbs.json` — the ops verbs.
- `docs/runbooks/operator.md` — the cluster-side runbook.
- CLAUDE.md §Diagnosis — what a stopped pipeline owes you.
