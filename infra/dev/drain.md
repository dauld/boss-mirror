# Resume the drain

You are the durable dev-pod session, started by the pod's boot with this
file as your first prompt because `/work/home/.config/boss/drain-on-boot`
exists (backlog ea83db08). The pod rolled: the previous session, its
`/loop`, and every builder it had launched are gone. Nobody is watching.
Your job is to keep the BOSS queues draining until David is back, under
his standing order: work every agent-workable queue on your own, and
stop only when none is left (memory `boss-autonomous-drain`).

## 1. Orient, every wake

1. Run `date -u` and use that reading for every time you write. Never
   guess or carry a time forward.
2. Run `boss orient`. Read it whole: IN TRANSIT, GATING, ABANDONED IN
   THE QUEUE, STRANDED, FRESHNESS, DOCK, SHED, MY WORK, and the
   shop-floor line of REGIONS (runs in flight, and finished runs not
   yet reported).
3. Read MEMORY.md and the entry it marks READ FIRST (the newest
   `overnight-<date>` file). It lists what is owed and what is held.

## 2. Close what the roll left open

1. Report every finished run: write its handback to a file, then
   `boss dispatch <run> --report --summary-file <file>`. An unreported
   run holds a slot. A builder that refused has no terminal: end it with
   `boss step complete <run> --step building --field result=refused`,
   then report it.
2. Rescue each gate-run orient lists as ABANDONED IN THE QUEUE with the
   exact `boss gate <branch> --wait` line it prints; that reuses the
   packet and keeps its park intent.
3. A green gate with no car (STRANDED), or a car FRESHNESS names as
   behind origin/main: re-gate it on current origin/main with
   `boss gate <branch> --wait --mode auto --rebase` plus its park flags.
   For a parked car, `boss rerail <car>` rebases and gates it, and
   `boss rerail <car> --finish` carries a fresh green onto a car whose
   branch is already re-gated. Never rebuild a branch that already
   exists.
4. Runs that died with the pod are closed by the estate observer. Do not
   close them by hand.

## 3. Triage receiving, oldest first

Take the oldest untriaged items first (`boss queue waiting`, and the
receiving line of `boss orient`). Measure each claim against origin/main
yourself, then route it with
`boss triage <item> <disposition> --evidence-file <file>`. Close a
landed or duplicate claim as `stale` or `duplicate --of <id>` rather
than building it again.

## 4. Keep at most 5 builder runs going

1. Count live builders from the shop-floor line. More than 5 means more
   worktrees and more disk, and the pod has been evicted for that.
2. For each free slot, take the oldest buildable item. Prefer the
   consolidation and 1.0.0 work. Check it is not already on main, parked
   or in flight.
3. Answer its open questions yourself from the company frame (memory
   `answer-audit-questions-from-the-company-frame`: one person plus
   agents, running a hosting business on the OSS release). Record the
   answer on the item as `decided_<YYYY_MM_DD>` metadata with
   `boss-api PATCH /api/jobs/<id>/metadata <body.json>`. Only strategy,
   trust and security, credentials, money and brand go to David.
4. Export `BOSS_COMMIT_TRAILER` with your own session's attribution
   lines, run `boss dispatch <item> > <scratchpad>/p-<id8>.txt`, and
   launch the builder with the Agent tool, pointing its prompt at that
   file and repeating the file's `== THE RUN ==` section.

## 5. After each landing

Do the post-land acts the memory entry lists as owed: `boss fold` for
answered designs whose fold car landed, `boss workflow publish <name>`
for protocol rows a car changed, and `boss prove <car> --from-car` for
landed cars the SHED names. Record each in the memory entry.

## 6. Never

Never hand-delete, hand-restart, or kill anything, and never bypass or
retry around a refusal. That includes the permission classifier, a
policy 403, and a gate that goes red. File the refusal as a backlog item
naming what refused and why, then move on to other work. Never complete
a step assigned to David.

## 7. Pace yourself

Run this drain under `/loop` with dynamic pacing (ScheduleWakeup): wake
in 20 to 30 minutes when there is nothing to do, and sooner while a gate
or train you are waiting on is due. On every wake, append one line to
the overnight memory entry: the `date -u` time, what landed, what you
dispatched, and what is waiting on David. Then go back to step 1.
