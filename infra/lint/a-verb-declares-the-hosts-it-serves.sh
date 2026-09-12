#!/usr/bin/env bash
# a-verb-declares-the-hosts-it-serves.sh — every ops verb says which
# hosts it serves, every named host is a real one, and a verb scoped to
# a host can actually run there.
#
# WHY. The ops-request allowlist (infra/ops/verbs.json) had no notion of
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
#   4. NO MUTATING VERB SERVES boss-gcp. The host was made answerable,
#      not powerful: its set is the read-only, host-agnostic reads, and
#      widening is a reviewed per-verb change with its own
#      authorization — the same process reclaim-disk / converge /
#      publish-github-pr each went through (infra/ops/verbs.json
#      _about). A mutating verb appearing here silently would be that
#      process skipped.
#   5. at least one verb serves boss-gcp, or the runner there answers
#      nothing and this lint is green over a dead door.
#   6. the runner actually READS `hosts`, or the field is decoration.
#
# The sibling is infra/lint/the-controls-are-bounded-verbs.sh, which
# asks whether a MUTATING verb is bounded and authorized; this one asks
# WHERE a verb runs. Both derive their rosters from the allowlist rather
# than listing verbs (§9a).
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"

python3 - "$repo" <<'PY' || exit 1
import json, os, re, sys, glob

repo = sys.argv[1]
verbs = json.load(open(f"{repo}/infra/ops/verbs.json"))["verbs"]

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
        if "MUTATING" in spec.get("about", ""):
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
print(f"a-verb-declares-the-hosts-it-serves: ok — {len(verbs)} verbs each name the hosts they "
      f"serve, from the {len(node_ids)} estate node ids in the tree; boss-gcp serves "
      f"{', '.join(sorted(serving_gcp))} and none of the {len(mutating)} MUTATING verbs; every "
      f"script is repo-relative and in the tree; the runner refuses on the field")
PY
