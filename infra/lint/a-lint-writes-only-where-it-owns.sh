#!/usr/bin/env bash
# a-lint-writes-only-where-it-owns.sh — no check in the pre-flight roster
# writes to a FIXED path under /tmp. Every scratch file hangs off a
# `mktemp`/`mktemp -d` the run owns, or off a parameter the caller sets.
#
# WHY. The roster runs on the long-lived dev pod, which is where a
# builder types `infra/gate.sh --quick` to form a belief about whether
# their change works. A fixed path there is shared with every other uid
# on the box, and the SECOND run is the one that fails — on a file the
# first run owns, for a reason that has nothing to do with the tree.
#
# Measured 2026-09-11 (packet 5bf96e72), twice in one sweep:
#
#   * a-cluster-node-reports-its-headroom.sh extracts the estate
#     observer out of a Kubernetes manifest and runs it, and that shell
#     wrote /tmp/nodes.json, /tmp/fs/, /tmp/free.json and
#     /tmp/observation.json. root ran the roster; a second uid then ran
#     it and died with `Permission denied` — reported by the lint as the
#     observer breaking its best-effort contract, which is not what had
#     happened. A verdict that names the wrong thing costs more than the
#     failure (CLAUDE.md §Diagnosis).
#   * svelte-check.sh captured `bun install` to
#     /tmp/boss-bun-install.log. The second uid's redirect fails, the
#     `if !` branch fires, and a clean tree reports "bun install failed"
#     — with a log it also cannot read.
#
# Both passed in a fresh gate pod, where /tmp is empty. That is what
# makes this latent rather than broken, and why it needs a mechanism
# instead of a note: the place it bites is the place nothing re-runs.
#
# WHAT THIS CAN AND CANNOT SEE. The real rule is "do not write a fixed
# path on the HOST THIS RUNS ON", and which machine a path names is not
# in the text. `docker cp x c:/tmp/y` (infra/install-smoke/nightly.sh)
# and the estate CronJob's own `> /tmp/nodes.json` both name a
# CONTAINER's /tmp, where a fixed path is correct and private. So this
# checks the write-shaped proxy — a redirection or a file-creating verb
# whose target is a literal /tmp path — over the roster only, and holds
# a named exemption list for the legitimate cases. When the first
# container-internal write appears in a lint, it goes in $EXEMPT with
# its reason; it does not get the rule watered down.
#
# ALLOWED, and each is the shape of the fix:
#   * `mktemp` / `mktemp -d`, including `-p /tmp` — a unique path.
#   * `${SOMETHING:-/tmp...}` — a parameter whose default is /tmp. The
#     caller can point it somewhere it owns, which is exactly how the
#     estate observer's inline shell was fixed: the pod reads /tmp, the
#     lint sets BOSS_OBSERVE_WORK to its own mktemp -d.
#   * comment lines, and any mention that is not write-shaped.
set -uo pipefail

NAME="a-lint-writes-only-where-it-owns"
cd "$(dirname "$0")/../.." || exit 1

# Paths exempted from the rule, as `<file>:<line-text-fragment>`, each
# with the reason it is tolerated. A NAMED SET, never a count, so adding
# one never edits a shared tail line (CLAUDE.md §9a). Empty today.
EXEMPT=()

# One file's offending lines, as `<line>\t<text>`. Empty output = clean.
#
# awk, and no `{n}` interval anywhere: mawk (the awk in the CI image)
# does not support intervals and silently matches NOTHING, which is a
# scanner that reads as a clean tree. The self-test below exists because
# that failure is invisible from the outside.
offending_lines() { # file
    awk '
        # Comment lines are prose. A trailing comment on a code line is
        # still scanned, which is the conservative direction.
        /^[ \t]*#/ { next }
        # The fix shapes, allowed wherever they appear on the line.
        /mktemp/ { next }
        /:-\/tmp/ { next }
        # Write-shaped: a redirection, or a verb that creates a path,
        # whose target is a literal /tmp path.
        /[0-9]?>>?[ \t]*"?\/tmp\// ||
        /&>[ \t]*"?\/tmp\// ||
        /(mkdir|touch|tee|rm|cp|mv|ln|install|chmod|chown|truncate)([ \t]+-[^ \t]+)*[ \t]+"?\/tmp\// {
            printf "%d\t%s\n", FNR, $0
        }
    ' "$1"
}

is_exempt() { # file line-text
    local e
    for e in ${EXEMPT+"${EXEMPT[@]}"}; do
        case "$2" in *"${e#*:}"*) [ "${e%%:*}" = "$1" ] && return 0 ;; esac
    done
    return 1
}

