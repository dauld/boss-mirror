#!/usr/bin/env bash
# consist: skip — runs the built `boss` CLI (`boss tenant check`) on every examples/*/ bundle; the boss-ci image carries no `boss` and a bare tree has no target/, so the gate runs it after its build phase
#
# an-image-sourced-tenant-passes-its-check — every tenant bundle the
# image ships under examples/ passes `boss tenant check`, judged by the
# CLI this tree just built, before the bundle can ride a train.
#
# WHY THIS EXISTS (backlog fd8ee021 part 1, 2026-09-19 — the other
# half of 1af5119d). Since 1af5119d the converge runs `boss tenant
# check` on a `tenant_repo` instance's checkout BEFORE it applies the
# boss-tenant ConfigMap, and a refusal keeps the previous ConfigMap:
# the software converges, the tenant does not, and a packet names the
# file and line. An IMAGE-SOURCED tenant — `tenant_dir =
# "examples/<name>"`, the playground's examples/brewery — never passes
# that door. `infra/oss-quickstart/Dockerfile` COPYs examples/ into the
# image CI builds from whatever the train carried, and the first thing
# to judge a bad seed line there is the gateway's own boot refusal
# (the launcher's "not a tenant directory", the manifest refusal of
# car 32bec388), which takes the system of record down until the
# delivered file is fixed — the 2026-09-07 class, a boot guard that
# refuses to start. So an image-sourced bundle is judged where the
# tree is judged: here, by the gate, with the verb the converge uses.
#
# THE CHECKED PROPERTY. For every DIRECTORY under examples/ — the
# roster is the directory, never a hand list; a file beside them
# (examples/README.md) is not a bundle — `boss tenant check
# examples/<name>` exits 0. The verb reads the contract's shape
# (tenant.toml or seeds/tenant.toml, plus seeds/*) with the product's
# own loaders (crates/orchestrators/boss-cli/src/tenant.rs), so a
# directory that is not a tenant at all is refused by its MISSING
# manifest row: exactly what render-instance.sh and the launcher would
# refuse, judged before the image exists. Every bundle is judged even
# when an earlier one refused, so one run names every broken bundle.
#
# WHICH `boss`. The tree's own build first, then PATH:
#   BOSS_CLI                          a caller's (or a test stub's) binary —
#                                     the converge lib's own variable name
#   $CARGO_TARGET_DIR/release/boss    the gate-runner builds into
#   $CARGO_TARGET_DIR/debug/boss      /gate-target/target and its build
#                                     phase is `cargo build` (debug); the
#                                     dev pod builds into /scratch/target
#   ./target/{release,debug}/boss     a bare cargo build
#   boss on PATH                      the pod's shim (infra/dev/boss),
#                                     the forge's /usr/local/bin/boss
# The tree's build is preferred because it judges the tree's bundles
# with the tree's contract; a PATH binary may be a landed main's.
#
# THREE VERDICTS, the converge lib's (`tenant_check` in
# infra/forge/cluster-deploy-lib.sh) so the two readers of this verb
# agree:
#   0  every bundle passed
#   1  a bundle was REFUSED — `<bundle>` and the first MISSING/INVALID
#      row as `<file>:<line>` on stderr, with the verb's whole report
#      replayed (a reduction before the record throws away the only
#      copy); a fact about the BRANCH
#   3  COULD NOT CHECK — no `boss` anywhere above, or one that exited
#      with anything but 0 or 1 (a panic, a missing shared library, a
#      kill): nothing about the bundle was judged, so neither `clean`
#      nor a violation can be claimed. lib/git-answer.sh's
#      LINT_CANNOT_ANSWER, with its marker, and NO `scanned` line —
#      the gate records a refusal, not a red. No evidence is not a
#      pass.
#
# NOT IN THE PRE-FLIGHT ROSTER, by the header line above. The roster
# runs before any cargo phase, and the boss-ci image ships no `boss`
# (infra/forge/boss-ci/Dockerfile: rust, node, bun, a browser — the CLI
# is a build artifact). A roster lint exiting 3 there would refuse and
# relaunch EVERY gate. So, like no-snapshot-arrays.sh, infra/gate.sh
# runs this as a named check after the build that produces the
# binary: unconditionally in the full gate, and in a scoped (--auto)
# gate when the car touches examples/, the CLI, or this lint — after
# `cargo build -p boss-cli`, because a bundle car's derived scope is
# its engine crate, not boss-cli. Pinned by
# crates/core/boss-testing/tests/an_image_sourced_tenant_passes_its_check.rs.
#
# Usage:  infra/lint/an-image-sourced-tenant-passes-its-check.sh

