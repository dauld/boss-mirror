# Design: protocol cadence — the clock coordinates, systemd supervises

**Status**: decided — all 4 questions resolved 2026-08-14, two folds superseded by David 2026-08-15; see Decisions.
**Origin:** David, 2026-08-12 (verbatim, `bacca14e`): "We should be
using dispatcher to coordinate the conductor as well rather than
systemd. We want every protocol internalized so we can measure,
experiment, and update."
**Related**: [protocol-policy-publish.md](./protocol-policy-publish.md) —
revises its "timers never migrate" boundary ·
[internal-forge.md](./internal-forge.md) — supersedes half of Q6's
resolution, with its objection answered ·
[job-packet-network.md](./job-packet-network.md) ·
the clock-as-service rule, documented in the header of
`infra/lint/no-wallclock.sh` (no doc was ever written for it)

## The claim

A protocol's cadence — when its windows open, how often its
reconciliation runs — is part of the protocol, and today it is the
one part living outside the system: in systemd timer units, on a
box, invisible to the log, changeable only by an operator with sudo.
Internalizing it means three things his sentence names exactly:

- **measure** — every window-open and every reconcile tick is an
  event in the log, so "how often does the train actually run" and
  "what did the cadence cost" are queries, not folklore;
- **experiment** — cadence is a dispatcher rule row, edited through
  the rules API and hot-reloaded by the existing 30-second
  supervision (`1e576baf`), so trying a 3×-daily train is a data
  change with an audit trail;
- **update** — no unit files, no daemon-reload, no drift between
  boxes; the cadence deploys with the registry like every other
  protocol change (and under 3P, a protocol edit already is a
  network configuration change).

systemd is not deleted; it is demoted to what an OS is for —
**keeping processes alive**. Coordination of work belongs to BOSS.

## The maintenance-family objection, answered

