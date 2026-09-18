# scanned.sh — the one line every scanning lint ends with, and the
# refusal behind it: a lint that scanned nothing certifies nothing.
#
# WHY THIS EXISTS (backlog cdf2d959, tech-debt audit H2, 2026-09-18).
# Two lints in the pre-flight roster exited 0 `clean` on every run for
# months while examining zero things:
#
#   sim-boundary-audit   its awk matched dependency lines with `\s`,
#                        which mawk (the gate image's awk, and this
#                        pod's) reads as a literal `s`. 0 declarations
#                        matched, so the 6-entry allowlist was never
#                        compared to anything — and the lint PRINTED
#                        "0 dep declarations scanned" under `clean`.
#   seed-bypass-smell    globbed examples/*/seeds/sql/, a directory
#                        retired with the playground bundles, and
#                        printed "no seed sql files" — exit 0.
#
# Both said the zero out loud and nothing read it. CLAUDE.md
# §Diagnosis: "a check nobody reads is a check that is not running."
# The fix is not a bigger summary line; it is making the count a
# VERDICT — zero is red — and reading it back from outside the lint.
#
# THE RULE. A scanning lint prints
#
#     <lint>: scanned <N> <what>
#
# on stdout, exactly once, and exits 1 when N is 0. N is the number of
# things the lint actually examined — files, crates, rules, manifests —
# not the number of findings; a count of findings is legitimately zero
# on a clean tree, a count of things looked at is not. The pin is
# crates/core/boss-testing/tests/a_lint_that_scanned_nothing_is_red.rs,
# which runs every infra/lint/*.sh against the tree and reads the line
# back; a lint that is not a scanner (a fixture-driven self-test of a
# script, a one-file assertion, a branch-diff check) is listed THERE
# with its reason, so "not a scanner" is a decision someone wrote down
# and not a lint the pin quietly missed.
#
# WHY EXIT 1 AND NOT 3. lib/git-answer.sh reserves 3 for "the machine
# could not answer". A zero scan is not that: the tree was readable and
# the lint looked in the wrong place, or its parser no longer matches
# the idiom the file is written in. Both are the lint author's to fix,
# which is what exit 1 means.
#
# USAGE
#   . "$LINT_DIR/lib/scanned.sh"
#   lint_scanned "$LINT" "$n" "Rust file(s)"    # prints, or exits 1
#
# Call it AFTER the lint's own verdict is decided and BEFORE the final
# `clean` line, so a zero scan is refused even on a tree with no
# findings — which is the only tree a zero scan ever produces.

# lint_scanned <lint> <n> <what...>
#
#   stdout  `<lint>: scanned <n> <what>` when n is a positive integer
#   stderr  the refusal otherwise, and the process EXITS 1: a lint that
#           read nothing has nothing further to say, and returning would
#           let a caller without `set -e` print `clean` on the next line.
lint_scanned() {
    local lint="$1" n="$2"
    shift 2
    case "${n:-empty}" in
        empty|*[!0-9]*)
            printf '%s: scanned count is not a number (%s) — refusing: a lint that cannot say how many things it read certifies nothing.\n' \
                "$lint" "${n:-empty}" >&2
            exit 1 ;;
    esac
    if [ "$n" -eq 0 ]; then
        {
            printf '%s: scanned 0 %s — refusing rather than passing vacuously.\n' "$lint" "$*"
            printf '  A clean verdict on nothing is the under-covering gate this tree keeps\n'
            printf '  paying for. Either the path this lint scans has moved, or its parser no\n'
            printf '  longer matches the idiom the files are written in; fix the lint, not the\n'
            printf '  tree. (lib/scanned.sh; backlog cdf2d959)\n'
        } >&2
        exit 1
    fi
    printf '%s: scanned %s %s\n' "$lint" "$n" "$*"
}