# --- self-test ---------------------------------------------------------
# Runs on EVERY invocation, not only under a flag: a scanner whose regex
# has stopped matching passes every file, and the only way to tell that
# from a clean tree is to hand it something it must refuse. Fixtures live
# in a temp directory, never in infra/lint/ — a file there would be
# discovered as a real lint by gate.sh's roster and by the conductor's
# consist check.
self_test() {
    local t hits d=/tmp
    t="$(mktemp -d)" || { echo "$NAME: cannot make a temp dir for the self-test" >&2; return 1; }
    # shellcheck disable=SC2064
    trap "rm -rf '$t'" RETURN

    cat >"$t/good.sh" <<EOF
# This comment mentions $d/nodes.json and must not trip the scanner.
tmp="\$(mktemp -d)"
trap 'rm -rf "\$tmp"' EXIT
echo hi > "\$tmp/out"
log="\$(mktemp -p $d)"
WORK="\${BOSS_OBSERVE_WORK:-$d}"
mkdir -p "\$WORK/fs"
printf 'x' > "\$WORK/nodes.json"
EOF
    hits="$(offending_lines "$t/good.sh")"
    [ -z "$hits" ] || { echo "$NAME: self-test FAILED — a hermetic script was flagged:" >&2
                        printf '%s\n' "$hits" >&2; return 1; }

    # Each bad line is a real shape this repo shipped. The fixed path is
    # passed to printf as an ARGUMENT rather than spelled in these lines,
    # so this file is scanned by its own scanner like every other lint —
    # no self-exclusion, which would be a hole exactly where the author
    # of the next fixed path is most likely to be working. The fixture
    # FILES still hold the literal text.
    printf 'kubectl get nodes -o json > %s/nodes.json\n'   "$d" >"$t/bad1.sh"
    printf 'bun install >%s/boss-bun-install.log 2>&1\n'   "$d" >"$t/bad2.sh"
    printf 'mkdir -p %s/fs && rm -f %s/fs/*.json\n'   "$d" "$d" >"$t/bad3.sh"
    printf 'kubectl get --raw x 2>"%s/fs/$n.err"\n'        "$d" >"$t/bad4.sh"
    printf 'somecmd &> %s/out.log\n'                       "$d" >"$t/bad5.sh"
    printf 'tee %s/report.txt < in\n'                      "$d" >"$t/bad6.sh"
    local f
    for f in bad1 bad2 bad3 bad4 bad5 bad6; do
        [ -n "$(offending_lines "$t/$f.sh")" ] || {
            echo "$NAME: self-test FAILED — the scanner passed $(cat "$t/$f.sh")" >&2
            return 1
        }
    done
    echo "$NAME: self-test ok — a mktemp script, a \${VAR:-/tmp} parameter and a prose mention pass; six fixed-path writes (a redirect, a log capture, mkdir+rm, an stderr redirect, &> and tee) are each named"
}

if [ "${1:-}" = "--self-test" ]; then self_test; exit $?; fi
self_test || exit 1

# --- the roster --------------------------------------------------------
# The DIRECTORY is the roster, the way gate.sh and the conductor's
# consist check both read it, so a lint added tomorrow is covered
# without this file changing.
shopt -s nullglob
files=(infra/lint/*.sh infra/lint/lib/*.sh)
shopt -u nullglob
if [ "${#files[@]}" -lt 20 ]; then
    echo "$NAME: found only ${#files[@]} lint(s) under infra/lint — the scrape broke, refusing rather than reporting a clean tree" >&2
    exit 1
fi

findings=0
for f in "${files[@]}"; do
    while IFS=$'\t' read -r lineno text; do
        [ -n "${lineno:-}" ] || continue
        is_exempt "$f" "$text" && continue
        findings=$((findings + 1))
        echo "$NAME: $f:$lineno writes a fixed path under /tmp:" >&2
        echo "    $text" >&2
    done < <(offending_lines "$f")
done

if [ "$findings" -gt 0 ]; then
    cat >&2 <<EOF
$NAME: FAIL — $findings fixed /tmp write(s) above.

  Two uids running the lint roster on one long-lived host collide there,
  and the SECOND one fails on a file it cannot write — for a reason that
  has nothing to do with the tree it is checking. Use a \`mktemp -d\` the
  run owns and clean it up on a trap, or, when the path belongs to
  something else (a container's own /tmp), make it a parameter
  \`\${VAR:-/tmp/...}\` so the caller can point it at a directory it owns.
EOF
    exit 1
fi

echo "$NAME: ok — ${#files[@]} lint(s) checked, none writes a fixed path under /tmp"
exit 0
