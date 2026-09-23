# The tier map — one registry, read three ways

`tiers.toml` beside this file is the ONE definition of which tier a
path in this tree belongs to. It exists because two ideas landed on the
backlog within an hour of each other on 2026-09-18, both from David,
both spelled "tier" — hosting safety levels (a479faf7: a hosted tenant's
IT department may edit data only, or its tenant tier, or modules, or
everything) and arrival channels (ba429e7f: split "software" arrivals
by tier, so "is the core settling while work moves outward" becomes a
series). Built alone, each would have decided for itself what a tier
is — a path prefix, a crate's declared tier, whether the frontend and
`infra/` are tiers — and the two definitions would have drifted the way
the four pairs in CLAUDE.md §9a drifted. Design 01c3cc3f (decided
2026-09-19) settles the fact once and lets every consumer read it.

## The shape

One `[[tier]]` row per tier, carrying:

- `name` — what a car, a train and `boss channels` call it;
- `rank` — 1 is the innermost. A change touching several tiers has
  the LOWEST rank as its headline (`software_tier`): a car that edits
  core and the frontend is a core car. Ranks tie on purpose (modules
  and orchestrators are both 2; tenants and frontend both 3; infra is
  1 beside core) — a tie is broken by row order, so the first row
  written at a rank is the headline among equals;
- `paths` — the prefixes the tier owns. The MOST SPECIFIC matching
  prefix wins: `infra/platform/tiers.toml` is data (prefix
  `infra/platform/`, 15 characters) rather than infra (`infra/`, 6),
  and `examples/brewery/seeds/x.sql` is data through `**/seeds/`
  (which matches a `seeds/` directory at any depth and is as specific
  as the whole prefix it consumed) rather than tenants. A path no row
  claims — a file at the tree's root, `.forgejo/` — is in NO tier, and
  a reader says so rather than guessing one.

`infra/` is listed so the map is total over the tree's directories,
not because a hosted tenant will ever edit it. `data` is a tier of
its own so the arrival series can show work moving to the outer
layer: a registry row, a rule file, a seed, a doc.

## The readers

1. **The lint.** `infra/lint/tier-import-audit.sh` reads the core
   root and the forbidden tiers from this file through
   `infra/lint/lib/tiers.sh` instead of its own text — a collapse,
   not a pin. Its verdict and its scanned line did not change.
2. **Arrivals** (ba429e7f, car 1 — this one). A car's changed paths →
   the SET of tiers it touched (`software_tiers`, sorted) with the
   lowest rank as the headline (`software_tier`), stamped on the
   gate-run and carried onto the car, beside `delivery_channel`.
   At arrival the train aggregates its cars (`software_tiers`, the
   union; `software_tier_counts`, cars per tier). `boss channels`
   prints the per-tier mix; `boss channels --backfill-tiers` reads
   each closed train's merge commit so the series has a past. The
   production view drawing the series is car 2. The series is READ
   every week at protocol-retro's `collect` step, whose required
   `tier_mix` field quotes `boss channels --tiers` (79fdc808): coverage
   first — how many landed trains carry a stamp, with every share
   withheld when that is not a majority — then the tiers, direction
   outward and no target ratio, by decision (design 32f18167). Beside
   it, the same step's required `core_changes` field quotes `boss
   channels --core-changes` (bd93d2be): the ABSOLUTE number of files
   in this map's `core` tier touched per day on origin/main, read
   from git, direction down, no target and no threshold — a share can
   be held flat while core work grows, a count cannot.
3. **Hosting levels** (a479faf7). A tenant's `edit_level` is a tier
   name — `[meta] edit_level` in its manifest (docs/tenant-contract.md;
   `data` is data-only, `tenants`, `modules`, `core` is everything) —
   and a change is admitted for that tenant's IT department iff every
   path it touches is in a tier of rank ≥ the level's rank; a path no
   row claims (the tree's root) only by a level at the innermost rank.
   ONE predicate, `TierMap::first_above` / `edit_level_first_above`,
   at BOTH doors: `boss dispatch` refuses a packet declaring
   `metadata.paths` above the level before the claim, and the gate's
   `a-car-stays-under-the-edit-level` lint refuses a car whose diff
   crosses it, naming the first path. Both read the level off the
   instance (`GET /api/tenant/edit-level` on the jobs API, which
   answers the manifest the launcher hands every service), never the
   checkout; an instance declaring none enforces nothing.

## The two loaders, and why there are two

`boss_core::tiers` embeds this file at build time (`include_str!`) and
parses it with a real TOML parser; every Rust reader — the arrival
classifier, later the dispatch door — goes through it. A shell lint
cannot call Rust, so `infra/lint/lib/tiers.sh` reads the same file
line by line, which is why the file keeps one key per line and
`paths` on one line. The two loaders are held equal by
`crates/core/boss-testing/tests/tiers_sh.rs`, which runs both over one
fixture of paths — the prefix-vs-glob case, the data-over-infra case,
the unclaimed case — and names the path where they disagree. That is
a pin, kept only because a shell cannot read the Rust; the file itself
is the one definition.
