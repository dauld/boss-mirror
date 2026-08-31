# Design: the internal forge — Git, CI, and maintenance come inside

**Status**: decided — all 7 questions resolved 2026-08-12 via the in-app tracker; see Decisions.
**Origin:** David, 2026-08-10 (`4bff901a`): "We are going to
internalize Git and CI" — plus, in the same breath: "develop a
better sense of the building up of commits into PRs pushed onto
trains, deployed periodically … I think we also have maintenance
missing here."
**Related**: [dev-cluster.md](./dev-cluster.md) (declared this as
the after-runners direction) · [idm-kanidm.md](./idm-kanidm.md) ·
[stations.md](./stations.md) — the forge and its runners become
instrumented stations

## What internalizing buys

- **The merge wall becomes policy.** Today merge is a GitHub admin
  bit no BOSS rule can see (the no-oversight test hit it
  structurally). Internal: the ship-a-change `review` step IS the
  review — the operator's sign-off completes it, the conductor
  executes the merge through the forge API, and the review budget
  becomes a policy row instead of a foreign platform's permission.
- **The external station comes inside.** On the activity network,
  GitHub/CI is the dashed station where packets vanish into
  off-department time. The forge and runners become instrumented
  stations; trains never leave the map.
- **Kanidm pays twice**: the forge fronts with the same OIDC door;
  agent service accounts (idm Q3, phase 2) are what let an agent
  merge under BOSS policy rather than a borrowed human credential.

## The pipeline, grained

David's "building up" ask, modeled: **commits accrete into a
change; changes board a train; trains deploy periodically.** Each
grain is already half-present — ship-a-change Jobs carry branches,
pr-train Jobs carry consists, the reconcile stamps deploys — but
the accretion itself is invisible: nothing shows a change GROWING
(commits landing on its branch), a train FILLING (candidates
parking), a deploy WINDOW approaching. Internal CI gives us the
events to model all three (the forge emits push/check/merge webhooks
we can land on the outbox), so the canvas can show the dev pipeline
as nested packets: commit-dots accreting onto a change-packet,
change-packets boarding the train-packet, the train departing on
schedule.

## Maintenance is missing

The department's recurring labor — backup, audit-integrity,
ledger-replay checks, views catchup, files GC, message purge, the
reconcile itself, certbot renewals, and soon the flush worker — runs
as systemd timers OUTSIDE the Job model. Invisible work, in a system
whose thesis is that work is visible. The dispatcher's schedule
runner already fires clock-driven rules; a `maintenance` Workflow
family (spawned on cadence, auto-executed, self-closing, loud on
failure) would put every chore in the log, on the canvas, and in the
stage-duration numbers — and a failed backup becomes an algedonic
signal instead of a quiet journal line.

## Open questions

All 7 open questions were resolved 2026-08-12 via the in-app
decision tracker and flushed to git. See the Decisions
section below. This section is kept empty as the landing
place for any new questions that surface during
implementation.

---


## Decisions

### Q1: Forgejo, and where does it live? (resolved)

Resolved 2026-08-12 — accept.

Forgejo — decided de facto and running (<forge-host>:3000: git, the OCI registry the CI image pulls from, and a registered Actions runner; fa7191b is the adoption act). Placement in two stages: the interim LAN box is legitimate for CI-shadowing, and its Forgejo data dir enters the backup set now — until then the GitHub mirror is its only off-host copy. Cluster placement lands per dev-cluster topology, now schedulable: the cluster is up and running BOSS, Kanidm, and Longhorn.

**Rationale:** David approved the worked recommendations 2026-08-11 (evidence-grounded decision sheet); recorded by claude:fable.


### Q2: CI engine? (resolved)

Resolved 2026-08-12 — accept.

Forgejo Actions — de facto (fa7191b, c4361cd, e6d612f: GitHub-Actions compatibility held with exactly two container-job deltas). Two debts close before cutover makes them load-bearing: the Forgejo jobs invoke infra/gate.sh and gate_sh.rs extends to pin .forgejo/workflows/ci.yml the same way it pins the GitHub side (the workflow currently inlines a second gate definition); and the boss-ci image Dockerfile comes into the repo, reconciled with infra/cluster/builder/Dockerfile — one rustc truth. The web job ports when a bun-bearing image exists; CodeQL and Scorecard stay on the GitHub mirror where they are native and free.