set -uo pipefail

NAME="an-image-sourced-tenant-passes-its-check"
cd "$(dirname "$0")/../.." || exit 1
# shellcheck source=infra/lint/lib/scanned.sh
. infra/lint/lib/scanned.sh || exit 3
# shellcheck source=infra/lint/lib/git-answer.sh
. infra/lint/lib/git-answer.sh || exit 3

# The refusal of a machine that could not run the verb: the marker
# lib/git-answer.sh reserves, the lint's name, and the reason — so the
# gate's receipt (`refused_because`) carries what happened in the
# lint's own words and nobody re-derives it.
cannot_answer() { # reason...
    {
        printf '%s: %s — %s\n' "$NAME" "$LINT_CANNOT_ANSWER_MARKER" "$*"
        printf '  An INFRASTRUCTURE refusal (exit %s), not a verdict on the branch: no\n' "$LINT_CANNOT_ANSWER"
        printf '  bundle was judged, so neither clean nor a violation can be claimed.\n'
        printf '  The gate runs this after its build phase; run it after `cargo build\n'
        printf '  -p boss-cli`, or name a binary with BOSS_CLI=<path>.\n'
    } >&2
    exit "$LINT_CANNOT_ANSWER"
}

# The binary, in the order the header states. `command -v` on a path
# with a slash answers whether it is executable, which is the question.
find_cli() {
    local target="${CARGO_TARGET_DIR:-target}" c
    if [ -n "${BOSS_CLI:-}" ]; then
        command -v "$BOSS_CLI" && return 0
        return 1
    fi
    for c in "$target/release/boss" "$target/debug/boss"; do
        if [ -x "$c" ]; then printf '%s\n' "$c"; return 0; fi
    done
    command -v boss
}

CLI="$(find_cli)" || cannot_answer "no \`boss\` CLI to run the verb with: not BOSS_CLI (${BOSS_CLI:-unset}), not ${CARGO_TARGET_DIR:-target}/{release,debug}/boss, not on PATH"

[ -d examples ] || { echo "$NAME: examples/ does not exist" >&2; exit 1; }

# The roster: directories directly under examples/, C-locale order, so
# two hosts judge the same bundles in the same sequence.
bundles="$(find examples -mindepth 1 -maxdepth 1 -type d | LC_ALL=C sort)"

judged=0
refused=0
while IFS= read -r dir; do
    [ -n "$dir" ] || continue
    judged=$((judged + 1))
    rc=0
    out="$("$CLI" tenant check "$dir" 2>&1)" || rc=$?
    case "$rc" in
        0) continue ;;
        1) ;;
        *)
            cannot_answer "\`$CLI tenant check $dir\` exited $rc without a verdict: $(printf '%s' "$out" | tr '\n' ' ' | cut -c1-300)" ;;
    esac
    refused=$((refused + 1))
    # The first refused row of the verb's render (tenant.rs
    # Report::render: status, path, detail), as the converge lib reads
    # it. A here-string, not a pipe: awk stops at its first match, and
    # a producer still writing is SIGPIPE under pipefail.
    row="$(awk '$1 == "INVALID" || $1 == "MISSING" { print; exit }' <<< "$out")"
    path="$(awk '{ print $2 }' <<< "$row")"
    if [[ "$row" =~ [[:space:]]line[[:space:]]+([0-9]+) ]]; then
        where="$path:${BASH_REMATCH[1]}"
    else
        where="${path:-(the verb named no MISSING or INVALID row)}"
    fi
    {
        printf '%s: %s REFUSED at %s\n' "$NAME" "$dir" "$where"
        printf '%s\n' "$out" | sed 's/^/    /'
    } >&2
done <<EOF
$bundles
EOF

if [ "$refused" -gt 0 ]; then
    cat >&2 <<MSG

  $refused of $judged tenant bundle(s) under examples/ would refuse to boot. The
  image ships every one of them (infra/oss-quickstart/Dockerfile COPYs
  examples/), and an instance declaring tenant_dir = "examples/<name>"
  boots its gateway from that directory — so a bundle the verb refuses
  here would take the system of record down at the next converge (backlog
  fd8ee021; the converge checks a tenant_repo checkout the same way,
  1af5119d). Fix the file and line named above; docs/tenant-contract.md
  is the shape, and \`boss tenant check examples/<name>\` is the verb.
MSG
    exit 1
fi

lint_scanned "$NAME" "$judged" "tenant bundle(s) under examples/"
echo "$NAME: ok — every tenant bundle the image ships passes \`boss tenant check\` (judged by $CLI)"
exit 0