internal-forge Q6 chose raw timers deliberately: "the dispatcher's
schedule runner fires on SIM-day boundaries, and at warp a daily
rule fires every couple of wall-minutes. Maintenance is wall-clock
work." That was an objection to the runner's *time basis*, not to
internalized cadence. The answer is to make the basis explicit:
a cadence rule declares `basis: wall | clock`. Wall-basis rules fire
from wall time regardless of warp (backups, certificate renewals,
the train's twice-daily); clock-basis rules keep today's sim-day
semantics (the brewery's daily cycles). With the basis field, the
maintenance family's timers migrate too — same guarantee they moved
to systemd for, now as measurable data. This doc supersedes the
timer-as-spawner half of that resolution once the basis lands.

## The conductor as a subscribed executor

The dispatcher cannot exec a binary on another box, and should not
learn to. The conductor becomes what every other actor already is:
**an executor with a queue**. A cadence rule emits the window packet
(`train.window.opened`, payload naming the window); the conductor —
`boss train serve`, a durable consumer exactly like the dispatcher's
own JetStream loop — claims it and runs the phases it already owns.
systemd keeps `boss-train.service` alive (a simple long-running
unit, no timers); the OS supervises the process, BOSS coordinates
the work. Reconcile ticks ride the same shape at their own cadence.
Every phase the conductor completes is already Job/step data; with
the trigger internalized, the train protocol is measurable
end-to-end: cadence → window → board → CI → merge → arrivals.

## Sequencing

Strictly after the Rust conductor lands (`26d61c97`) and never under
a moving train: (1) the cadence-rule schema + basis field + runner
support; (2) `boss train serve` consuming window packets, proven in
shadow against the timers; (3) the timers delete, systemd unit goes
long-running; (4) the maintenance family migrates onto wall-basis
rows, retiring its ExecStartPre wrapper pattern.

Steps 1–3 shipped between 2026-08-12 and 08-13, against the separate
`cadence_rules` table. The fold David chose on 08-15 re-sequences what
is left, and the order matters because the loop that would be migrated
is the one scheduling its own migration:

1. **`[[cadence]]` in `rules.toml`, read-only.** Add the shape and
   parse it; the loop still obeys `cadence_rules`. Nothing changes
   behaviourally, so this cannot break a window.
2. **Serve cadence from the jobs API**, the surface car `c083a38e`
   already began (`/api/cadence/*`). The loop keeps its sqlx pool.
3. **Switch the loop to the API and delete the pool.** This is where
   the split-brain dies: one reader, one registry, and the database
   the operator reads becomes the database the loop obeys. Do it
   between windows, never with a train in flight. *Shipped 2026-08-31
   (backlog a516f1f1): the pool had been recording every firing in a
   database the last-firing surface never reads, so
   `/api/cadence/rules/{name}/last-firing` answered `null` for every
   rule while the loop fired on schedule. The loop now refuses to
   start without `BOSS_JOBS_URL` — a local default here is the same
   silent redirect the maintenance wrappers already paid for.*
4. **Retire `cadence_rules`** once no reader remains, and fold its
   rows in as `[[cadence]]` entries under the same ratchet as
   `[[rule]]`.
5. **Then Q4's claim CAS**, which is a separate change and strictly
   later: a window packet claimed like a step only makes sense once
   the window is emitted from the registry the dispatcher serves.

The trap to avoid is doing 4 before 3: deleting the table while the
loop still reads it stops every train at once, and the loop is what
would otherwise fire the boarding that ships the fix.

## Open questions

All 4 open questions were resolved 2026-08-14 via the in-app
decision tracker and flushed to git. See the Decisions
section below. This section is kept empty as the landing
place for any new questions that surface during
implementation.

---


## Decisions

> **Q1 and Q4 were answered twice, and the later answer stands.**
>
> Two reviews of this doc were open at once — `d23998f7` (2026-08-12)
> and `47f3c3d2` (2026-08-14) — and they resolved the same questions
> in opposite directions. The 08-12 review RATIFIED WHAT SHIPPED: a
> separate `cadence_rules` table, and `cadence_firings` primary-key
> dedupe instead of the claim CAS. The 08-14 review ACCEPTED THE
> ORIGINAL PROPOSALS. The entries below are the 08-12 ones, recorded
> first because that review's flush landed first.
>
> David, 2026-08-15, asked directly: **"The 08-14 answer stands, fold
> cadence into dispatcher rules."** So Q1 and Q4 below are SUPERSEDED,
> and the superseding text is under each. Q2 and Q3 agree in both
> reviews and stand as written.
>
> Recorded rather than rewritten, because which answer won is itself
> the interesting fact — and because a doc that silently showed only
> the winner would hide that the tracker allowed two concurrent
> reviews of one document to produce contradictory records. That gap
> is filed separately; it is not fixed by editing this file.

### Q4: Does the conductor's queue use the claim CAS? (resolved)

Resolved 2026-08-14 — override.

**The question was:**

> Proposed: yes — window packets are ordinary packets; `boss train
> serve` claims via the same CAS the human queues use, which gives the
> board's live dot its data and makes a second conductor instance safe
> by construction rather than by deployment discipline.

cadence_firings primary-key dedupe

**SUPERSEDED 2026-08-15 (David): yes, the claim CAS.** Window packets
are ordinary packets and `boss train serve` claims one the way an actor
claims a step. Primary-key dedupe makes a firing *unique*; it does not
make the CLAIM and the WORK the same act, which is the property that
matters. Today a firing row is claimed BEFORE the verb runs and the
conductor's flock decides afterwards whether the work happens — so a
verb that loses the lock consumes its window and leaves silently. That
is exactly how the twice-daily boarding window came to fire for months
without ever boarding a train (`4ed0e791`). A claim that happens where
the work happens cannot be spent by a verb that never ran, and it makes
a second conductor safe by construction rather than by deployment
discipline.


### Q1: What is the cadence row's shape? (resolved)

Resolved 2026-08-14 — override.

**The question was:**

> Proposed: dispatcher rules grow a `[[cadence]]` sibling: `name`,
> `basis: wall|clock`, `every` (interval) or `at` (times-of-day),
> `emit` (topic + payload template). Same table, same hot-reload, same
> ratchet posture as `[[rule]]` — and the departure board's "next
> departure" line reads it as data.

separate cadence_rule table

**SUPERSEDED 2026-08-15 (David): fold cadence into the dispatcher
rules.** A `[[cadence]]` sibling of `[[rule]]` — same table, same
hot-reload, same ratchet posture, and the departure board reads the
next departure as data.

Why the later answer is the better one, in evidence gathered after the
first: the separate table is not merely a second shape, it is a second
DATABASE. `boss train cadence` reads `cadence_rules` through its own
sqlx pool pointed at boss-gcp's local Postgres, while the packets it
schedules live on the cluster. Measured 2026-08-14: cluster
`cadence_firings` held 0 rows against 244 locally, and the cluster
registry said the boarding threshold was 4 while the running loop used
8 — so an agent read the system of record, concluded a four-car dock
would board, and told David so. It did not. Folding cadence into the
registry the dispatcher already serves removes the second pool, and
with it the whole class: there is no longer a copy for the operator to
read that the runtime does not obey
(`protocol-data-agrees-between-record-and-runtime`).

The migration is not free and should be sequenced deliberately: the
rows exist in two places today, and the conductor is the only reader.


### Q2: Where does the wall-basis tick come from? (resolved)

Resolved 2026-08-14 — override.

**The question was:**

> The schedule runner drives off the clock service's tick stream,
> which under warp compresses days. Proposed: the clock service
> already knows both times (`ClockNow` carries wall and sim); the
> cadence runner evaluates wall-basis rows against wall time from the
> same feed — one clock service remains authoritative for both bases,
> and no component grows a second time source.

shipped as proposed


### Q3: Exactly-once windows across restarts? (resolved)

Resolved 2026-08-14 — override.

**The question was:**

> A cadence firing must not double-emit after a dispatcher restart nor
> skip a window that elapsed while down. Proposed: each firing records
> its event with a deterministic id (`cadence:<name>:<window-stamp>`),
> the outbox dedupes on it, and catch-up on start emits at most the
> single most-recent missed window per rule — a deliberate "no
> thundering backfill" choice matching the conductor's own
> one-window-at-a-time cadence.

shipped verbatim
