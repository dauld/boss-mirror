# git-answer.sh — the one place a lint asks git something, and the only
# place that can tell "git answered, and the answer is no" apart from
# "git did not answer".
#
# WHY THIS EXISTS. Measured 2026-09-11, backlog 6b2f4a1a. git was
# refusing every command in a gate workspace (dubious ownership as uid
# 65534, exit 128). Of the lints that ask git a question, FOUR reported
# a tree they had never read:
#
#   no-session-paths     printed `clean`, exit 0   (git grep ... || true)
#   one-palette          printed `clean`, exit 0   (git grep ... || true)
#   a-new-style-has-a-caller   `no trunk ref found — skipping`, exit 0
#   a-kind-bundle-does-not-tighten  same, exit 0
#
# and two invented a remediation:
#
#   migrations-append-only   `no trunk ref found … Fetch the trunk`
#   steptype-bundle-ratchet  the same, both exit 1
#
# The trunk was present in all six cases. Fetching it would have changed
# nothing. The worst of those behaviours is not the refusal — it is the
# CERTIFICATION: a green check on a tree nothing looked at is the
# under-covering gate this repo keeps paying for, and it is invisible by
# construction.
#
# THE RULE. `grep`, `git grep`, `git rev-parse --verify --quiet` and
# friends already distinguish the two answers for you: exit 1 means "no,
# and I looked", exit >1 means "I could not look". `|| true` and
# `2>/dev/null` throw exactly that distinction away — the first discards
# the status, the second discards git's explanation — and what is left is
# a confident sentence with nothing behind it.
#
# THE STATUS. A lint that cannot answer exits 3, never 0 and never 1:
#
#   0  the lint read the tree and the tree is clean
#   1  the lint read the tree and found a violation — a fact about the
#      BRANCH, which is the author's to fix
#   3  the lint never read the tree — a fact about the MACHINE, which no
#      author can fix by editing code
#
# Why a third code rather than plain 1. CLAUDE.md §Diagnosis: "an
# infrastructure refusal is not a consist failure… recorded as a plain CI
# failure it strikes every car aboard". Exit 1 erases the distinction at
# the point where it is still cheap to keep; nothing downstream can
# recover it afterwards, because the text of a lint's message is not a
# contract. Exit 3 is already this tree's word for it —
# `infra/safe-cargo.sh`, `infra/forge/journal-read.sh` ("UNREACHABLE →
# SKIPS LOUDLY (exit 3). Never 0"), `infra/forge/run-car-probe.sh`,
# `infra/cluster/undeclared-objects.sh` — so a reader who knows the
# vocabulary reads it right today.
#
# What each reader does with it NOW, measured rather than assumed:
#
#   the consist check (`boss-cli` train.rs `run_one_lint`) maps every
#   code other than 0/126/127 to `LintResult::Failed`, and a failed
#   cheap lint "opens no PR, strikes no car, and leaves every one of
#   them boardable carrying a reason that names the lint". That is
#   already the correct handling of an infrastructure refusal, so exit 3
#   needs no change there. Deliberately NOT mapped to `Unrunnable`:
#   that is a warning the train PROCEEDS past, which would turn a lint
#   that read nothing into a boarding.
#
#   the per-car gate (`infra/gate.sh` `check()`) knows only pass/fail
#   and will report exit 3 as a plain red check. That is safe — it never
#   certifies — but it is the remaining half: the gate should record a
#   refusal the way its disk floor already does (`GATE_REFUSAL`,
#   `write_receipt "refused"`, exit 2) instead of as a verdict on the
#   branch. gate.sh is owned by another car as this lands; the mapping
#   is filed separately. The code carries the distinction so that change
#   is a gate-only edit and nothing has to be re-derived.
#
# USAGE
#   . "$LINT_DIR/lib/git-answer.sh"
#   LINT=my-lint
#   hits=$(git_answer "$LINT" 0,1 grep -nE "$pat" -- apps/) || exit $?
#   #                          ^ the statuses that are ANSWERS; anything
#   #                            else is a refusal, printed on stderr,
#   #                            and `git_answer` returns 3.
#
# The answer-status set is per-command and there is no safe default, so
# it is required at every call site:
#   git grep            0 = hits, 1 = no hits
#   rev-parse --verify --quiet   0 = resolved, 1 = no such ref
#   merge-base          0 = found, 1 = no common ancestor
#   ls-files, diff, show, ls-tree   0 only
# `git cat-file -e <tree>:<path>` is deliberately not used anywhere in
# this tree any more: it exits 128 BOTH for "that path is not in that
# tree" and for "this repository is unreadable", so the two cases cannot
# be told apart at all. `git ls-tree --name-only <tree> -- <path>` is the
# readable form — exit 0 with empty output means absent, and only a real
# failure is non-zero.

