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
| journal gateway | `systemd-journal-gatewayd.socket` on `:19531`, enabled by `install.sh` | the read door: any unit's journal over HTTP from the pod, plus anything a script logs with `\| systemd-cat -t <tag>`. **Read it through `infra/forge/journal-read.sh`, never raw curl** — see below | the distro's units (`systemd-journal-remote`); converged since 2026-09-10 |
| the `boss` CLI | `/usr/local/bin/boss`, a link to the wrapper in `/opt/boss-cli`, installed by `install.sh` for the `cluster-operator` role from the cluster image built for the converged commit (`infra/estate/install-cli-from-image.sh`, since 2026-09-18) | the tree's own CLI on this host — `boss --version` names the converged commit; the packet records `cli_sha` beside `converge_sha`. The first converge tick after a train usually finds no image yet (`cli_result: not yet`) and installs it on the next | one directory per generation under `/opt/boss-cli`, newest 3 kept; nothing else on the host shells to it yet (the forge's shell twins of CLI verbs retire onto it one car at a time, backlog 9f00a805) |

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
| `disk-floor-sweep` | hourly (boot +5) | two passes. **Every** run prunes the **system** daemon's per-train `boss-ci:<sha>` images older than `BOSS_CI_IMAGE_AGE_HOURS` (6h), keeping the newest 3 — only sha-shaped tags, so `rust1.96` and `latest` are never candidates. **Below** `BOSS_DISK_FLOOR_GB` (100 in the service) it goes on to the emergency remediations: all unused system-daemon images over 4h, the whole rootless builder cache, dangling images, registry-verified old tags — in that fixed order, stopping at the floor; regenerable caches only, never volumes | exits non-zero with `FLOOR UNMET — a human decides next` rather than deleting harder, and non-zero when the age pass could not reach the system daemon (a prune of the wrong daemon would report success and free nothing) |
| `reap-dead-ci-jobs` | daily (boot +15) | remove the containers and volumes of crashed CI jobs | journal |
| `estate-observe-host` | 15 min (boot +3) | record this host's disk, load and units into the estate as observations; the conductor's boarding refuses on a positive "host is short" reading | journal; a stale series reads as unverifiable, and boarding proceeds with one loud line |
| `boss-ops-runner` | ~1 min | answer `ops-request` packets filed against `forge` with a verb from `infra/ops/verbs/` (one file per verb) | `refused` outcome on the packet; installed by `install.sh` since 2026-09-05 (a drop-in carries this host's identity) |
| `systemd-journal-gatewayd` | socket-activated, no timer | serve this host's journal over HTTP on `:19531` — the read door the pod uses when there is no ssh and the API is dark | **it does not fail loudly, and that is the point of `journal-read.sh`.** The distro's units, enabled (never copied) by `install.sh`; if the package is absent the installer says so and carries on, because a visibility door must not be able to abort the converge. It has **no periodic restart**: bounding the process with `RuntimeMaxSec=` would leave the unit `failed` after every expiry, and `infra/estate/observe-units.sh` reads `ActiveState=failed` as unhealthy — an hourly red nobody reads is the same defect as no check at all (CLAUDE.md §Diagnosis). When it wedges, `journal-read.sh` refuses and prints `systemctl restart systemd-journal-gatewayd.service` |
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

- **The journal gateway — through `infra/forge/journal-read.sh`.**

      infra/forge/journal-read.sh _SYSTEMD_UNIT=disk-floor-sweep.service
      infra/forge/journal-read.sh SYSLOG_IDENTIFIER=<tag> --count 200
      infra/forge/journal-read.sh --check        # freshness only

  It states how far behind the door is before it reads anything, and
  **refuses (exit 4) past 30 minutes**, naming both timestamps. Zero
  rows from a fresh door is then a real "nothing to report" and it says
  so, with the retained window, so "rotated out" stays distinguishable
  from "never logged". An unreachable or wrong target skips loudly
  (exit 3) and never passes.

  **Why not raw curl.** On 2026-09-10 the gateway served a journal whose
  newest entry was seven hours old while answering 200 to everything, so
  a unit-filtered query for `disk-floor-sweep.service` — which had run —
  came back empty, and was one report away from becoming "the forge disk
  sweep is not running" (packet 8bea0c9c). `GET /machine` → 200, the
  reachability check this file used to document, does not detect that at
  all. The raw endpoints still work
  (`http://10.20.0.15:19531/entries?_SYSTEMD_UNIT=<unit>`,
  `?SYSLOG_IDENTIFIER=<tag>`, `/fields/_SYSTEMD_UNIT` for every unit that
  has ever logged, with `Accept: application/json`) — they just answer
  without telling you whether the answer is current.

  **When it refuses,** the `journal-tail` ops verb below reads the same
  journal with local `journalctl` on the host and is unaffected by the
  gateway; that is the independent path, and it is what the refusal
  points you at.
- **An ops-request packet.** `boss job file --kind ops-request
  --metadata '{"host":"forge","verb":"df"}'`; the runner answers within
  about a minute with the output on the packet's `execute` step. Verbs:
  `df`, `uptime`, `timer-list`, `unit-status <unit>`, `journal-tail
  <unit> [n]`, `disk-report` (what is consuming disk — both daemons,
  Forgejo's data, the checkout), `reach <ipv4> <port>` (one TCP
  connect from this host's vantage — the WireGuard overlay and the
  LAN the pod cannot route to; nothing sent), and the mutating verbs, each
  authorized by name in `infra/ops/verbs/reclaim-disk.json`: `reclaim-disk <floor>` (the
  sweep, with a floor), `rollback-to <sha>` (roll deploy/boss to a
  named build, verified Ready), `hold-converge <reason>` and
  `release-converge` (the runner builds and rolls nothing while a
  hold stands), `mirror-base-images`, `delete-orphan-object
  <Kind>/<namespace>/<name> [--dry-run]` (delete one cluster object the
  TREE ALREADY PROVES is undeclared — the apply does not prune, so a
  deleted manifest leaves its object running; there is deliberately no
  general `kubectl delete` verb, and this one's authority is DERIVED: it
  re-runs `infra/cluster/undeclared-objects.sh` at call time and acts
  only on an object that computation names, refuses unless the manifests
  directory is clean in git, withholds the kinds whose deletion destroys
  bytes, credentials or privileges, and prints the object's YAML onto
  the packet before deleting it; `--dry-run` runs every bound and
  deletes nothing), and `publish-github-pr` (the
  machine step of publish-to-github v6: snapshot forge main onto the
  public mirror as a PR from the dauld fork; reads the dauld token at
  `/etc/boss-publish/github.token`, credentials registry
  `dauld-github-token`, and refuses loudly without it; `--check`
  validates its inputs with no network), `read-publish-checks` (the
  second machine step of publish-to-github v7: waits for the mirror
  PR's check-runs over the PUBLIC API — no token — reads the CodeQL
  annotations and writes the reading onto the publish packet as
  `code_scanning`, so the `judge-checks` step and David's merge follow a
  judged reading instead of a red badge; the wait blocks this runner
  for up to 25 minutes once per publish, stated in the script header;
  `--check` validates with no network), and `run-car-probe <car-uuid>`
  (the machine half of `boss prove`: runs the probe a landed car
  recorded at park time — `boss gate --park-probe/--park-expect` — as
  david, never root, and completes the car's `proven` step with the
  proof record or stamps `proof_attempt`; filed per car by the
  dispatcher when its train arrives). **The probe was written on the
  dev pod and runs HERE**, in `/home/david/boss` with this host's
  tools: no kubectl, no kubeconfig, the cluster only over HTTP. A
  probe that needs a tool this host lacks is refused at `boss gate`
  against `infra/forge/host-absent-tools.txt`; one that slips through
  is recorded as `proof_attempt.unrunnable` with the tool named and
  exits 3, so "cannot run here" never reads as "the claim is
  false" (f9304366). The verb list is the tree's,
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

## Planned downtime

Every section below this one is about a failure. This one is about
choosing to power the host off — first written 2026-09-20, the night
before an inspection for a possible second disk, when the question
"what actually breaks if we switch it off" had no written answer and
had to be re-derived from the manifests.

**What keeps running.** Everything already running in the cluster:
`postgres-0`, `nats-0`, the app pod, the dispatcher. The system of
record stays up, and an operator can keep reading and writing packets
through the whole window. That is the reassurance worth stating first,
because the host's centrality invites the opposite assumption.

**What stops.** Trains, gates, CI, every converge and deploy, and every
image pull in the estate — 13 manifests name `10.20.0.15:3000`, and
`postgres` is one of them. Its manifest says why in its own comment: *a
database that cannot re-pull its image during a flake cannot restart.*
So the rule for the window is not "avoid the cluster" but something
sharper:

> While this host is down, any cluster pod that stops will not come
> back until it returns. Running pods are safe; restarts are not.

That makes a planned forge window incompatible with a planned cluster
window, in either order. Do not take a node down to fill the wait.

**Two arms go down with the host, and both are easy to forget.**

- `cluster-watchdog.timer` runs *here*, every 5 minutes. It is the loop
  that reads the cluster from outside and rolls it to the last
  converged build when it is dark — deliberately built to owe nothing
  to the API it watches (2026-09-05). It does still owe everything to
  this host being powered on. For the length of the window the cluster
  has no outside-in recovery arm.
- **This host is the only `cluster-operator` in the estate.** The role
  holds `talosctl`, `kubectl` and the cluster credentials under
  `/etc/boss-ops`. `boss-gcp` is an `ops-runner` but not a
  `cluster-operator`, so with this host off there is no supported path
  to the cluster's control plane at all. `boss ops forge …` is gone
  too — the ops-runner is here.

Neither is an argument against the window. Both are arguments for
keeping it short, starting it from a healthy cluster, and knowing
before the power comes off whether any other machine can reach the
cluster API if it is needed.

**Before the power comes off**

1. Read the cluster's health and only start from a green one — a window
   with no watchdog is the wrong time to discover a sick node.
2. Confirm no train is in flight and no `gate-run` is open, then hold
   the conductor so none starts into the window.
3. Confirm the nightly backup's three legs (the Longhorn PVC,
   `boss-gcp:/var/backups/boss-cluster-pg`, GCS). Nothing in this
   window should touch the database, which is exactly why an untested
   backup should not be discovered afterwards.
