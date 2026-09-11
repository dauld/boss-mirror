# pattern-scan.sh — the shared body of every "this pattern must not
# appear" lint, so none of them has to remember the two things they
# all forget.
#
# WHY THIS EXISTS. A lint that forbids a pattern has to NAME that
# pattern, and so does the test that proves the lint works. Both are
# then hits. `no-session-paths.sh` remembered to exclude itself.
# `one-palette.sh` remembered too — and still went red on a train,
# because the mocked spec that documents its rule also names
# `prefers-color-scheme`, and the lint and the spec only met each
# other during assembly (2026-08-24: four checks green, one red, nine
# cars held). Each lint got this right or wrong on its own; a rule
# every author must re-derive is a rule that will be got wrong again.
#
# THE CONVENTION, so the exemption needs no list:
#   a lint at infra/lint/<name>.sh is proven by files whose path
#   contains <name> and which live under a tests/ directory or carry
#   a .spec./.test. segment.
# Both are excluded by construction. Nothing else is — a lint that
# wants a domain exemption (no-session-paths lets docs/ name paths,
# because runbooks legitimately tell that story) states it as an extra
# argument, in its own file, where a reader can see the judgement.
#
# AND THE SECOND THING THEY ALL FORGOT (backlog 6b2f4a1a, measured
# 2026-09-11). The scan ended `|| true`, so a `git grep` that could not
# RUN — exit 128 on a dubious-ownership refusal, a corrupt object, an
# unreadable index — produced no hits and the caller printed `clean` and
# exited 0 on a tree nothing had looked at. `git grep` already tells the
# two apart (1 = no match, >1 = error) and `|| true` was throwing exactly
# that away. So does `git ls-files` below, whose failure emptied the
# exemption list instead. Both now refuse through `git_answer`, which
# prints git's own words and returns 3; see lib/git-answer.sh for why the
# status is 3 and not 1.
#
# USAGE
#   . "$(dirname "$0")/lib/pattern-scan.sh"
#   hits=$(pattern_scan 'prefers-color-scheme' -- 'apps/' 'libs/') || exit $?
#   hits=$(pattern_scan 'X' --exclude ':!docs/' -- 'crates/') || exit $?
#
# Returns hits on stdout (empty when clean); the caller decides the
# message, because the remediation text is the part that has to be
# written by someone who understands the rule. A non-zero return is
# $LINT_CANNOT_ANSWER with the refusal already on stderr — `|| exit $?`
# is the whole of the caller's duty, and leaving it off is a shell error
# under `set -e` rather than a silent green.

# shellcheck source=infra/lint/lib/git-answer.sh
. "$(dirname "${BASH_SOURCE[0]}")/git-answer.sh"

pattern_scan() {
    local pattern="$1"; shift
    local name; name="$(basename "${BASH_SOURCE[1]:-$0}" .sh)"
    local excludes=(":!infra/lint/${name}.sh")

    # Caller-declared exemptions, before the -- separator.
    while [ "$#" -gt 0 ] && [ "$1" != "--" ]; do
        if [ "$1" = "--exclude" ]; then
            shift; excludes+=("$1")
        fi
        shift
    done
    [ "${1:-}" = "--" ] && shift

    # The proof-exempts-itself rule. Discovered, not listed: a file is
    # a proof of THIS lint if its path names the lint and it lives
    # where tests live. `git ls-files` so it matches what git grep
    # searches, and so a file nobody tracked cannot buy an exemption.
    #
    # Read into a variable rather than through a process substitution:
    # `done < <(git ls-files)` discards the status, so a git that
    # refused produced an EMPTY exemption list — the loop simply never
    # ran — and the scan carried on as if the repo tracked nothing.
    local tracked f
    tracked=$(git_answer "$name" 0 ls-files) || return "$LINT_CANNOT_ANSWER"
    while IFS= read -r f; do
        [ -n "$f" ] || continue
        case "$f" in
            *"$name"*)
                case "$f" in
                    */tests/*|*.spec.*|*.test.*|*_test.*|*/testdata/*)
                        excludes+=(":!${f}") ;;
                esac ;;
        esac
    done <<EOF
$tracked
EOF

    # 0 = hits, 1 = no hits, anything else = the scan did not happen.
    local hits status
    hits=$(git_answer "$name" 0,1 grep -nE "$pattern" -- "$@" "${excludes[@]}")
    status=$?
    [ "$status" -eq "$LINT_CANNOT_ANSWER" ] && return "$LINT_CANNOT_ANSWER"
    if [ -n "$hits" ]; then printf '%s\n' "$hits"; fi
    return 0
}