# The exit status of "I could not answer". Exported so a self-test that
# re-enters a lint's functions in a child shell sees the same number.
LINT_CANNOT_ANSWER=3
export LINT_CANNOT_ANSWER

# The words every refusal starts with. A constant because two things read
# it: an operator, and a lint that runs a git-using self-test in a child
# shell and has to tell "my fixtures are broken" from "git is broken"
# (§9a — one definition, not a string typed twice).
LINT_CANNOT_ANSWER_MARKER="CANNOT ANSWER"
export LINT_CANNOT_ANSWER_MARKER

# git_can_answer <lint>
#
# One cheap question whose answer is already known — "is there a git
# repository here" — asked before a lint does anything expensive, so a
# machine where git cannot run at all refuses by name instead of failing
# in whatever it tried first. CLAUDE.md §Doors: "before concluding
# something does not exist, run a query on the same connection whose
# answer you already know."
git_can_answer() {
    git_answer "$1" 0 rev-parse --git-dir >/dev/null
}

# The refusal text. Four facts, because each one was re-derived by hand
# during the measurement above: WHICH lint, WHICH git command, WHAT
# status, and git's OWN words — the last being the only one that says
# whether this is an ownership refusal, a corrupt object or a full disk.
_git_answer_refuse() {
    local lint="$1" status="$2" err="$3"
    shift 3
    {
        printf '%s: CANNOT ANSWER — `git %s` exited %s, so nothing was read.\n' \
            "$lint" "$*" "$status"
        if [ -s "$err" ]; then
            printf '  git said:\n'
            sed 's/^/    /' "$err"
        else
            printf '  git said nothing on stderr, which is itself the finding.\n'
        fi
        printf '  An INFRASTRUCTURE refusal (exit %s), not a verdict on the branch:\n' \
            "$LINT_CANNOT_ANSWER"
        printf '  no file was examined, so neither `clean` nor a violation can be\n'
        printf '  claimed. Fix the git environment on this machine and re-run — there\n'
        printf '  is nothing here for the author of the change to edit.\n'
    } >&2
}

# git_answer <lint> <answer-statuses> <git args...>
#
#   stdout  git's stdout, verbatim (one trailing newline when non-empty)
#   stderr  git's stderr on an answer; the refusal above otherwise
#   return  git's own status when it is one of <answer-statuses>;
#           $LINT_CANNOT_ANSWER otherwise
git_answer() {
    local lint="$1" answers="$2"
    shift 2
    local err out status
    err="$(mktemp "${TMPDIR:-/tmp}/lint-git-answer.XXXXXX")" || {
        printf '%s: CANNOT ANSWER — no writable temp dir, so `git %s` could not be run with its stderr kept.\n' \
            "$lint" "$*" >&2
        return "$LINT_CANNOT_ANSWER"
    }
    out="$(git "$@" 2>"$err")"
    status=$?
    case ",${answers}," in
        *",${status},"*)
            # An answer. git's stderr is passed through rather than
            # swallowed: a warning on a successful command is still the
            # only copy of itself.
            if [ -s "$err" ]; then cat "$err" >&2; fi
            if [ -n "$out" ]; then printf '%s\n' "$out"; fi
            rm -f "$err"
            return "$status"
            ;;
    esac
    _git_answer_refuse "$lint" "$status" "$err" "$@"
    rm -f "$err"
    return "$LINT_CANNOT_ANSWER"
}