4. Name the last converged build by digest, so any rollback after the
   window has a target by name rather than "the previous one".
5. Settle whether another machine holds a working kubeconfig. If none
   does, that is a known and accepted gap for the window, not a
   surprise during it.

**On the way back up**

Verify in this order, because each one depends on the last: the host
boots and `df /` is what you expect → Forgejo answers on `:3000` → the
registry serves a real pull → `forgejo-runner.service` is polling →
`cluster-watchdog.timer` and `cluster-deploy-runner.timer` are active
again → release the conductor. A window is over when a pull and a gate
have both succeeded, not when the box is pingable.

**If the reason for the window was disk**, read `disk-report` before
and after and record both numbers — and read the RIGHT number, which
is not the percentage. This section was first written against
"99G used of 228G, 46%, `verdict: clean`" and called the trip
headroom rather than repair. That reading was wrong, and the sweep's
own journal is what corrects it.

**The operating constraint is the 100 GB floor, not the percentage.**
Measured 2026-09-20, hourly, over consecutive runs:

```
disk-floor-sweep: CI-image prune freed 1210MiB on / (now 118GB free)
disk-floor-sweep: 118GB free >= 100GB floor — nothing to do
disk-floor-sweep: CI-image prune freed 1210MiB on / (now 116GB free)
disk-floor-sweep: 116GB free >= 100GB floor — nothing to do
```

