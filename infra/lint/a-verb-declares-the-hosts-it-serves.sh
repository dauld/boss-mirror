#!/usr/bin/env bash
# a-verb-declares-the-hosts-it-serves.sh — every ops verb says which
# hosts it serves, every named host is a real one, and a verb scoped to
# a host can actually run there.
#
# WHY. The ops-request allowlist (infra/ops/verbs/, one file per verb) had no notion of
# a host: a runner executed any verb a packet named. That was harmless
# while exactly one host had a runner, and stopped being harmless on
# 2026-09-11 when boss-gcp got one (backlog c3d06016). Eleven of the
# sixteen verbs name a script under the FORGE's checkout
# (/home/david/boss/infra/forge/…), which does not exist on boss-gcp —
# so an unscoped allowlist advertised a sixteen-verb vocabulary of which
# eleven could only fail on ENOENT, and `converge` or `rollback-to`
# filed against the bastion would have recorded an exec failure instead
# of a refusal. An exec failure is not a verdict (CLAUDE.md §Diagnosis:
# a verdict must name what failed).
#
# WHAT IT CHECKS
#   1. every verb declares a non-empty `hosts` list — the runner refuses
#      when `hosts` omits its HOST_ID, absent included, so a verb that
#      forgets to say reaches nobody; this is what makes that a decision
#      rather than an oversight
#   2. every host named is a REAL estate node id, derived from the
#      `INSERT INTO nodes` rows in infra/postgres/schema (the same ids a
#      packet's metadata.host carries) — never a list typed here
#   3. a verb whose argv[0] is an absolute path lives in ONE host's
#      filesystem, and may be scoped only to that host. This is the
#      check that would have caught the whole class: the forge scripts
#      are reachable by path only on the forge.
#   4. NO MUTATING VERB SERVES boss-gcp unless it is ADMITTED BY NAME
#      below, with its authorization. The host was made answerable,
#      not powerful: its set is the read-only, host-agnostic reads, and
#      widening is a reviewed per-verb change with its own
#      authorization — the same process reclaim-disk / converge /
#      publish-github-pr each went through (infra/ops/verbs/README.md
#      §Authorization). A mutating verb appearing here silently would be that
#      process skipped; one appearing in GCP_MUTATING_ADMITTED is that
#      process having happened, and this file is where it is noticed.
#   5. at least one verb serves boss-gcp, or the runner there answers
#      nothing and this lint is green over a dead door.
#   6. the runner actually READS `hosts`, or the field is decoration.
#
# The sibling is infra/lint/the-controls-are-bounded-verbs.sh, which
# asks whether a MUTATING verb is bounded and authorized; this one asks
# WHERE a verb runs. Both derive their rosters from the allowlist rather
# than listing verbs (§9a) — and the allowlist itself is DERIVED from the
# directory infra/ops/verbs/ by the one script the runner uses
# (infra/ops/verbs-allowlist.sh, 5086842d), so this lint reads exactly
# what the runner reads, assembled the same way.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"

allowlist="$(sh "$repo/infra/ops/verbs-allowlist.sh" "$repo/infra/ops/verbs")" \
    || { echo "FAIL: infra/ops/verbs-allowlist.sh could not assemble infra/ops/verbs/ (see above)" >&2; exit 1; }

python3 - "$repo" "$allowlist" <<'PY' || exit 1
import json, os, re, sys, glob

repo = sys.argv[1]
verbs = json.loads(sys.argv[2])["verbs"]

# ---- the hosts that EXIST, derived from the estate registry's seeds.
# `nodes` is the estate registry's table; its id is what an ops-request
# packet carries as metadata.host and what a runner presents as
# HOST_ID. Reading the seeds keeps this lint correct when a node joins.
# Line-based on purpose: a row's `notes` text contains semicolons, so
# reading to the statement's `;` truncates the block (measured — it
# found three of seven ids). The needle is named `..._pattern` so
# api-path-bypass-smell.sh reads this file as MATCHING that SQL rather
# than running it — which is also what makes it one definition.
nodes_insert_pattern=r"\s*INSERT INTO nodes \(id[ ,]"
node_ids = set()
for path in sorted(glob.glob(f"{repo}/infra/postgres/schema/*.sql")):
    in_rows = False
    for line in open(path):
        if re.match(nodes_insert_pattern, line):
            in_rows = True
            continue
        if in_rows:
            if re.match(r"\s*(ON CONFLICT|INSERT|UPDATE|SELECT|--|$)", line):
                in_rows = False
                continue
            m = re.match(r"\s*\('([A-Za-z0-9._-]+)'\s*,", line)
            if m:
                node_ids.add(m.group(1))
if len(node_ids) < 5:
    sys.exit(f"FAIL: only {len(node_ids)} estate node id(s) derived from "
             f"infra/postgres/schema — the derivation broke, so every host check below "
             f"would be vacuous: {sorted(node_ids)}")