**Rationale:** David approved the worked recommendations 2026-08-11 (evidence-grounded decision sheet); recorded by claude:fable.


### Q3: What exactly is the internal review protocol? (resolved)

Resolved 2026-08-12 — accept.

The ship-a-change review step gains sign_offs_required: [operator] — a versioned registry change; in-flight Jobs stay pinned. The conductor's reconcile, on seeing a signed-off review, calls the forge adapter's new merge verb and then sets the merged marker exactly as today: the observe-then-mark shape survives, the observer becomes the executor. No bespoke review UI first — the PR page becomes a diff viewer. Sequenced behind Q7's rehearsal: a broken merge path stops all shipping.

**Rationale:** David approved the worked recommendations 2026-08-11 (evidence-grounded decision sheet); recorded by claude:fable.


### Q4: Mirror shape? (resolved)

Resolved 2026-08-12 — accept.

Forgejo-to-GitHub push-mirror on every main update, superseding dev-cluster's 'daily'. The mirror is a disaster-recovery artifact of the system of record — a day-stale copy is a day of lost commits — and the GitHub-native checks (CodeQL, Scorecard) only audit what the mirror shows them. (install-smoke was in that list, but it is not currently a live automated check: the forge copy has been workflow_dispatch-only since 2026-08-18 — its compose run took the CI runner's network down — and the mirror copy's guardianship is not current, since the mirror only advances when a publish is pushed.) Inbound stays deliberate-pull via GitHub PRs; the fork model keeps external code off internal runners.

**Rationale:** David approved the worked recommendations 2026-08-11 (evidence-grounded decision sheet); recorded by claude:fable.


### Q5: Do forge events land on the outbox? (resolved)

Resolved 2026-08-12 — accept.

Yes. A small ingress validates Forgejo's webhook secret and stages via record_event_in_tx — never post-commit publish; the ratchet already bans the alternative. forge.push, forge.check.completed, and forge.merge are born declared in the event-kind registry as it lands. The conductor's reconcile may later consume these instead of polling; polling stays until then. Converges with the infrastructure-as-Subjects item (8016504c): forge events get a subject to be about.

**Rationale:** David approved the worked recommendations 2026-08-11 (evidence-grounded decision sheet); recorded by claude:fable.


### Q6: What is the maintenance Workflow family? (resolved)

Resolved 2026-08-12 — override.

Resolved by the shipped implementation (train #226): maintenance_spec() kinds plus the ExecStartPre wrapper; success completes the run step, failure completes nothing and the Job stays open and loud, recovery closes the standing Job. One deliberate amendment to the doc's proposal, ratified: the spawner is the systemd timer's wrapper on wall-clock — not the dispatcher's schedule runner, whose sim-day rules fire every couple of wall-minutes at warp. Remaining rollout is mechanical: wrap the five uncovered timers (files-gc, ledger-recognize, messages-events-purge, search-reindex, views-catchup).

**Rationale:** David approved the worked recommendations 2026-08-11 (evidence-grounded decision sheet); recorded by claude:fable.


### Q7: Sequencing — what preps before the cluster is up? (resolved)

Resolved 2026-08-12 — override.

(a) the conductor's forge seam and (b) the maintenance family are done — landed via #227 and #226. (c) amends to the ladder reality already proved better than staging-first: 1) Forgejo CI shadows GitHub CI on the interim host — the shadow surfaced three real port bugs at zero cutover risk; 2) the forge's configuration comes into the repo infra/idm-style (install script, runner registration, the boss-ci Dockerfile) and its data dir into the backup set, so the cluster install is a re-run, not a reinvention; 3) the ForgejoForge adapter lands behind the existing seam, exercised with BOSS_TRAIN_FORGE=forgejo in --dry-run; 4) the git-host and review cutover (Q3, Q4) waits on a log-copy-style rehearsal on the now-live cluster.

**Rationale:** David approved the worked recommendations 2026-08-11 (evidence-grounded decision sheet); recorded by claude:fable.