Free space oscillates between 116 and 118 GB, so the margin above the
floor is **16–18 GB**, and the sweep holds that line by pruning one
per-train CI image (~1.2 GB, of 3.48 GB each) on every pass. A cold CI
job needs 70 GB free to start and consumes about 74 GB: **one fits,
two concurrent do not.** A host at 46% that can run one build at a
time is not a host with room to spare, and "46%" is exactly the
number that makes it look like one.

**What the record does and does not say.** Every `pr-train` packet in
the system of record — 98 of 98 — carries no locomotive disk refusal.
That window is four days (2026-09-17 onward), because that is all the
trains the record holds; it is NOT evidence that the 2026-09-10
retention fix retired the problem, and the backlog's own note
(2026-09-19) says the floor was being hit three times a month. Four
quiet days is four quiet days.

So: measure before concluding the host needs hardware, and measure
against the floor.

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
CI images (81 GB on 2026-09-05 — since 2026-09-10 pruned by AGE on
every hourly pass, not only below the floor), the rootless daemon's converge
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

- The journal gateway's residue closed on 2026-09-10 (`install.sh`
  enables `systemd-journal-gatewayd.socket`, installing the
  `systemd-journal-remote` package if a rebuild has left it absent, and
  `journal-read.sh` is the door that states its own freshness — packet
  8bea0c9c). Since 2026-09-11 the *how* is `infra/journal-door-ensure.sh`
  — one definition, called by this installer and by
  `install-units.sh units` on boss-gcp, because that host needed the same door
  (backlog 68757702) and a second copy is what drifts. `journal-read.sh`
  now takes `--host boss-gcp` as a named target.
- The ops runner's residue closed on 2026-09-05 (`install.sh` lands it
  from `infra/ops` with a drop-in for this host).
- **Not established:** why the gateway went seven hours stale on
  2026-09-10 while still answering 200. Measured from the pod: journald
  was healthy and the on-disk journal continuous through the window (264
  cron entries between 09-09 19:00 and 09-10 06:00 UTC, no gap over 10
  min in the 7 days retained); journald logged no rotation and no restart
  between 09-05 21:11 and the measurement, so "a reader holding a rotated
  file" is not supported by journald's own record; and the gateway has
  been the same process since its one `Started` at 2026-09-03 23:10:06,
  recovering without a restart. A transient fault inside a long-lived
  reader. Pinning the mechanism needs `lsof` or `ls -l /proc/<pid>/fd` on
  the host while it is stale — the pod has no ssh here, so it is a human
  step, and the refusal is what makes the next occurrence safe to ignore
  until someone can take it.

## Related

- `infra/forge/install.sh` — the installer and its `UNITS` list.
- `infra/forge/journal-read.sh` — the journal door, fronted by the
  freshness assertion; `infra/lint/the-journal-door-states-its-freshness.sh`
  proves its three postures and that the installer still enables the socket.
- `infra/forge/disk-floor-sweep.sh` — the one definition of a bounded
  reclaim; `infra/ops/verbs/` — the ops verbs, one file each.
- `docs/runbooks/operator.md` — the cluster-side runbook.
- CLAUDE.md §Diagnosis — what a stopped pipeline owes you.