# MUTATING verbs boss-gcp may serve, each with the authorization that
# admitted it. Empty until 2026-09-14. An entry here is the review
# check 4 exists to force: the verb's own `about` must carry the same
# authorization (the-controls-are-bounded-verbs.sh checks for "David"),
# and this table says which verbs went through it.
GCP_MUTATING_ADMITTED = {
    # design 9e3e093f, decided by David 2026-09-11 ("Go ahead and retire
    # it quickly ... part of our tech debt payoff"); backlog d5941ef3
    # car 2. Bounded to infra/gcp/second-stack-units.txt, capture before
    # stop, --dry-run exercisable without acting.
    "retire-second-stack": "David 2026-09-11, design 9e3e093f",
    # backlog 3ce95b85, car 3a of 8f4e9cc0: David 2026-09-11 asked for
    # "some sort of doc diff view for me to approve"; this verb is the
    # publish that approve fires. Bounded to the checkout's own
    # infra/platform/workflows/<kind>.toml, refuses a live row the tree
    # never said unless --force-tree, --check exercisable without
    # acting, the read-back is the verdict. Flagged for David's review
    # as the first verb that writes the workflow registry from a host,
    # the way run-car-probe was flagged as the first to run builder text.
    "publish-workflow": "David 2026-09-11, 8f4e9cc0 / backlog 3ce95b85 — flagged for review",
    # design 9e3e093f, decided by David 2026-09-11 — its accepted
    # proposal names this path: "an uninstall path for units the role
    # does not name ... through the ops-runner door as a bounded verb";
    # backlog d5941ef3 car 4. Bounded to the installer's own roster
    # derivation (deploy-services.sh roster: TIMERS minus what the
    # host's LIVE roles name), refuses an empty set, --dry-run
    # exercisable without acting.
    "uninstall-not-in-role": "David 2026-09-11, design 9e3e093f",
}

problems = []
serving_gcp = []
for name in sorted(verbs):
    spec = verbs[name]
    hosts = spec.get("hosts")
    if not isinstance(hosts, list) or not hosts or not all(isinstance(h, str) and h for h in hosts):
        problems.append(
            f"{name} declares no usable `hosts` ({hosts!r}). Say which estate node ids "
            f"this verb serves; the runner refuses it everywhere until you do.")
        continue
    if len(set(hosts)) != len(hosts):
        problems.append(f"{name}.hosts repeats a host: {hosts}")
    for h in hosts:
        if h not in node_ids:
            problems.append(
                f"{name}.hosts names '{h}', which is not an estate node id. Known ids "
                f"(from the `nodes` seed rows in infra/postgres/schema): {', '.join(sorted(node_ids))}. "
                f"A runner matches metadata.host EXACTLY, so a host that does not exist is a "
                f"verb nobody can reach.")
    argv0 = spec["argv"][0]
    if argv0.startswith("/"):
        # An absolute argv[0] is ONE host's filesystem baked into a file
        # every host reads. Until 2026-09-12 eleven verbs carried the
        # forge checkout's path and could run nowhere else (66077f9c).
        # The runner resolves a repo-relative script against its own
        # checkout, so every managed host can carry every script.
        problems.append(
            f"{name}'s argv[0] is the absolute path {argv0}. Name the script relative to "
            f"the repo (infra/forge/reach.sh); the runner resolves it against its own "
            f"checkout, on whichever host runs it. A bare command stays a bare command.")
    elif "/" in argv0:
        script = os.path.join(repo, argv0)
        if not os.path.isfile(script):
            problems.append(
                f"{name}'s argv[0] {argv0} is not a file in this tree — the runner would refuse "
                f"it as 'not in this checkout' on every host.")
        elif not os.access(script, os.X_OK):
            problems.append(f"{name}'s argv[0] {argv0} is in the tree but not executable.")
    if "boss-gcp" in hosts:
        serving_gcp.append(name)
        if "MUTATING" in spec.get("about", "") and name not in GCP_MUTATING_ADMITTED:
            problems.append(
                f"{name} is MUTATING and scoped to boss-gcp. boss-gcp was made ANSWERABLE, not "
                f"powerful (c3d06016): its verbs are the read-only, host-agnostic reads. A "
                f"mutating verb there needs the same explicit per-verb authorization "
                f"reclaim-disk / converge / publish-github-pr each carry — and this line is "
                f"where that review is noticed, so adding one means saying so here too.")

if not serving_gcp:
    problems.append(
        "no verb serves boss-gcp. The host runs an ops-runner (deploy-services.sh units "
        "installs it) and would answer nothing — a door that opens onto a wall.")

if problems:
    print("FAIL: the ops allowlist's host scoping is wrong:", file=sys.stderr)
    for p in problems:
        print(f"  * {p}", file=sys.stderr)
    sys.exit(1)

# The field has to be READ, or it is documentation pretending to be a
# mechanism — the same thing this lint asks of the allowlist.
runner = open(f"{repo}/infra/ops/ops-runner.sh").read()
if "$spec.hosts" not in runner or "does not serve host" not in runner:
    sys.exit("FAIL: ops-runner.sh does not refuse on `hosts` — the field is decoration "
             "(expected `$spec.hosts` in the decision jq and a refusal naming the host)")

mutating = sorted(n for n, s in verbs.items() if "MUTATING" in s.get("about", ""))
gcp_mutating = sorted(n for n in serving_gcp if n in mutating)
# An admitted name that no longer exists, or that exists but no longer
# serves boss-gcp as a MUTATING verb, is a stale admission — say so
# rather than carry it.
for n in GCP_MUTATING_ADMITTED:
    if n not in verbs:
        sys.exit(f"FAIL: GCP_MUTATING_ADMITTED names {n}, which is not a verb — drop the stale admission")
    if n not in gcp_mutating:
        sys.exit(f"FAIL: GCP_MUTATING_ADMITTED names {n}, which is not a MUTATING verb serving boss-gcp — drop the stale admission")
print(f"a-verb-declares-the-hosts-it-serves: ok — {len(verbs)} verbs each name the hosts they "
      f"serve, from the {len(node_ids)} estate node ids in the tree; boss-gcp serves "
      f"{', '.join(sorted(serving_gcp))}, and of the {len(mutating)} MUTATING verbs only the admitted "
      f"{', '.join(gcp_mutating) or 'none'}; every "
      f"script is repo-relative and in the tree; the runner refuses on the field")
PY
