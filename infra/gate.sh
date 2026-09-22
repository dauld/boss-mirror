#!/usr/bin/env bash
# gate.sh — THE definition of the rust gate. CI's rust job invokes this
# script; anyone gating a car locally invokes this script. There is no
# second list of checks to drift from this one (CLAUDE.md §9a — on the
# 2026-08-10 train the gate's definition lived twice and drifted twice
# in one day; boss-testing/tests/gate_sh.rs pins this collapse).
#
# Usage:
#   infra/gate.sh                 # full gate — exactly what CI runs
#   infra/gate.sh --quick         # PRE-FLIGHT only: fmt + the lints
#                                 # that need no build, ~17s. Not a
#                                 # gate — nothing compiles. Run it
#                                 # before spending 17 minutes of
#                                 # cluster time on a formatting slip.
#   infra/gate.sh --lint          # --quick PLUS clippy, scoped to the
#                                 # crates the tree changed. Seconds on
#                                 # a warm tree against ~11 minutes of
#                                 # gate, and clippy is the red class
#                                 # that buys the least: prefer this to
#                                 # --quick before pushing. Still not a
#                                 # gate — the build and the suites
#                                 # remain unproven.
#   infra/gate.sh --auto          # car mode, scope DERIVED from the
#                                 # tree. Skips cargo entirely when
#                                 # nothing changed implies a crate —
#                                 # 74 of 164 live branches are in that
#                                 # class. A migration implies every
#                                 # crate that stands up the schema.
#                                 # Never used by CI.
#   infra/gate.sh --self-test     # run the roster loop's own pin and
#                                 # nothing else. It runs inside every
#                                 # mode below too; this is the way to
#                                 # read its verdict on its own.
#   infra/gate.sh --roster        # print the pre-flight roster, one
#                                 # `<name> <path>` per lint, and stop
#   infra/gate.sh --exclusions    # print the lints the pre-flight
#                                 # leaves out, `<path>\t<why>` each,
#                                 # read off their own headers
#   infra/gate.sh -p crate [...]  # car mode — cargo phases scoped to
#                                 # the named crates (FULL suites, all
#                                 # features); lints + fmt always run
#                                 # repo-wide, they are cheap
#
# Car mode REFUSES a `-p` set that does not cover the crates the tree
# actually changes — see "`-p` states a belief" below.
#
# Environment setup (toolchain, dependency cache, schema apply for
# DB-backed tests) is the caller's job — CI does it in ci.yml steps,
# a dev box has it standing. The gate is the checks, nothing else.

set -u

cd "$(dirname "$0")/.."

# The lint vocabulary, read from its one definition: `LINT_CANNOT_ANSWER`
# (exit 3) is a lint saying "the machine could not answer", and
# `check_lint` below turns it into a refusal rather than a red. Sourced
# rather than spelled as a `3` here so the number cannot drift from the
# lints that exit it (CLAUDE.md §9a).
# shellcheck source=infra/lint/lib/git-answer.sh
. infra/lint/lib/git-answer.sh || {
    echo "gate.sh: infra/lint/lib/git-answer.sh could not be read — without it the pre-flight cannot tell a lint that could not answer from one that failed. Refusing." >&2
    exit 2
}

# Incremental compilation helps REPEATED local builds; a gate build is
# cold and one-shot, so incremental only writes an incremental/ dir that
# is pure disk cost here — part of the ~80GB target/ that exhausted the
# forge CI disk and blocked trains (2026-09-04;
# docs/design/the-build-plane-manages-itself.md). Off for the gate and CI
# (this script IS the CI rust job); a human's own `cargo build` outside
# this script is untouched and keeps incremental.
export CARGO_INCREMENTAL=0

# THE GATE'S GIT READS MUST WORK FOR WHATEVER UID THE GATE RUNS AS.
#
# Since #310 (2026-09-11) the gate runs as uid 65534 / gid 1500, and
# every builder brief says to verify work under `setpriv --reuid=65534`.
# This pod's checkout is root-owned (`drwxrwsr-x root 1500`), so since
# git 2.35.2 every git command run by any other uid refuses it:
#
#     fatal: detected dubious ownership in repository at '/work/boss'
#
# MEASURED as 65534 on this tree (packet 8674c440): `--quick` exits 1
# naming three pre-flight lints, and the defect is NOT three loud
# failures — it is five lints, failing three different ways:
#
#   no-secrets                 `git ls-files` refuses; the lint FAILS,
#                              printing the refusal. The honest one.
#   migrations-append-only     the trunk `rev-parse` is `2>/dev/null`,
#   steptype-bundle-ratchet    so the refusal is swallowed and the lint
#                              fails with "no trunk ref found … Fetch
#                              the trunk" — a confident WRONG diagnosis
#                              of an environment problem.
#   no-session-paths           `git grep` through pattern-scan.sh and
#   one-palette                one-palette's own call both end `|| true`.
#                              The scan returns nothing and the lint
#                              reports `clean`. A SILENT FALSE PASS:
#                              the gate certifies a tree it never read.
#
# The last pair is why the fix belongs HERE and not in the lints. A
# per-lint fix is three edits that have to FIND all five, and the two
# that matter most announce nothing when they are broken (CLAUDE.md §9a
# — that is the drift shape, not the fix). One exported env slot reaches
# every git in the child tree, including the lints nobody has written
# yet, and cannot drift from itself.
#
# git's env-var config channel, not a `git -c` on each call site, for
# the same reason: a call site list is the pair this repo keeps paying
# for.
#
# SCOPED TO THE RESOLVED ROOT, never `*`. Blessing one known path is a
# statement about this checkout; a wildcard would bless every repository
# any child ever reaches — the throwaway `git init` fixtures that
# migrations-append-only and an-expectation-names-a-rule build, the
# clones a probe makes — and those are exactly the repositories whose
# ownership nobody has vouched for.
#
# APPENDED after whatever `GIT_CONFIG_*` the caller already exports,
# the way `boss-cli`'s `git_auth::apply_at` appends:
# `infra/cluster/manifests/boss-dev.yaml` uses slot 0 for the forge
# credential helper, so a flat `GIT_CONFIG_COUNT=1` here would silently
# take that helper away from every child of the gate.
#
# WHY `safe.directory` IS RIGHT HERE AND WRONG IN
# `infra/forge/delete-orphan-object.sh`, which rejects it by name and
# drops to the tree's owner instead: that verb runs as root inside
# somebody else's clone, and a root WRITE there leaves root-owned
# objects that break the owner's later pulls — silencing the check would
# buy its read at the price of making that hazard reachable by the next
# edit. NOTHING IN THE GATE WRITES TO GIT: `--auto` diffs, the receipt
# reads HEAD, the lints read history and the index. That is the same
# distinction `infra/forge/publish-github-pr.sh` drew when it DID accept
# `safe.directory` for its fetch. Do not copy this to a path that
# writes, and do not widen it.
#
# It is not new trust, either: the gate is already EXECUTING this
# checkout's scripts, so declining to read its history was never a
# safety property — only a refusal in the wrong place.
GATE_REPO_ROOT="$(pwd -P)"
gate_git_slot="${GIT_CONFIG_COUNT:-0}"
case "$gate_git_slot" in
    ''|*[!0-9]*) gate_git_slot=0 ;;
esac
export "GIT_CONFIG_KEY_${gate_git_slot}=safe.directory"
export "GIT_CONFIG_VALUE_${gate_git_slot}=${GATE_REPO_ROOT}"
export GIT_CONFIG_COUNT=$((gate_git_slot + 1))

SCOPE=()
NAMED=()
AUTO=0
QUICK=0
LINT=0
ROSTER=0
EXCLUSIONS=0
SELFTEST=0
while [ $# -gt 0 ]; do
    case "$1" in
        -p) shift; SCOPE+=(-p "${1:?-p needs a crate name}"); NAMED+=("$1"); shift ;;
        --auto) AUTO=1; shift ;;
        --quick) QUICK=1; shift ;;
        --lint) LINT=1; shift ;;
        --roster) ROSTER=1; shift ;;
        --exclusions) EXCLUSIONS=1; shift ;;
        --self-test) SELFTEST=1; shift ;;
        *) echo "gate.sh: unknown arg: $1 (accepts -p <crate>, --auto, --quick, --lint, --roster, --exclusions and --self-test)" >&2; exit 2 ;;
    esac
done
# Alternatives, not companions: --auto derives exactly what -p states,
# so accepting both would mean silently preferring one belief over the
# other — and the whole point of the refusal below is that a stated
# belief gets checked, never quietly overridden.
if [ "$AUTO" -eq 1 ] && [ ${#NAMED[@]} -gt 0 ]; then
    echo "gate.sh: --auto and -p are alternatives; --auto derives what -p would state" >&2
    exit 2
fi


# ---------------------------------------------------------------------
# The pre-flight set: every check that needs no build
# ---------------------------------------------------------------------
# `cargo fmt -- --check` and the lint roster are repo-wide greps and
# audits. Together they take ~17 SECONDS on a cold tree. They used to
# run near the END of the gate, behind clippy, the full test suite and
# the bun web suite.
#
# That ordering is not a bug — `check()` deliberately runs every check
# even after one fails, "a red gate should report every failure it can
# see, not make the author fix serially", and reordering saves a red
# gate nothing because it runs everything regardless.
#
# The cost lands somewhere else: there was no way to run the cheap
# checks WITHOUT the expensive ones. So the only way to find a
# formatting slip was to spend a gate. On 2026-08-27 a car did exactly
# that — 17 minutes of cluster time, a scheduled pod and a clone, to
# learn that `cargo fmt` had been run on one crate and not another.
# 17 seconds of local work, discovered 60x more slowly.
#
# Hence `--quick`, and hence this list existing ONCE. Two rosters would
# drift (CLAUDE.md §9a) and would drift in the worst direction: a check
# quietly missing from the local pre-flight still passes locally and
# still reds a full gate, which is precisely the failure being fixed.
#
# The roster is the DIRECTORY. Until 2026-09-05 it was a hand-listed
# array here, and every car that added a lint edited the same tail
# line: four cars collided on it in one day, and the fourth was left
# behind by train #218 ("conflict: infra/gate.sh"). That is the
# manifest.txt lesson (CLAUDE.md §9a) one level up — a list holding no
# information its source does not is a merge conflict waiting to
# happen. Adding a lint is now dropping a file in infra/lint/; this
# file does not change. The conductor's consist check reads the same
# directory and asks THIS SCRIPT what to leave out (train/consist.rs
# `gate_exclusions` runs `gate.sh --exclusions` in the assembled
# tree), so the two readers agree by construction rather than by
# being kept in step.
#
# What is NOT run here is declared BY THE LINT ITSELF, on one line of
# its header:
#
#     # consist: skip — <what a bare tree cannot answer in seconds>
#
# The header, not anywhere in the file: the first line that is not a
# comment ends it, so a lint that mentions the marker in its prose is
# not excluded by it. The reason is required — a bare declaration is
# refused by name, because an exemption nobody explained is one nobody
# can later judge. Each declaring lint needs something a bare tree
# cannot answer in seconds: a live database, a built workspace, a
# package manager.
#
# WHY THE LINT DECLARES IT, rather than a list here. Until 2026-09-18
# the exclusion set lived FIVE times — an array here, a hand copy in
# boss-testing's gate_sh.rs, the conductor's compiled fallback in
# delivery_policy.rs, the delivery-policy seed migration, and the live
# registry row — with two pins holding two of the pairs and NOTHING
# holding this array equal to what the conductor ran on (tech-debt
# audit H9, backlog 6fa15484). A fact the lint's author already knows
# when writing it (this needs psql; this needs a built binary) was
# being retyped four times by four other people. Now the lint says it
# once, this function reads it, `--exclusions` prints it, and every
# other reader asks here. The set is pinned by gate_sh.rs against the
# roster, both asked of this script rather than re-parsed from it.
consist_exclusions() {
    local listed path readable=()
    # Only files awk can open: a lint it cannot read (a dangling
    # symlink some car left) declares nothing, and the roster loop
    # below reports it as a check that could not run — which is the
    # honest verdict, where an awk refusal here would take the whole
    # roster down over one name.
    for path in $(LC_ALL=C ls infra/lint/*.sh); do
        [ -r "$path" ] && readable+=("$path")
    done
    [ ${#readable[@]} -eq 0 ] && return 0
    listed=$(LC_ALL=C awk '
        FNR == 1 { header = 1; if (/^#!/) next }
        !header { next }
        /^[ \t]*$/ { next }
        !/^#/ { header = 0; next }
        sub(/^# consist: skip[ \t]*/, "") {
            sub(/[ \t]+$/, "")
            if (!sub(/^—[ \t]*/, "") || $0 == "") {
                printf "gate.sh: %s declares a consist skip with no reason — the line is: # consist: skip — <why a bare tree cannot answer it>\n", FILENAME > "/dev/stderr"
                bad = 1
                next
            }
            print FILENAME "\t" $0
        }
        END { exit bad }
    ' "${readable[@]}") || return 1
    [ -n "$listed" ] && printf '%s\n' "$listed"
    return 0
}

# FIRST on purpose: it says what this workspace cannot cover, which
# frames every result below it. A green pre-flight on a machine with
# no Postgres is 118 database-backed test targets unrun, and saying
# so before the rest is the difference between confidence and a
# gate failure eleven minutes later (design 775f0b35 Q3).
PREFLIGHT_FIRST="infra/lint/workspace-declares-what-it-runs.sh"

# One `<name> <path>` line per lint, in the order they run: the pinned
# first, then the directory in C-locale order, so two hosts ask the
# same questions in the same sequence. The excluded set cannot name a
# file that does not exist any more — it is read off the files that
# do — so the only roster entry that can be missing is the pinned one.
#
# `cargo-advisories` is the one lint allowed a network fetch: it is
# report-only (always exits 0) and soft-skips when the tool or the
# advisory DB is absent, so it cannot red a gate — only add a line.
preflight_roster() {
    local excluded path nl=$'\n'
    excluded=$(consist_exclusions) || return 1
    excluded=$(printf '%s\n' "$excluded" | cut -f1)
    if [ ! -f "$PREFLIGHT_FIRST" ]; then
        echo "gate.sh: the pre-flight roster names a lint that does not exist: $PREFLIGHT_FIRST" >&2
        return 1
    fi
    echo "$(basename "$PREFLIGHT_FIRST" .sh) $PREFLIGHT_FIRST"
    for path in $(LC_ALL=C ls infra/lint/*.sh); do
        [ "$path" = "$PREFLIGHT_FIRST" ] && continue
        case "${nl}${excluded}${nl}" in *"${nl}${path}${nl}"*) continue ;; esac
        echo "$(basename "$path" .sh) $path"
    done
}

# THE LISTINGS ANSWER BEFORE EVERY REFUSAL. `--roster` and `--exclusions`
# read no tree and run no check: they list infra/lint/ and what its
# headers declare, and the conductor's consist check asks the assembled
# tree's gate.sh for exactly that. Until 2026-09-18 this dispatch sat
# BELOW the disk floor, so at 9GB free on the conductor's volume
# `gate.sh --exclusions` was refused with "9GB free, need 12GB.
# Refusing to start." and the consist check recorded a failure it could
# not judge (backlog 13700f6f; CLAUDE.md Diagnosis: an infrastructure
# refusal is not a consist failure). The floor guards a gate RUN — a
# build that fills the volume — not a question about the roster; and
# the untracked-files refusal (`refuse_untracked_files`, the other
# exit-2 before a check runs) is asked only by the pre-flight modes,
# which certify a tree, where a listing certifies nothing. So the two
# listings dispatch here, in one place, ahead of both — pinned by
# gate_sh.rs `the_listings_answer_below_the_disk_floor`.
if [ "$ROSTER" -eq 1 ]; then
    preflight_roster
    exit $?
fi

# `<path>\t<why>` per excluded lint, in directory order — the set the
# roster above leaves out and the reason each lint gave. This is the
# print the conductor's consist check and gate_sh.rs both read.
if [ "$EXCLUSIONS" -eq 1 ]; then
    consist_exclusions
    exit $?
fi

# DISK FLOOR, before anything compiles — and after the listings above,
# which compile nothing.
#
# On 2026-08-16 this box ran out of disk mid-`cargo build`. The failure
# was not a build error: the volume filled, the tool harness could no
# longer create a file to hold a command's output, and NOTHING would
# run — not the gate, not `df`, not the cleanup. Recovering it needed a
# human at a terminal.
#
# locomotive.sh has had exactly this check for the CI runner since
# 2026-08-14 (`min_free_gb`, default 70), written after a full disk
# surfaced as four unrelated boss-ledger tests failing on "could not
# extend file" and cost an hour of archaeology. Three hosts run builds
# — the runner, the dev pod, this box — and only the runner could say
# "not enough disk" before spending twenty minutes finding out. That
# asymmetry is the defect (CLAUDE.md 9a), not the number.
#
# A FLOOR, not a prediction. A cold workspace build measured ~74GB on
# the forge host; an incremental one on a warm target is a fraction of
# that, so a gate floor set at 74 would refuse almost every honest run.
# 12GB is set where it means something: below it a compile is likely to
# die partway and take the shell with it, and refusing costs two
# seconds instead of a wedged machine. It will not catch every
# too-tight case, and that is stated rather than papered over.
#
# The durable fix is not a bigger number: it is building in the dev pod
# (188GB on node-local scratch, one workspace instead of six worktrees
# each with their own target/). This is the guard for whichever host
# ends up running it.
#
# AND IT IS CHECKED BEFORE EVERY PHASE, not only at startup. A one-shot
# precondition cannot catch the failure it was written for: the run
# STARTS above the floor and then grows a `target/` — 32GB in the
# 2026-08-16 incident, 81GB on the Mac when this poll was added — so
# the only reading that matters is the one taken while the build is
# under way. `check()` is where every phase passes through, which makes
# it the poll point; the cost is one `df` per phase against a gate whose
# phases run for minutes.
#
# Tripping mid-run EXITS rather than recording a failed check. A gate
# that keeps going after this consumes the disk it just warned about,
# and the reported incident is precisely what that costs: the harness
# could no longer create the file it writes command output into, so
# `df` and `rm` stopped working too and the failure had disabled its
# own diagnosis.
gate_min_free_gb="${BOSS_GATE_MIN_FREE_GB:-12}"

# The free-space reader, overridable ONLY so the poll itself can be
# tested — a fake that shrinks across calls is a faithful model of the
# incident, and there is no other way to prove re-evaluation without
# actually filling a disk.
gate_avail_gb() {
    local kb
    kb="$(${BOSS_GATE_DF_CMD:-df -Pk .} 2>/dev/null | awk 'NR==2 {print $4}')"
    if [ -z "${kb}" ]; then
        echo "gate: could not read free space for $(pwd) — refusing rather than guessing." >&2
        exit 2
    fi
    echo $((kb / 1024 / 1024))
}

# `when` is "to start" or "to continue" — the distinction matters when
# reading a log: the second means the run itself ate the headroom.
require_headroom() {
    local when="$1" avail
    avail="$(gate_avail_gb)" || exit 2
    if [ "${avail}" -ge "${gate_min_free_gb}" ]; then
        return 0
    fi
    echo "gate: ${avail}GB free, need ${gate_min_free_gb}GB. Refusing ${when}." >&2
    echo "  A build that fills this volume does not fail cleanly — it wedges the" >&2
    echo "  shell, and on 2026-08-16 it stopped even \`df\` from running." >&2
    echo "  remediation: drop target/ dirs from landed worktrees —" >&2
    echo "    du -sh */target | sort -hr        # the usual culprits" >&2
    echo "    rm -rf <landed-worktree>/target" >&2
    echo "  Better: build in the dev pod, which has 188GB of scratch." >&2
    # A REFUSAL IS NOT A FAILURE, and the receipt has to say which.
    #
    # The distinction already lived in the exit code — `exit 1` after
    # `write_receipt "failed"` means "I ran the checks and the branch is
    # bad"; `exit 2` means "I declined to run" — but a refusal wrote no
    # receipt at all, so every reader downstream saw only a dead run and
    # guessed. On 2026-09-02 that guess cost two trains: CI reported a
    # plain failure, the conductor recorded it as a verdict on the
    # consist, and the cars aboard were one auto-cancel away from taking
    # strikes for a full disk on the host. Two strikes hold a car out of
    # the queue until a human looks.
    #
    # `refused` is deliberately its own word, not a flavour of failed: a
    # reader that only knows green/failed must not silently round this
    # to either. Written before exiting so the reason survives the
    # process.
    GATE_REFUSAL="${avail}GB free, need ${gate_min_free_gb}GB (${when})"
    # The startup call happens before `write_receipt` and GATE_RECEIPT
    # exist — nothing has run yet, so there is no receipt to write and
    # the exit code is the whole signal. The mid-run calls (one before
    # every phase) are the ones a reader needs, and by then both are
    # defined. Guarding on the function keeps that honest instead of
    # emitting a half-built receipt.
    if declare -F write_receipt >/dev/null 2>&1; then
        write_receipt "refused"
    fi
    exit 2
}

require_headroom "to start"

# ---------------------------------------------------------------------
# `-p` states a belief; the tree states a fact
# ---------------------------------------------------------------------
# Car mode asks the author which crates they changed, and on 2026-08-16
# the answer was wrong in the way that matters. A docs branch was gated
# `-p boss-docs` while `git add -A` had also swept an uncommitted
# crates/core/boss-jobs change into the commit — so the gate compiled
# the crate the author believed they touched, missed two independent
# defects in the one they had, and a three-car train went red on
# clippy (a6ffcb7c).
#
# The fix is not a new flag to remember. A flag you have to remember is
# the folklore this repo keeps paying for; the check has to fire
# exactly when `-p` is used, which is when the belief is being stated.
# So: derive the crate set from the tree and refuse a `-p` that does
# not cover it.
#
# Derivation is deliberately dumb — a path under crates/<tier>/<name>/
# means <name>. Two extras earn their place:
#
#   docs/design/ used to map to boss-docs, because those markdown files
#       were INPUT to a corpus gate: boss-docs/tests/docs_corpus_presents.rs
#       parsed every one, so a docs-only change really could fail a
#       crate's tests. That crate, the test and the corpus index behind
#       it were deleted on 2026-09-10 (backlog f5da586c) — the packet is
#       the doc — so docs/design/ now implies no crate, like every other
#       path under docs/. The rule it illustrated still holds for the
#       data files below: path-to-crate is not the same question as
#       which SOURCE files a crate compiles.
#
#   Data files some crate's test READS, so changing one can redden that
#   crate without touching a line of its source:
#     infra/gate.sh, infra/lint/*, .forgejo/workflows/ci.yml
#         -> boss-testing, which owns gate_sh.rs. That test pins that
#            ci.yml invokes this script, that this script runs every
#            check, and that every infra/lint/*.sh either runs in the
#            pre-flight or is named in its exclusion set. Omitting these would let `--auto` skip the only
#            test guarding the file being edited — which this very car
#            would have done to itself.
#            infra/lint/* ALSO implies boss-cli (added with backlog
#            294bb7c9): train/consist.rs's `the_roster_is_the_lint_directory
#            _itself` reads infra/lint/ and asserts every lint the
#            delivery policy excuses is STILL a file there, so deleting
#            or renaming a lint reddens the conductor's consist check.
#            The gate already compiles boss-testing for these paths;
#            compiling the other reader too is the cheap half.
#     infra/dispatcher/rules/*.toml -> boss-dispatcher, which owns
#            dispatcher_rules_seed.rs. It compares the seeded registry
#            against that directory in BOTH directions, and skipping the
#            authored half is what reddened the 13-car train
#            20260815-0621.
#            AND boss-brewery-engine, decided with backlog 294bb7c9.
#            protocol_holds_e2e.rs's `overhead_absorption_rules_agree`
#            reads three NAMED rule files and asserts the three overhead
#            drivers and their rates match the brewery's own table — a
#            §9a pin on a fact that lives twice, whose own doc comment
#            says "change a rate → change it here + in the rule's own
#            file". Scoped out, a rate edit passes the gate and the two
#            halves disagree silently. It is listed rather than derived
#            because the test assembles the filename at run time
#            (`rules_dir.join(format!("{name}.toml"))`), so no scan of
#            the source can see which rule files it reads; the derivation
#            below finds literals, not format strings.
#     examples/<tenant>/seeds/* -> boss-jobs for workflows.toml (its
#            seed_loader parses BOTH tenants' bundles through the
#            viability lint), boss-sim for tenant.toml (seven of its
#            shape-driven unit tests load that exact file),
#            boss-policy-client for policy_rules.toml (its loader's unit
#            tests parse both tenants' grants), and
#            boss-<tenant>-engine for anything in the bundle — four of
#            the brewery's TOMLs are `include_str!`d into that crate, so
#            they are compile input, and its layer-1 lint test reads the
#            directory. This line was missing until backlog b59efe54:
#            the rule above it was written for infra/platform/workflows
#            and the tenant's equivalent never got a line beside it, so
#            a bundle-only car scoped to ZERO crates and the one test
#            that can reject a broken predicate never ran on it. The
#            tenant name is DERIVED from the directory, never listed — a
#            brewery-only rule would have left the same hole for the
#            used-device-shop bundle and its 36 kinds.
#
#   Anything else (infra/, apps/, .forgejo/) maps to no crate and is
#       REPORTED rather than ignored. The lints already run repo-wide,
#       so there is nothing to scope — but the author should see the
#       list, because a file they did not expect is the whole warning.
changed_paths() {
    # THE QUESTION IS "what will this car land", and that has three
    # answers depending on where the author is in the loop. Asking only
    # the first two is a bug I shipped: `--auto` derived from the
    # WORKING TREE alone, so gating after a commit — or after a rebase,
    # which is when you most want to re-check — found a clean tree,
    # scoped to nothing, skipped every cargo phase and reported
    # "all checks green". A gate that runs nothing must never say that.
    #
    # Staged first: that is what a commit will actually carry.
    local staged
    staged=$(git diff --cached --name-only 2>/dev/null)
    if [ -n "$staged" ]; then
        printf '%s\n' "$staged"
        return
    fi
    # Then the working tree, for the common case of gating before
    # `git add`.
    local dirty
    dirty=$({ git diff --name-only 2>/dev/null
              git ls-files --others --exclude-standard 2>/dev/null; })
    if [ -n "$dirty" ]; then
        printf '%s\n' "$dirty"
        return
    fi
    # Finally the commits this branch adds over the trunk. A clean tree
    # on a branch with commits is not "no change" — it is a car that is
    # ready, which is exactly when it gets gated.
    local base
    base=$(git merge-base "$AUTO_TRUNK" HEAD 2>/dev/null) || return 0
    [ -n "$base" ] || return 0
    git diff --name-only "$base" HEAD 2>/dev/null
}

# Paths whose correctness ONLY a database can judge. A migration is
# valid SQL long before it is a valid migration: ordering against a
# unique index, agreement with the registry seed it duplicates, and
# whether it applies at all on top of the migrations already recorded
# are all invisible to shape lints and to `bash -n`.
#
# This list is the answer to three red trains on 2026-08-18, every one
# of them from a car whose local gate read "26 of 28 — the two
# failures are the absent local Postgres". That sentence was true and
# the car was still broken; the receipt gave no way to tell those
# apart, so the author (me) supplied the optimistic reading each time.
db_backed_paths() {
    changed_paths | grep -E '^infra/postgres/schema/|^infra/dispatcher/rules/[^/]*\.toml$|/seeds/[^/]*\.toml$' || true
}

# Did the checks that need a live database actually pass? `fixture`
# standing up IS the database being reachable, so a fixture failure
# means every DB-backed result below it is absent rather than green.
db_checks_passed() {
    local entry name result seen=0
    for entry in ${RAN+"${RAN[@]}"}; do
        name="$(ran_name "$entry")"; result="$(ran_result "$entry")"
        case "$name" in
            fixture|test) seen=1; [ "$result" = "pass" ] || return 1 ;;
        esac
    done
    [ "$seen" -eq 1 ]
}

crates_from_paths() {
    changed_paths | path_map | tr ' ' '\n' | sed '/^$/d'
}

# Following invariant-register.sh and no-secrets.sh: a check that
# cannot demonstrate itself is a check nobody can trust. This one is
# pure string work, so it runs every time car mode does — a rule that
# only self-tests when asked is a rule that stops working quietly.
#
# The fixtures are path lists rather than real trees on purpose. The
# rule under test is paths -> crates; staging files would test git.
# ---------------------------------------------------------------------
# Files a crate READS: derived from the tree, never listed here
# ---------------------------------------------------------------------
# The rules below are hand-written path shapes, and hand-written shapes
# only cover the couplings somebody noticed. On 2026-09-10 a survey found
# the cost (backlog 294bb7c9): nine infra paths that a crate compiles in
# or a test executes mapped to NO crate, and one —
# infra/deploy-services.sh — was positively asserted to map to none while
# boss-ports `include_str!`s it. A car editing it scoped away the very
# test CLAUDE.md §9a cites as the fix for "two services silently absent
# from a deploy".
#
# So don't list them: DERIVE them. A crate that reads a file names that
# file, and the name is in the source. Two scans answer it, and each is
# one fact, not a roster:
#
#   A COMPILE INPUT escapes its own crate. `include_str!("../../../..
#   /infra/deploy-services.sh")`, a build.rs `.join("../../../infra/
#   estate/observe-lib.sh")`, a `#[cfg(test)]` read of `../../../infra/
#   gate-runner/gate-runner.yaml` — all are relative literals that climb
#   out of the crate, which is a shape nothing else has. Anywhere in the
#   crate's .rs, because `include_str!` and build.rs are not confined to
#   tests/.
#
#   A TEST SUBJECT is named repo-relative. boss-testing's script tests do
#   `read("infra/ops/ops-runner.sh")` from a root joined separately, so
#   there is no `../` to key on — but a plain repo path literal inside a
#   crate's tests/ directory that names an existing file is a fixture by
#   construction. Restricted to tests/ deliberately: the same literal in
#   src/ is usually prose (boss-content's 503 body mentions
#   infra/deploy-services.sh in a help string, and boss-policy's and
#   boss-jobs' port defaults mention it in comments), and mapping those
#   would compile three crates for a shell-script edit.
#
#   A PATH BEING JOINED is a path being read, wherever it sits. The
#   two shapes above left one idiom uncounted: a `#[cfg(test)]` module
#   under src/ reading `boss_testing::repo_root().join("docs/
#   tenant-contract.md")` — repo-relative, so not escaping, and not in
#   tests/. On 2026-09-19 train #470's car 4f1ba1f9 edited that doc
#   without editing the CONTRACT it is held equal to; the pin lives in
#   boss-cli's src/tenant.rs, the car derived boss-gateway alone, the
#   train gate derived the same, and origin/main went red on the pin
#   the first time anything ran it, striking the next car gated on top
#   (backlog 1b52c278). Measured at #471: eight (path, crate) pairs sat
#   outside the index this way (estate.toml, sor-ports.env,
#   pod-build.env, as-gate-uid.sh, access.toml, tax.toml, the contract
#   doc). What separates them from the prose the src/ restriction
#   exists for is the call around the literal: `.join("…")` is a path
#   being built, and a sentence never sits inside one. So a literal
#   whose immediate prefix is `.join(` counts in src/ too — still
#   subject to the existence test, so `dir.join("seeds/x.toml")` on a
#   scratch base maps to nothing because no such file sits at the root.
#
# RESOLVED AGAINST TWO BASES because the two idioms differ:
# `include_str!` is relative to the source FILE, while
# `env!("CARGO_MANIFEST_DIR").join("../../../x")` is relative to the
# crate ROOT. A literal that lands on a real file under either is a
# reference to it; one that lands nowhere (`"../../etc/passwd"` in a
# traversal test, `"../fixtures/upstream.git"` in an assertion) is not,
# and the existence test is what separates them — no allow-list.
#
# TWO EXCLUSIONS, both measured, both deliberate:
#
#   infra/postgres/schema/** — boss-testing's build.rs compiles every
#   migration in, and boss-jobs names one in a test, so this scan would
#   map migrations to those TWO crates — an under-count, and a wrong
#   shape: a migration is read by every crate that stands up the
#   schema, not by the ones that mention it by path. `schema_readers`
#   below derives that set, and `path_map` applies it to any change
#   under this tree (backlog 4711828d).
#
#   docs/design/** — boss-jobs' subject_existence_pg.rs uses
#   "docs/design/subject-identity-and-relationships.md" as a Subject ID,
#   not as a file it reads. It is a path-shaped string that happens to
#   name a real file, so the existence test cannot tell it apart, and
#   including it would compile boss-jobs for any docs-only car. The one
#   real dependency in the docs tree (gate_sh.rs reads
#   docs/runbooks/dev-environment-bootstrap.md) is outside design/ and is
#   picked up normally.
#
# Cost: one awk pass over 843 .rs files, ~0.12s, once per invocation.
# `[ -f ]` is a builtin, so filtering the candidates costs no processes.
#
# `/dev/null` is passed to awk as a guaranteed file argument, and it is
# not decoration: GNU xargs runs the command once even with no input, and
# an awk with no file arguments reads STDIN — which here is the gate's own
# stdin. That is the same defect this car fixes on the other side (a
# check inheriting the gate's stdin), so the scanner must not commit it.
# A fixed file argument closes it portably, where `xargs -r` would not
# (BSD xargs has no -r).
file_input_index() {
    local path crate
    find crates -name '*.rs' -print0 2>/dev/null | xargs -0 awk '
        # `a/b/../c` -> `a/c`, iteratively, with no realpath: GNU
        # realpath --relative-to does not exist on a Mac, and the Mac
        # gate and the runner must derive the same scope.
        function resolve(base, rel) {
            if (rel !~ /^\.\.\//) return rel
            while (sub(/^\.\.\//, "", rel)) {
                if (base ~ /\//) sub(/\/[^\/]*$/, "", base); else base = ""
            }
            return (base == "" ? rel : base "/" rel)
        }
        FNR == 1 {
            crate = ""
            n = split(FILENAME, p, "/")
            if (n >= 4 && p[1] == "crates") {
                crate = p[3]
                root = p[1] "/" p[2] "/" p[3]
                dir = FILENAME
                sub(/\/[^\/]*$/, "", dir)
                intests = (index(FILENAME, root "/tests/") == 1)
            }
        }
        crate == "" { next }
        /^[[:space:]]*\/\// { next }
        {
            rest = $0
            while (match(rest, /"[^"]*"/)) {
                spec = substr(rest, RSTART + 1, RLENGTH - 2)
                joined = (substr(rest, 1, RSTART - 1) ~ /\.join\($/)
                rest = substr(rest, RSTART + RLENGTH)
                if (spec !~ /\//) continue
                # A literal with whitespace in it is a sentence that
                # mentions a path, not a path.
                if (spec ~ /[[:space:]]/) continue
                # Outside tests/, only an ESCAPING literal counts —
                # or one being `.join`ed onto a base (backlog 1b52c278).
                if (!intests && !joined && spec !~ /^\.\.\//) continue
                print resolve(dir, spec) " " crate
                print resolve(root, spec) " " crate
            }
        }
    ' /dev/null | sort -u | while read -r path crate; do
        case "$path" in
            crates/*|infra/postgres/schema*|docs/design/*) continue ;;
        esac
        if [ -f "$path" ]; then printf '%s %s\n' "$path" "$crate"; fi
    done
}

# Computed ONCE and read by `path_map` out of the environment: path_map
# runs inside a command substitution on every call, and the self-test
# calls it a few dozen times.
GATE_FILE_INPUTS="$(file_input_index)"

# ---------------------------------------------------------------------
# Crates that READ THE SCHEMA: derived from the tree, never listed here
# ---------------------------------------------------------------------
# A migration is a change to every crate whose tests stand the schema
# up. On 2026-09-16 two cars each added a credentials-registry
# migration; `--auto` scoped each to the crates whose FILES changed
# (boss-dispatcher-handlers, boss-testing) and never ran boss-jobs,
# whose credentials_pg.rs pins the seeded rows. Both gated green
# (receipts 4c2f15c5, 719b6e61), the train gate ran boss-jobs on the
# assembled tree, and all six cars aboard were struck (train 767cfb14,
# gate-run 06995f6c; backlog 4711828d). Until then a migration-only car
# ran "fixture + lints only": the fixture proves the schema APPLIES,
# and nothing proved that what it seeds still agrees with the tests
# that read it.
#
# THE PREDICATE IS THE CONSTRUCTOR, not a feature flag or a file name.
# `boss_testing::TestDb::new` (and `new_without`) is the one call that
# applies infra/postgres/schema/ to a fresh database, so a crate that
# calls it anywhere in src/ or tests/ reads every migration. Measured
# on 2026-09-16 against the alternatives: "declares a `postgres`
# feature AND has tests/*_pg.rs" names 13 crates and drops
# boss-dispatcher (no feature, stands up a TestDb in rules_wait_pg.rs)
# and eleven others whose DB tests are named differently; the
# constructor names 25 — which is the whole Pg-tested workspace, and
# that is the honest answer, not a cost to optimise away. Correctness
# over speed: a migration-only gate now runs those 25 crates instead of
# none, ~2 minutes to a workspace-shaped run.
#
# Members come from `cargo metadata --no-deps` (~0.05 s, no resolution,
# no network), so a crate outside the workspace cannot be named and a
# `-p` it produces is one cargo can satisfy. The crate name is the
# manifest's directory, the same convention `path_shapes` reads off
# `crates/<tier>/<name>/`, and `scope_self_test` refuses a name that is
# not a crate. ~0.12 s in total, once per invocation, in every mode —
# `--quick` derives no scope and never reads it, but the cost is the
# same either way and a conditional would be a second code path to
# keep honest.
schema_readers() {
    local dir
    cargo metadata --no-deps --format-version 1 2>/dev/null \
        | grep -o '"manifest_path":"[^"]*/Cargo.toml"' \
        | sed -e 's|^"manifest_path":"||' -e 's|/Cargo.toml"$||' \
        | while read -r dir; do
            if grep -rlq --include='*.rs' 'TestDb::new' "$dir/src" "$dir/tests" 2>/dev/null; then
                basename "$dir"
            fi
        done | sort -u | tr '\n' ' '
}
GATE_SCHEMA_READERS="$(schema_readers)"
GATE_SCHEMA_READERS="${GATE_SCHEMA_READERS% }"

# The paths on stdin that are migrations, one per line. Empty when the
# change touches none.
schema_paths() {
    grep -E '^infra/postgres/schema/' || true
}

# The schema half of the map: a change under infra/postgres/schema/
# implies every reader. Reads the readers out of the environment for
# the same reason `input_crates` does.
schema_crates() {
    if [ -n "$(schema_paths)" ]; then printf '%s\n' "${GATE_SCHEMA_READERS}"; fi
}

# The derived half of the map: which crates read the paths on stdin.
input_crates() {
    awk -v idx="${GATE_FILE_INPUTS}" '
        BEGIN {
            n = split(idx, rows, "\n")
            for (i = 1; i <= n; i++) {
                split(rows[i], f, " ")
                if (f[1] != "") map[f[1]] = map[f[1]] " " f[2]
            }
        }
        $0 != "" && ($0 in map) { print map[$0] }
    '
}

path_map() {
    # Stdin is read ONCE and handed to all three parts: the hand-written
    # shapes below, the file-input derivation above that reads the
    # tree, and the schema readers.
    local paths
    paths="$(cat)"
    { printf '%s\n' "$paths" | path_shapes
      printf '%s\n' "$paths" | input_crates
      printf '%s\n' "$paths" | schema_crates
    } | tr ' ' '\n' | sed '/^$/d' | sort -u | tr '\n' ' '
}

path_shapes() {
    # An expression REWRITES the pattern space, so the one that matches
    # first wins and the later ones never see the original path. That is
    # why the two specific bundle files sit above the catch-all below
    # them. A rule may name more than one crate, space-separated; the
    # split happens before the dedupe so a crate implied twice is still
    # named once.
    sed -n -e 's|^crates/[^/]*/\([^/]*\)/.*|\1|p' \
           -e 's|^infra/gate\.sh$|boss-testing|p' \
           -e 's|^infra/lint/.*|boss-cli boss-testing|p' \
           -e 's|^\.forgejo/workflows/ci\.yml$|boss-testing|p' \
           -e 's|^infra/dispatcher/rules/[^/]*\.toml$|boss-brewery-engine boss-dispatcher|p' \
           -e 's|^infra/platform/[^/]*/[^/]*\.toml$|boss-jobs|p' \
           -e 's|^examples/\([^/]*\)/seeds/workflows\.toml$|boss-jobs boss-\1-engine|p' \
           -e 's|^examples/\([^/]*\)/seeds/tenant\.toml$|boss-sim boss-\1-engine|p' \
           -e 's|^examples/\([^/]*\)/seeds/policy_rules\.toml$|boss-policy-client boss-\1-engine|p' \
           -e 's|^examples/\([^/]*\)/seeds/.*|boss-\1-engine|p'
}


scope_self_test() {
    local fails=0 label want got seeds tenant bundle_dir
    _case() {
        label="$1"; want="$2"; shift 2
        got=$(printf '%s\n' "$@" | path_map); got="${got% }"
        if [ "$got" != "$want" ]; then
            echo "gate.sh scope self-test FAIL: ${label} -> [${got}], wanted [${want}]" >&2
            fails=1
        fi
    }
    # The commit this rule was written for: a docs title over a
    # boss-jobs change (a6ffcb7c). The docs file contributed a crate of
    # its own until 2026-09-10; the defect it caught was never about
    # that, it was about the boss-jobs change riding along unnamed.
    _case "the commit that earned this rule" "boss-jobs" \
        "docs/design/queue-visibility.md" \
        "crates/core/boss-jobs/src/registry.rs" \
        "crates/core/boss-jobs/tests/platform_bundle.rs"
    # No crate parses the design corpus any more (boss-docs and its
    # docs_corpus_presents.rs went on 2026-09-10), so a docs-only car
    # has nothing to compile — the lints still run repo-wide.
    _case "a genuinely docs-only car" "" "docs/design/payload-encryption.md"
    # The platform bundle is DATA, but boss-jobs compiles a test that
    # parses and lints it (`the_platform_bundle_matches_the_specs_it
    # _replaced`). Without this line a protocol-only car scoped to
    # "lints + fmt only" and never ran the one test that can reject it
    # — which is how correct-the-record's second defect nearly shipped:
    # the bundle lint caught a free-text fork with no fallback, and the
    # gate would not have run that lint at all.
    _case "a protocol-only car still has a crate" "boss-jobs" \
        "infra/platform/workflows/ship-a-change.toml"
    # THE THIRD RE-PIN (backlog f532c345). The line above was written
    # when infra/platform/ held one registry, and the shape it pinned
    # named `workflows/`. On 2026-09-18 the bundle grew stations/ and
    # step-plugins/ (H4 cars 1 and 3), each held equal to the migrations
    # by a `*_bundle_is_the_migrations_pg.rs` pin in boss-jobs — and a
    # row in either derived NO crate (measured on main at #452:
    # stations/repair.toml -> [], step-plugins/sign-off.toml -> []), so
    # a bundle-only car gated lints + fmt and never ran the pin. The
    # three bundle cars gated boss-jobs only because each also changed
    # Rust. Same hole as the tenant bundle two cases down, same fix: the
    # shape is now `infra/platform/<registry>/*.toml` — the directory,
    # not a list of names — and this loop walks the directory so a
    # fourth registry is pinned the day it appears. A fictional row, not
    # a real one, so the answer is the shape's alone and not the
    # file-input index's.
    for bundle_dir in infra/platform/*/; do
        [ -d "$bundle_dir" ] || continue
        bundle_dir="${bundle_dir%/}"
        _case "a row in ${bundle_dir} implies boss-jobs" "boss-jobs" \
            "${bundle_dir}/zz-a-scratch-row.toml"
    done
    # …and the bundle's prose does not: a README beside the rows is read
    # by nobody a compile can reach, so it must not cost a boss-jobs
    # build. The shape is keyed on `.toml`, and this is the case that
    # keeps it so.
    _case "a README in a platform bundle implies no crate" "" \
        "infra/platform/stations/README.md"
    _case "two files, one crate" "boss-cli" \
        "crates/orchestrators/boss-cli/src/train/conductor.rs" \
        "crates/orchestrators/boss-cli/src/gate.rs"
    # The tier segment must not be mistaken for the crate name.
    _case "tier is not the crate" "boss-people" "crates/modules/boss-people/src/http.rs"
    _case "a crate's root files count" "boss-jobs" "crates/core/boss-jobs/Cargo.toml"
    # Everything outside those two trees implies nothing to scope —
    # the lints already run repo-wide.
    # gate.sh and ci.yml are READ by boss-testing's gate_sh.rs, so a
    # change to either must compile and run that crate. Since
    # docs/design/a-probe-shape-follows-the-car.md, gate.sh is ALSO read
    # by boss-jobs/tests/a_probe_shape_follows_the_car.rs (the receipt
    # shape is one of the four facts it pins), and the file-input index
    # above derives that on its own — this line is the pinned copy of a
    # derived fact, so it moves whenever a new crate starts reading the
    # gate's files, and a red here after adding such a reader is the
    # index being right.
    _case "the gate's own files imply boss-testing" "boss-cli boss-jobs boss-testing" \
        "infra/gate.sh" ".forgejo/workflows/ci.yml" "infra/lint/no-secrets.sh"
    _case "a dispatcher rule file implies boss-dispatcher" "boss-brewery-engine boss-dispatcher" \
        "infra/dispatcher/rules/converge-on-merge.toml"
    # A TENANT seed bundle is the same shape as the platform bundle one
    # case up, and it was missed for the same reason: the rule was
    # written for infra/platform/workflows and the tenant's equivalent
    # never got a line beside it (backlog b59efe54). boss-jobs parses
    # both tenants' workflows.toml through the viability lint
    # (`round_trips_brewery_seed_bundle`,
    # `round_trips_used_device_shop_seed_bundle`), and the tenant's own
    # engine parses it again in its layer-1 lint test — so a bundle-only
    # car that scoped to no crate ran neither, and a broken predicate or
    # a missing terminal would have gated GREEN on its way to the live
    # registry. boss-testing since 2026-09-17 (backlog 18d6a6c9):
    # the_step_type_registry_names_no_tenant_role.rs reads this bundle
    # to hold the four steps that once relied on the platform's
    # required_roles to their own authority_role.
    _case "a tenant seed bundle still has a crate" "boss-brewery-engine boss-jobs boss-testing" \
        "examples/brewery/seeds/workflows.toml"
    # Derived from the directory, not a list of tenants: the
    # used-device-shop bundle declares 36 kinds and must be covered by
    # the same line, without that line naming either tenant.
    _case "the sibling tenant needs no line of its own" "boss-jobs boss-used-device-shop-engine" \
        "examples/used-device-shop/seeds/workflows.toml"
    # The rest of a bundle is its tenant engine's business: four of the
    # brewery's TOMLs are `include_str!`d into boss-brewery-engine, so
    # they are compile INPUT, and its e2e test reads the whole directory.
    _case "the rest of a tenant bundle implies its engine" "boss-brewery-engine" \
        "examples/brewery/seeds/vendors.toml"
    # policy_rules.toml is parsed by boss-policy-client's own unit tests
    # (`brewery_seed_parses`, `used_device_shop_seed_parses`) — the
    # privilege model every write passes through, so a malformed grant
    # must not reach the seed with nothing compiled against it.
    _case "a tenant policy bundle implies the policy loader" "boss-brewery-engine boss-policy-client" \
        "examples/brewery/seeds/policy_rules.toml"
    # tenant.toml is boss-sim's parse fixture — seven of its unit tests
    # load this exact file, so a shape change there reddens the sim.
    # boss-testing since 2026-09-17 (backlog b03f38de):
    # generate_configs_sh.rs runs the config generator against this
    # manifest and asserts the brewery's id is what turns [demo_agents]
    # on, so an id change there must run that test too.
    _case "a tenant manifest implies the sim that parses it" "boss-brewery-engine boss-sim boss-testing" \
        "examples/brewery/seeds/tenant.toml"
    # A bundle edit beside a boss-jobs edit must name boss-jobs ONCE:
    # these rules emit more than one crate, so the split has to happen
    # before the dedupe.
    _case "a crate named twice is named once" "boss-brewery-engine boss-jobs boss-testing" \
        "examples/brewery/seeds/workflows.toml" \
        "crates/core/boss-jobs/src/seed_loader.rs"
    # What is deliberately NOT mapped: everything in examples/ outside a
    # seed bundle. The domain docs compile nowhere, and the data/ JSON
    # rosters are read best-effort (`if let Ok(...)`) by the engines, so
    # a malformed one degrades rather than failing a test.
    _case "examples outside a seed bundle imply no crate" "" \
        "examples/used-device-shop/DOMAIN.md" "examples/brewery/data/assets.json"
    # Infra no crate READS. Both are real scripts, and that is the point:
    # "unmapped" has to be a fact about the tree, not a fact about which
    # paths nobody got round to listing.
    _case "other infra implies no crate" "" \
        "infra/forge/locomotive.sh" "infra/forge/cluster-watchdog.sh"
    # THE RE-PIN (backlog 294bb7c9). Until that car, the case above also
    # asserted `infra/deploy-services.sh` implies no crate — and that
    # answer was WRONG, not merely incomplete: boss-ports `include_str!`d
    # that script, so it was a COMPILE INPUT, and the gate was scoping
    # out the documented mechanism for a defect that had already bitten.
    # Nothing was missing; the wrong answer was asserted, which is why it
    # needed un-asserting rather than adding to. The fixture moved to
    # host-absent-tools.txt when the bare-metal deploy script was deleted
    # (2026-09-18, e109bd71); boss-jobs' probe.rs `include_str!`s it.
    _case "a file a crate include_str!s is that crate's compile input" \
        "boss-jobs" "infra/forge/host-absent-tools.txt"
    # A build script's read is a compile input too: boss-dispatcher-
    # handlers' build.rs `.expect`s infra/estate/observe-lib.sh to exist
    # and compiles its text in.
    _case "a build script's input implies its crate" \
        "boss-dispatcher-handlers" "infra/estate/observe-lib.sh"
    # Two crates read this manifest — boss-cli defaults to it and asserts
    # against the real file, boss-testing's gate_runner_manifests.rs pins
    # it — and the map owes both, not whichever was noticed first.
    _case "a file two crates read implies both" \
        "boss-cli boss-testing" "infra/gate-runner/gate-runner.yaml"
    # The wide set the old map simply omitted: boss-testing owns tests
    # that EXECUTE infra scripts, and the map gave it only infra/gate.sh
    # and infra/lint/*. Editing one of these scoped to NO crate, so the
    # only test that runs the script never ran on the car that changed it.
    _case "a script boss-testing executes implies boss-testing" "boss-testing" \
        "infra/ops/ops-runner.sh" "infra/forge/checkout-lock.sh" \
        "infra/maintenance/forge-token-audit.py" "infra/prep-github-publish.sh"
    _case "docs outside design/ imply no crate" "" "docs/invariants/x.toml" "README.md"
    # …unless a crate READS it. gate_sh.rs asserts this runbook tells a
    # developer to set core.hooksPath, so editing the runbook can redden
    # boss-testing. Derived, not listed — the point of the rule is that
    # nobody had to notice this one.
    _case "a runbook a test reads implies that crate" "boss-testing" \
        "docs/runbooks/dev-environment-bootstrap.md"
    # THE FOURTH RE-PIN (backlog 1b52c278). The runbook above is read
    # from boss-testing's tests/, which the index always counted; this
    # doc is read from a #[cfg(test)] module under boss-cli's src/
    # through repo_root().join(...), which it did not — so a docs-only
    # car derived NO crate, gated lints-only, and train #470 landed a
    # doc edit that reddened main on the equality pin holding the doc's
    # table to CONTRACT. The `.join(` prefix is what counts it now.
    _case "a doc a crate's own test module reads by repo path implies that crate" \
        "boss-cli" "docs/tenant-contract.md"
    _case "the web app implies no crate" "" "apps/web/src/me/MePage.svelte"
    # THE SECOND RE-PIN (backlog 4711828d). Until this car the line here
    # asserted "a migration implies no crate", with a note that mapping
    # schema to a crate "would compile a crate for no reason". The
    # reason arrived on 2026-09-16: two migration cars gated green
    # without boss-jobs and struck a six-car train on its
    # credentials_pg pin. A migration implies every crate that stands
    # up the schema — derived, so the want is read from the same
    # derivation and this case pins the WIRING (path_map consults it),
    # while the checks below pin the derivation itself.
    _case "a migration implies every crate that stands up the schema" \
        "${GATE_SCHEMA_READERS}" "infra/postgres/schema/141-x.sql"
    # And beside a crate change it implies both, named once each.
    _case "a migration beside a crate change implies both" \
        "$(printf '%s\n' "${GATE_SCHEMA_READERS}" boss-expr | tr ' ' '\n' | sort -u | tr '\n' ' ' | sed 's/ $//')" \
        "infra/postgres/schema/141-x.sql" "crates/core/boss-expr/src/lib.rs"
    # The tenant-bundle rules DERIVE a crate name from the directory
    # rather than listing the two tenants, which moves the thing that
    # can rot: a third tenant whose engine crate is not
    # `boss-<dir>-engine` would make this map demand a `-p` cargo cannot
    # satisfy, and the gate would refuse with an impossible instruction.
    # So pin the derivation against the tree that defines it (CLAUDE.md
    # §9a — the directory is the definition, and a derivation that
    # cannot be collapsed any further gets a test).
    for seeds in examples/*/seeds; do
        [ -d "$seeds" ] || continue
        tenant="$(basename "$(dirname "$seeds")")"
        if [ ! -f "crates/tenants/boss-${tenant}-engine/Cargo.toml" ]; then
            echo "gate.sh scope self-test FAIL: ${seeds} maps to boss-${tenant}-engine, which is not a crate under crates/tenants/" >&2
            fails=1
        fi
    done
    # And pin the OTHER derivation the same way: the file-input index is
    # read out of the tree, so the two ways it can rot are rotting to
    # nothing and naming a crate cargo cannot build.
    #
    # A GREP THAT MATCHES NOTHING IS A MAP THAT COVERS NOTHING, and it
    # fails exactly like the defect this car fixes — silently, with every
    # infra file implying no crate. The floor is deliberately a round
    # number well under the live count rather than an exact total: an
    # exact total is a second copy of the tree (CLAUDE.md §9a) that every
    # car adding a test would have to edit.
    local idx_rows idx_path idx_crate idx_manifest idx_found
    idx_rows=$(printf '%s\n' "${GATE_FILE_INPUTS}" | grep -c '[^[:space:]]')
    if [ "$idx_rows" -lt 20 ]; then
        echo "gate.sh scope self-test FAIL: the file-input index found ${idx_rows} crate/file pairs in this tree, which is too few to be a real answer — the scan is broken and every infra path now implies no crate (backlog 294bb7c9)" >&2
        fails=1
    fi
    while read -r idx_path idx_crate; do
        [ -n "$idx_crate" ] || continue
        idx_found=0
        for idx_manifest in crates/*/"$idx_crate"/Cargo.toml; do
            [ -f "$idx_manifest" ] && idx_found=1
        done
        if [ "$idx_found" -eq 0 ]; then
            echo "gate.sh scope self-test FAIL: ${idx_path} was derived as an input of ${idx_crate}, which is not a crate — the map would demand a -p cargo cannot satisfy" >&2
            fails=1
        fi
    done <<< "${GATE_FILE_INPUTS}"
    # The schema readers rot the same two ways, plus a third: the
    # derivation could quietly key on the wrong thing and drop a crate
    # that reads the schema without saying so. Two named readers pin
    # that — boss-jobs, whose credentials_pg pin is the one the train
    # found, and boss-dispatcher, which declares no `postgres` feature
    # and stands up a TestDb anyway, so a derivation keyed on the
    # manifest instead of the constructor reds here by name. The floor
    # is a round number well under the live 25, for the reason given
    # above the index floor.
    local rd_count=0 rd_crate rd_found rd_manifest
    for rd_crate in ${GATE_SCHEMA_READERS}; do
        rd_count=$((rd_count + 1))
        rd_found=0
        for rd_manifest in crates/*/"$rd_crate"/Cargo.toml; do
            [ -f "$rd_manifest" ] && rd_found=1
        done
        if [ "$rd_found" -eq 0 ]; then
            echo "gate.sh scope self-test FAIL: ${rd_crate} was derived as a schema reader, which is not a crate — the map would demand a -p cargo cannot satisfy" >&2
            fails=1
        fi
    done
    if [ "$rd_count" -lt 10 ]; then
        echo "gate.sh scope self-test FAIL: only ${rd_count} crate(s) derived as schema readers, which is too few to be a real answer — cargo metadata or the TestDb scan is broken and a migration now implies almost nothing (backlog 4711828d)" >&2
        fails=1
    fi
    for rd_crate in boss-jobs boss-dispatcher; do
        case " ${GATE_SCHEMA_READERS} " in
            *" ${rd_crate} "*) ;;
            *) echo "gate.sh scope self-test FAIL: ${rd_crate} stands up a TestDb and was not derived as a schema reader — a migration car would gate without the crate that reads it (backlog 4711828d)" >&2
               fails=1 ;;
        esac
    done
    if [ "$fails" -ne 0 ]; then
        echo "gate.sh: the scope check cannot be trusted — fix it before relying on -p" >&2
        exit 2
    fi
}

if [ ${#NAMED[@]} -gt 0 ]; then
    scope_self_test
    IMPLIED=$(crates_from_paths)
    if [ -n "$IMPLIED" ]; then
        echo "gate: tree implies $(echo "$IMPLIED" | tr '\n' ' ')"
        MISSING=""
        for c in $IMPLIED; do
            covered=0
            for n in "${NAMED[@]}"; do [ "$n" = "$c" ] && covered=1; done
            [ "$covered" -eq 0 ] && MISSING="${MISSING} ${c}"
        done
        if [ -n "$MISSING" ]; then
            echo "" >&2
            echo "GATE REFUSED: -p names [${NAMED[*]}] but the tree also changes:${MISSING}" >&2
            if [ "$(schema_touched)" = "yes" ]; then
                echo "  (a change under infra/postgres/schema/ is a change to every crate that stands up the schema)" >&2
            fi
            echo "" >&2
            echo "Those crates would not be compiled or tested by this run. Either add" >&2
            echo "them (-p ${MISSING# }) or run the full gate. If a change is there by" >&2
            echo "accident — \`git add -A\` sweeping an unrelated edit into a car is how" >&2
            echo "this rule was earned — this is the moment to notice." >&2
            exit 2
        fi
    fi
fi

# ---------------------------------------------------------------------
# `--auto`: derive the scope instead of stating it
# ---------------------------------------------------------------------
# The refusal above is the SAFETY half of scoping — it stops a `-p`
# that misses a crate. This is the efficiency half, and it is worth
# having on a measured basis: of 164 live branches, 74 touch no Rust
# at all, and two of the fourteen cars shipped on 2026-08-16 were in
# that class. For those, everything cargo does is dead weight — the
# lint roster and fmt are the entire useful gate, thirty seconds
# against eight to fifteen minutes.
#
# A FLAG, not the default, because bare `infra/gate.sh` is what CI
# invokes and must keep meaning "the whole workspace,
# unconditionally". A gate that quietly narrowed itself in CI would be
# the same hole as the mis-scoped `-p` that reddened a three-car train
# (a6ffcb7c), pointed the other way.
#
# THE SCHEMA IS THE SUBTLE PART. `infra/postgres/schema/**` is not a
# crate, but the shared fixture LOADS it into every DB-backed test in
# the workspace, so a schema-only change can break any of them. Until
# 2026-09-16 the answer was "fixture + lints only": the fixture proved
# the schema applied and no test that reads it ran (backlog 4711828d —
# two such cars struck a six-car train). Now `path_map` maps a schema
# change to every crate that stands up the schema (`schema_readers`),
# so a migration-only car derives a scope like any other and the
# fixture runs unscoped ahead of it as before. What remains here is the
# question the receipt asks: did the schema move at all.
schema_touched() {
    if [ -n "$(changed_paths | schema_paths)" ]; then echo yes; else echo no; fi
}

# Which ref is "the trunk" for deriving a branch's own commits. The
# remote-tracking main this repo actually uses, with the local branch
# and an override as fallbacks — a box whose remote is named
# differently must not silently fall through to gating nothing.
AUTO_TRUNK="${BOSS_GATE_TRUNK:-}"
if [ -z "$AUTO_TRUNK" ]; then
    for candidate in gcp/forge-main origin/main main; do
        if git rev-parse --verify --quiet "$candidate" >/dev/null 2>&1; then
            AUTO_TRUNK="$candidate"
            break
        fi
    done
fi

AUTO_LINTS_ONLY=0
AUTO_SKIP_FIXTURE=0
if [ "$AUTO" -eq 1 ]; then
    scope_self_test
    DERIVED=$(crates_from_paths)
    if [ -n "$DERIVED" ]; then
        for c in $DERIVED; do SCOPE+=(-p "$c"); NAMED+=("$c"); done
        echo "gate: --auto scoping to $(echo "$DERIVED" | tr '\n' ' ')"
        # Say WHY when the schema widened it: an author who changed one
        # crate and sees twenty-six in the scope should read the reason
        # here, not infer it.
        if [ "$(schema_touched)" = "yes" ]; then
            echo "gate: --auto schema change -> $(changed_paths | schema_paths | tr '\n' ' ')is read by ${GATE_SCHEMA_READERS}"
        fi
    else
        AUTO_LINTS_ONLY=1
        AUTO_SKIP_FIXTURE=1
        local_changed=$(changed_paths | tr '\n' ' ')
        if [ -z "${local_changed// /}" ]; then
            # Nothing staged, nothing dirty, and nothing this branch
            # adds over the trunk. Refuse rather than report green:
            # "the gate passed" and "the gate had nothing to check"
            # must not look the same, and they did.
            echo "GATE REFUSED: --auto found no change at all against ${AUTO_TRUNK:-<no trunk>}." >&2
            echo "" >&2
            echo "Nothing is staged, the tree is clean, and this branch adds no commit" >&2
            echo "over the trunk — so there is nothing to scope and nothing to check." >&2
            echo "If that is wrong, the trunk ref is: ${AUTO_TRUNK:-<none found>}." >&2
            echo "Set BOSS_GATE_TRUNK to the right one, or run the full gate." >&2
            exit 2
        fi
        echo "gate: --auto — nothing changed implies a crate; lints + fmt only"
        echo "gate: (changed: ${local_changed})"
    fi
fi


FAILED=()
# Every check, how it went, and HOW LONG IT TOOK, so the receipt can say
# what RAN rather than only what broke — and can tell a starved check
# from a broken one.
#
# One entry is `name:result:seconds`. The duration is the field that was
# missing: a web unit test stalling ~8s under two parallel gates reddened
# a car that had nothing wrong with it, and the receipt could not say so.
# Without a number, "slow" and "wrong" are the same observation.
#
# The FORMAT is parsed in two places (`db_checks_passed` and
# `write_receipt`), so it gets accessors rather than two copies of the
# same parameter expansion (CLAUDE.md §9a). They peel from the RIGHT, so
# a check name containing a colon still reads correctly — which is what
# the two-field version did, and is worth not losing.
RAN=()
ran_name() { local e="$1"; printf '%s' "${e%:*:*}"; }
ran_result() { local e="${1%:*}"; printf '%s' "${e##*:}"; }
ran_secs() { printf '%s' "${1##*:}"; }

# ---------------------------------------------------------------------
# The receipt
# ---------------------------------------------------------------------
# A car's `gate` step is free text, so "the gate was green" has always
# been an assertion the protocol takes on trust. On 2026-08-17 a car
# asserted `infra/gate.sh --auto green` while its crate did not compile,
# and the train it boarded went red twice (packet 742d1faa).
#
# This writes down what actually happened, in a form the author did not
# type: the mode, the commit, the host, whether CI markers were set, the
# free space, and every check with its result. It is evidence, not
# enforcement — nothing here can stop someone pasting a fiction into the
# step — but it makes the honest thing the easy thing, and it records
# the one fact that keeps catching us out: WHERE the gate ran. Two of
# today's reds were "passed on my machine, failed on the runner",
# and neither prose field would have shown that.
GATE_RECEIPT="${BOSS_GATE_RECEIPT:-.gate-receipt.json}"

write_receipt() {
    local verdict="$1" mode checks="" first=1 entry name result secs
    if [ "$AUTO" -eq 1 ]; then mode="auto"
    elif [ ${#NAMED[@]} -gt 0 ]; then mode="scoped"
    else mode="full"; fi
    for entry in ${RAN+"${RAN[@]}"}; do
        name="$(ran_name "$entry")"
        result="$(ran_result "$entry")"
        secs="$(ran_secs "$entry")"
        [ "$first" -eq 1 ] || checks="${checks},"
        first=0
        checks="${checks}{\"name\":\"${name}\",\"result\":\"${result}\",\"seconds\":${secs}}"
    done
    # `ci` is the fact that keeps mattering: a gate run where no CI
    # marker is set cannot have exercised anything those markers gate.
    local in_ci=false
    if [ -n "${CI:-}${GITHUB_ACTIONS:-}${FORGEJO_ACTIONS:-}" ]; then in_ci=true; fi
    # The honest-reading guard. Empty when nothing DB-backed changed,
    # or when the DB-backed checks actually ran and passed.
    local unver="" unver_count=0 p
    if ! db_checks_passed; then
        for p in $(db_backed_paths); do
            [ "$unver_count" -eq 0 ] || unver="${unver},"
            unver="${unver}\"${p}\""
            unver_count=$((unver_count + 1))
        done
    fi
    # WHY the scope is what it is, for the half of it a reader cannot
    # infer from the changed files: a migration names no crate, so a
    # scope of twenty-five crates over a one-file diff looks wrong
    # until the receipt says the schema moved and these are its
    # readers. `paths` is the migrations this change carries (empty
    # when none), `readers` the derived set whether or not it was
    # pulled in — so a full-mode receipt still states which crates a
    # migration reaches (backlog 4711828d).
    local schema_json="" schema_n=0
    for p in $(changed_paths | schema_paths); do
        [ "$schema_n" -eq 0 ] || schema_json="${schema_json},"
        schema_json="${schema_json}\"${p}\""
        schema_n=$((schema_n + 1))
    done
    # Only a refusal sets this; it names WHY the gate declined, which is
    # the fact a reader needs to tell "the host was unfit" from "the
    # branch was bad".
    local refusal_json=""
    if [ -n "${GATE_REFUSAL:-}" ]; then
        refusal_json="\"refused_because\": \"${GATE_REFUSAL}\","
    fi
    cat > "${GATE_RECEIPT}" <<RECEIPT
{
  ${refusal_json}
  "verdict": "${verdict}",
  "mode": "${mode}",
  "scope": "${NAMED[*]:-}",
  "head": "$(git rev-parse HEAD 2>/dev/null || echo unknown)",
  "dirty": $( [ -n "$(git status --porcelain 2>/dev/null)" ] && echo true || echo false ),
  "host": "$(hostname 2>/dev/null || echo unknown)",
  "ci": ${in_ci},
  "free_gb": $(gate_avail_gb),
  "unverifiable": [${unver}],
  "schema_change": {"paths": [${schema_json}], "readers": "${GATE_SCHEMA_READERS}"},
  "checks": [${checks}]
}
RECEIPT
    if [ -n "${unver}" ]; then
        printf 'gate: UNVERIFIABLE — this change touches %s, and the database-backed checks did not pass here.\n' "${unver_count} path(s)" >&2
        printf '      A migration or registry row cannot be called green on a machine that cannot apply it.\n' >&2
        printf '      Do not record this receipt as evidence of green; let CI judge it.\n' >&2
    fi
}

# Each check runs even if an earlier one failed — a red gate should
# report every failure it can see, not make the author fix serially.
#
# EVERY CHECK GETS AN EMPTY STDIN, and it is declared here rather than at
# the call sites so the property holds for phases nobody has written yet.
# The roster loop does its own `< /dev/null` (for the separate reason
# written there), but twelve direct `check "…"` calls — the cargo phases,
# the web phases, svelte-check — inherited whatever stdin the GATE got: a
# pipe under the gate-runner, a terminal when a person runs it by hand.
# A check that reads stdin would then read different bytes, or block
# forever, depending on how the gate was invoked rather than on anything
# in the tree (backlog f1369b3b, the small sibling of 9d5797d4). No
# present check wants stdin — cargo, bun and svelte-check all read none —
# and if one ever does, that is the finding: a gate phase that can block
# on input cannot run unattended. `check_stdin_self_test` pins this.
check() {
    local name="$1"; shift
    # The poll. Growth during the run is what wedges the box, so the
    # reading taken before this phase is the one that counts.
    require_headroom "to continue before '${name}'"
    echo "::group::gate: ${name}"
    # TIMED, and the timing starts AFTER the headroom poll: the number
    # has to be what the check cost, not what the gate's own bookkeeping
    # cost around it.
    local t0=$SECONDS
    if "$@" < /dev/null; then
        CHECK_STATUS=0
        echo "::endgroup::"
        RAN+=("${name}:pass:$((SECONDS - t0))")
    else
        # Kept for `check_lint`, which needs the NUMBER: a lint's exit 3
        # is not a failure, and pass/fail cannot carry that.
        CHECK_STATUS=$?
        echo "::endgroup::"
        echo "GATE FAIL: ${name} (exit ${CHECK_STATUS}, after $((SECONDS - t0))s)" >&2
        FAILED+=("${name}")
        RAN+=("${name}:fail:$((SECONDS - t0))")
    fi
}
CHECK_STATUS=0

# A LINT THAT COULD NOT ANSWER IS A REFUSAL, NOT A RED.
#
# `infra/lint/lib/git-answer.sh` gives the lints a third exit: 0 clean,
# 1 a violation — a fact about the BRANCH — and 3, `LINT_CANNOT_ANSWER`,
# the lint never read what it judges — a fact about the MACHINE, which
# no author can fix by editing code. It named this mapping as the half
# still open: `check` knows only pass/fail, so a 3 was a plain red.
#
# MEASURED 2026-09-18 (backlog a26f92c4, gate 35f4ff0c): the system of
# record was rolling under a converge, so
# `the-live-protocols-are-the-authored-protocols` could not read the
# registry, and the gate went red on a car that had changed nothing the
# lint judges. CLAUDE.md §Diagnosis: "an infrastructure refusal is not a
# consist failure" — recorded as one it strikes every car aboard.
#
# THE DISK FLOOR'S SHAPE, exactly: `GATE_REFUSAL` set, `write_receipt
# "refused"`, exit 2. `train_gate::standing` reads that receipt as
# `Standing::Refused` and relaunches; the yard and `red_verdict_detail`
# print `refused_because`; nothing takes a strike. Immediate, like the
# floor, rather than at the end of the roster: a machine that cannot
# answer one lint is not a machine whose other verdicts are worth
# recording as the branch's.
#
# The lint's own stderr is kept to a file so the receipt can carry its
# first refusal line — the WHY, in the lint's words, which is otherwise
# a log the runner reaps (§Diagnosis: a verdict someone must go
# re-derive is not a verdict). It is replayed to the gate's stderr after
# the lint exits, so nothing is lost, only delayed by one lint's runtime.
# Only the roster runs through here; cargo, bun and svelte-check have no
# such vocabulary, and a 3 from them means nothing this maps.
lint_keeping_stderr() { # <file> <cmd...>
    local keep="$1" status
    shift
    "$@" 2>"$keep"
    status=$?
    cat "$keep" >&2
    return "$status"
}

# The lints `--quick` could not run to a verdict, named in its closing
# line (see the warning branch in `check_lint`). Only `--quick` fills it.
CANNOT_READ=()
check_lint() {
    local name="$1" keep why target
    shift
    keep="$(mktemp)" || {
        echo "gate: could not make a file to keep a lint's stderr — refusing rather than running a lint whose words would be lost." >&2
        GATE_REFUSAL="no temp file for lint stderr"
        write_receipt "refused"
        exit 2
    }
    # Through a variable, as `run_roster` invokes its runner: boss-cli's
    # brief.rs derives the gate's PHASE list from lines that begin
    # `check "<name>"`, and a literal `check "$name"` here would put a
    # phase called `$name` in every brief.
    local runner=check
    "$runner" "$name" lint_keeping_stderr "$keep" "$@"
    if [ "$CHECK_STATUS" -ne "$LINT_CANNOT_ANSWER" ]; then
        rm -f "$keep"
        return 0
    fi
    # `check` recorded a failure; the lint said otherwise. Rewrite that
    # one entry so the receipt's checks list agrees with its verdict —
    # a reader counting `fail` entries must not count this one.
    unset "FAILED[$((${#FAILED[@]} - 1))]"
    RAN[${#RAN[@]} - 1]="${name}:refused:$(ran_secs "${RAN[${#RAN[@]} - 1]}")"
    # The first line carrying the marker, else the first line at all,
    # made safe for a JSON string literal (the receipt is written by
    # interpolation): quotes and backslashes dropped, control characters
    # flattened, bounded so a chatty lint cannot bloat the receipt.
    why="$(grep -m1 -F "$LINT_CANNOT_ANSWER_MARKER" "$keep" || grep -m1 '[^[:space:]]' "$keep" || true)"
    why="$(printf '%s' "${why:-(the lint printed nothing on stderr)}" | tr -d '"\\' | tr '[:cntrl:]' ' ' | cut -c1-400)"
    # The lint's own `target: <url> (override with <VAR>)` line, when it
    # prints one — the two live lints read different variables
    # (BOSS_JOBS_URL, BOSS_DISPATCHER_URL), and the lint knows which.
    target="$(grep -m1 -F 'override with' "$keep" | tr -s '[:space:]' ' ' | sed -e 's/^ *//' -e 's/ *$//' || true)"
    rm -f "$keep"
    # `--quick` WARNS instead. It is not a gate and says so — its whole
    # claim is "you will not lose a gate to a lint", and it judges
    # nothing a gate will not re-judge on the receipt that counts. The
    # refusal above, landing here for every mode, made a workstation
    # with no route to the registry (the Mac off the LAN) lose the 80
    # lints that can read a bare tree to the two that cannot (backlog
    # c7bea0e2). So: the lint's own CANNOT ANSWER line as a WARNING,
    # the address it was reading so the operator sees WHICH registry,
    # and the closing line counts these rather than saying `clean`
    # bare — a pre-flight must not claim what it could not check.
    # `--lint` and the gate proper keep refusing: their receipts are
    # read as verdicts, and a refused check is not a warning there.
    if [ "$QUICK" -eq 1 ]; then
        CANNOT_READ+=("$name")
        echo "pre-flight: WARNING — '${name}' could not answer: ${why}" >&2
        echo "pre-flight: WARNING — ${target:-BOSS_JOBS_URL=${BOSS_JOBS_URL:-<unset, the lint used its default>}}; the gate will run this lint against the registry it CAN reach." >&2
        return 0
    fi
    echo "gate: REFUSED — the pre-flight lint '${name}' could not answer (exit ${LINT_CANNOT_ANSWER}), so this run judged nothing about the branch." >&2
    echo "  ${why}" >&2
    echo "  An infrastructure refusal, recorded as one (the GATE FAIL line above is check's" >&2
    echo "  pass/fail bookkeeping; the receipt records this check as refused). Fix what the" >&2
    echo "  lint could not reach and gate again — there is nothing here for the author to edit." >&2
    GATE_REFUSAL="pre-flight lint ${name} could not answer (exit ${LINT_CANNOT_ANSWER}): ${why}"
    write_receipt "refused"
    exit 2
}

# ---------------------------------------------------------------------
# Running a roster
# ---------------------------------------------------------------------
# The loop that runs every lint, and the three mechanisms that keep it
# from eating itself.
#
# Until 2026-09-10 this was `while read -r name path; do check "$name"
# bash "$path"; done <<< "$roster"`, which handed every lint the
# REMAINING ROSTER LINES as its stdin. Any lint that reads stdin — a
# `grep` or `awk` whose file list came out empty and so falls back to
# it, a bare `cat`, a `read` — consumed the rest of the roster. The loop
# then ended normally having run only the checks above it, and the gate
# printed `pre-flight: clean` and exited 0. MEASURED on a draft lint
# with an empty grep file list: a 61-lint roster ran NINE checks and the
# gate called it clean (backlog 9d5797d4).
#
# The irony is the point, and it is the comment the old loop carried: it
# ran `bash <path>` rather than executing the file precisely because "a
# lint that quietly could not run is the under-covering gate this roster
# exists to prevent". It guarded one way a lint silently does not run
# and introduced another.
#
# THREE mechanisms, because they fail differently:
#
#   `3<<<` / `<&3` — the ROSTER's defence. The list the loop reads is
#   not on a descriptor a child is handed by default, so no body this
#   loop ever grows can truncate it. A `< /dev/null` on the one call
#   below would have fixed the one call; the descriptor fixes the loop.
#
#   `< /dev/null` — the CHILD's defence, and worth keeping as well.
#   With only fd 3, a stdin-reading lint inherits whatever stdin the
#   GATE got: a pipe under the runner, a terminal by hand. The same lint
#   would then read different bytes, or block forever, depending on how
#   the gate was invoked. An explicit empty stdin makes it EOF
#   everywhere, which is the only answer a lint can be written against.
#
#   the COUNT — the unknown mechanism's defence. The two above close the
#   causes we know; a truncation is invisible by nature, so the loop
#   also asserts it ran as many checks as the roster holds and refuses
#   BY NAME when it did not, whatever ate them. fd 3 is deliberately
#   left open to the children rather than closed with `3<&-`: a lint
#   reading it directly is absurd but possible, and a loud count refusal
#   on that is worth more than closing the hole and leaving the count
#   with no failure mode anyone can exercise.
#
# `$runner` is `check` in the gate and a recorder in the self-test
# below, so the pin exercises THIS loop rather than a copy of it
# (CLAUDE.md §9a).
#
# `bash <path>` rather than executing it, as the consist check does: a
# checkout may not carry the executable bit, and a lint that quietly
# could not run is the under-covering gate this roster exists to
# prevent.
run_roster() {
    local roster="$1" runner="$2" name path want ran=0 last="(none)"
    want=$(printf '%s\n' "$roster" | grep -c '[^[:space:]]')
    while read -r name path <&3; do
        [ -n "$name" ] || continue
        "$runner" "$name" bash "$path" < /dev/null
        ran=$((ran + 1))
        last="$name"
    done 3<<< "$roster"
    if [ "$ran" -ne "$want" ]; then
        printf 'GATE REFUSED: the roster holds %s checks but %s ran.\n' "$want" "$ran" >&2
        printf '  The last check that ran was `%s`. Something consumed the\n' "$last" >&2
        printf '  descriptor this loop reads, which truncates the roster silently —\n' >&2
        printf '  without this count the gate would report clean having run %s of\n' "$ran" >&2
        printf '  %s checks (backlog 9d5797d4).\n' "$want" >&2
        return 1
    fi
    return 0
}

# The pin on all three mechanisms, and it runs wherever the roster runs.
# That is why it is NOT a case inside `scope_self_test`: that one fires
# only on a `-p` invocation, and the modes that matter most here are the
# bare run CI makes and the `--quick` a builder makes, neither of which
# names a crate.
#
# The fixtures live in a temp directory, NEVER in infra/lint/: a file
# there would be discovered as a real lint by this roster and by the
# conductor's consist check, which is the one way a fixture could ship.
#
# Both cases hand the loop a stdin of their own, so no fixture can block
# on a terminal: case 1 hands it the roster text, which is exactly what
# the defect did.
roster_loop_self_test() {
    local tmp bad=0 fixture seen_want
    tmp="$(mktemp -d)" || { echo "gate.sh: the roster self-test cannot make a temp dir" >&2; exit 2; }
    # shellcheck disable=SC2064
    trap "rm -rf '$tmp'" RETURN
    printf 'cat > /dev/null\n' > "$tmp/eats-stdin.sh"
    printf 'cat <&3 > /dev/null 2>&1 || true\n' > "$tmp/eats-fd3.sh"
    printf 'if IFS= read -r l; then echo "stdin carried: $l" >&2; exit 1; fi\nexit 0\n' \
        > "$tmp/demands-empty-stdin.sh"
    printf 'exit 0\n' > "$tmp/quiet.sh"
    local ST_SEEN=""
    # The same shape as `check`: take a name, shift, run the rest, record
    # the name and how it went. A runner that redirected anything itself
    # would be testing itself.
    _st_runner() {
        local n="$1"; shift
        if "$@"; then ST_SEEN="${ST_SEEN}${n}:pass "; else ST_SEEN="${ST_SEEN}${n}:fail "; fi
    }

    # 1. THE DEFECT, reproduced. Three checks with a stdin-eater in the
    #    middle, and the loop's own stdin set to the roster text — which
    #    is what `done <<< "$roster"` made it. All three must run.
    fixture="first $tmp/quiet.sh
eats-stdin $tmp/eats-stdin.sh
last $tmp/quiet.sh"
    printf '%s\n' "$fixture" > "$tmp/as-stdin"
    seen_want="first:pass eats-stdin:pass last:pass "
    ST_SEEN=""
    if ! run_roster "$fixture" _st_runner < "$tmp/as-stdin"; then
        echo "gate.sh roster self-test FAIL: a stdin-reading check truncated the roster" >&2
        bad=1
    fi
    if [ "$ST_SEEN" != "$seen_want" ]; then
        echo "gate.sh roster self-test FAIL: ran [${ST_SEEN}], wanted [${seen_want}] — a check that reads stdin ate the roster behind it" >&2
        bad=1
    fi

    # 2. THE COUNT IS NOT VACUOUS. A check that drains the descriptor the
    #    loop itself reads truncates the roster by a route the two
    #    redirections do not cover, and the count is the only thing that
    #    can see it. `run_roster` must REFUSE.
    fixture="first $tmp/quiet.sh
eats-fd3 $tmp/eats-fd3.sh
last $tmp/quiet.sh"
    ST_SEEN=""
    if run_roster "$fixture" _st_runner < /dev/null 2>/dev/null; then
        echo "gate.sh roster self-test FAIL: a check that drained the loop's own descriptor left [${ST_SEEN}] and run_roster still returned success — either the count guard is gone, or the loop no longer reads the roster on fd 3 and case 1 is the one to fix" >&2
        bad=1
    fi

    # 3. EVERY CHECK GETS AN EMPTY STDIN, which case 1 cannot see: with
    #    the roster on fd 3, a lint reading stdin no longer breaks the
    #    ROSTER, so the truncation cases stay green while the lint reads
    #    the gate's own stdin — different bytes on a runner than by hand,
    #    or a block forever on a terminal. This roster holds no eater, so
    #    the only thing that can give this check EOF is the `< /dev/null`
    #    on the call itself.
    fixture="first $tmp/quiet.sh
demands-empty-stdin $tmp/demands-empty-stdin.sh
last $tmp/quiet.sh"
    seen_want="first:pass demands-empty-stdin:pass last:pass "
    ST_SEEN=""
    run_roster "$fixture" _st_runner < "$tmp/as-stdin" 2>/dev/null
    if [ "$ST_SEEN" != "$seen_want" ]; then
        echo "gate.sh roster self-test FAIL: ran [${ST_SEEN}], wanted [${seen_want}] — a check was handed the gate's own stdin instead of an empty one, so what it reads depends on how the gate was invoked" >&2
        bad=1
    fi

    if [ "$bad" -ne 0 ]; then
        echo "gate.sh: the roster loop cannot be trusted to run every lint — fix it before trusting a green pre-flight" >&2
        exit 2
    fi
}

# The SAME property, one layer out: the roster loop is not the only
# caller of `check`. The cargo phases, the web phases and svelte-check
# are direct `check "…"` calls outside that loop, and until this car they
# inherited whatever stdin the GATE got — a pipe under the gate-runner, a
# terminal by hand. Same class as the defect above, one step smaller: a
# phase whose behaviour depends on a file descriptor nobody declared, so
# a check that reads stdin reads different bytes on the runner than on a
# workstation, or blocks forever (backlog f1369b3b).
#
# Fixing the twelve call sites one by one would have been a list, not a
# fix; the redirection lives inside `check` so every FUTURE phase
# inherits it too. That makes this the pin on `check`'s own contract, and
# it exercises the REAL `check` rather than a copy of it (CLAUDE.md §9a)
# — which is why it has to put the two ledgers back afterwards.
check_stdin_self_test() {
    local tmp bad=0 failed_before saved_ran=() saved_failed=()
    tmp="$(mktemp -d)" || { echo "gate.sh: the check self-test cannot make a temp dir" >&2; exit 2; }
    # shellcheck disable=SC2064
    trap "rm -rf '$tmp'" RETURN
    printf 'if IFS= read -r l; then echo "stdin carried: $l" >&2; exit 1; fi\nexit 0\n' \
        > "$tmp/demands-empty-stdin.sh"
    printf 'a line the gate itself was handed\n' > "$tmp/as-stdin"
    # A df OF ITS OWN, because `check` polls the disk and gate_sh.rs tests
    # that poll with a fake df that shrinks on every call. A fixture that
    # ate one of those readings would move the refusal that test expects
    # — which is exactly what it did on the first attempt, turning a
    # green receipt-timing test red. The self-test is about stdin; it must
    # leave the poll's own fixture untouched.
    printf '#!/usr/bin/env bash\necho "Filesystem 1024-blocks Used Available Capacity Mounted on"\necho "/dev/check-self-test 1 1 943718400 1%% /"\n' \
        > "$tmp/df"
    chmod +x "$tmp/df"
    saved_ran=(${RAN+"${RAN[@]}"})
    saved_failed=(${FAILED+"${FAILED[@]}"})
    failed_before=${#FAILED[@]}
    # The gate's own stdin is NOT empty here — that is the environment
    # being modelled. Only a redirection inside `check` can give the
    # child EOF.
    BOSS_GATE_DF_CMD="$tmp/df" \
        check "check-stdin-self-test" bash "$tmp/demands-empty-stdin.sh" \
        < "$tmp/as-stdin" > /dev/null
    if [ "${#FAILED[@]}" -ne "$failed_before" ]; then
        echo "gate.sh check self-test FAIL: a check was handed the gate's own stdin instead of an empty one, so every phase outside the roster loop behaves one way under the runner and another by hand (backlog f1369b3b)" >&2
        bad=1
    fi
    # Put the ledgers back: a fixture must not appear in the receipt.
    RAN=(${saved_ran+"${saved_ran[@]}"})
    FAILED=(${saved_failed+"${saved_failed[@]}"})
    if [ "$bad" -ne 0 ]; then
        echo "gate.sh: a gate phase that can block on input cannot run unattended — fix \`check\` before trusting this run" >&2
        exit 2
    fi
}

# THE GATE'S GIT READS SURVIVE A FOREIGN-OWNED CHECKOUT — the pin on the
# `safe.directory` slot this script exports at the top.
#
# Same class as the two self-tests above: a mechanism whose failure is
# INVISIBLE. A gate whose git reads refuse does not stop — two lints
# report `clean` on a tree they never read (`no-session-paths` and
# `one-palette`, both through a `git grep` that ends `|| true`) and two
# more mis-diagnose it as a missing trunk ref. There is no red to notice,
# which is why the property needs a pin rather than a comment.
#
# It is NOT a restatement of the export. The probes below set no
# `safe.directory` of their own; they run git as a CHILD of this script,
# which is the inheritance every lint depends on. Delete the export at
# the top and step 2 fails.
#
# `GIT_TEST_ASSUME_DIFFERENT_OWNER` is git's own "pretend another user
# owns this" knob, and using it is what makes this pin UID-INDEPENDENT:
# it reproduces the uid-65534 refusal while running as root, so the pin
# holds on the runner, on a workstation, and under `setpriv` alike — no
# `chown`, and so no root, required to prove it.
gate_git_reads_self_test() {
    # No git, or no repository here at all: there is nothing to pin, and
    # refusing would invent a precondition the gate never had (it reads
    # HEAD as `|| echo unknown` for exactly this case). Deliberately a
    # WORKING-TREE test — `git rev-parse --git-dir` would be answered by
    # the ownership check itself, so a refused tree would skip the pin
    # that exists to catch the refusal.
    command -v git >/dev/null 2>&1 || return 0
    [ -e .git ] || return 0

    # `ls-files` rather than `rev-parse HEAD`: it is ownership-checked,
    # it is what two of the five affected lints actually call, and it
    # answers on a repository with no commit yet — so a failure here is
    # the ownership refusal and not an empty history.
    #
    # 1. THE SIMULATION IS REAL. With every config channel that could
    #    carry a `safe.directory` silenced, the knob MUST produce the
    #    refusal. If it does not, this git cannot simulate a foreign
    #    owner and step 2 would be vacuously green — say so rather than
    #    bank a proof that did not happen (CLAUDE.md §Diagnosis: a check
    #    nobody can read is a check that is not running).
    if env GIT_CONFIG_COUNT=0 GIT_CONFIG_GLOBAL=/dev/null \
           GIT_CONFIG_SYSTEM=/dev/null GIT_TEST_ASSUME_DIFFERENT_OWNER=1 \
           git ls-files >/dev/null 2>&1; then
        echo "gate.sh: $(git --version) did not refuse a checkout it was told another user owns \
(GIT_TEST_ASSUME_DIFFERENT_OWNER had no effect) — the foreign-owner pin is UNPROVEN on this host, not green" >&2
        return 0
    fi

    # 2. THE DOOR OPENS ANYWAY, through the environment this script
    #    exports and a child inherits.
    if ! GIT_TEST_ASSUME_DIFFERENT_OWNER=1 git ls-files >/dev/null 2>&1; then
        echo "gate.sh git self-test FAIL: a child of the gate cannot read git in $GATE_REPO_ROOT when another \
user owns it. On a root-owned checkout the gate's own uid (65534 since #310) hits this for real, and it does not \
surface as one failure: no-secrets fails, migrations-append-only and steptype-bundle-ratchet fail claiming \
\"no trunk ref found\", and no-session-paths and one-palette report \`clean\` on a tree they never read. The \
scoped safe.directory slot exported at the top of this script is what prevents it (packet 8674c440)" >&2
        exit 2
    fi
}

# Runnable on its own, because a pin whose only output is silence is a
# pin nobody can check is still a pin. `roster_loop_self_test` exits 2
# with a named failure, so reaching the line below means it held.
if [ "$SELFTEST" -eq 1 ]; then
    roster_loop_self_test
    check_stdin_self_test
    gate_git_reads_self_test
    echo "gate.sh: roster self-test ok — a check that reads stdin cannot truncate the roster, \
every check is handed an empty stdin (inside the roster loop and out), a truncation by any \
other route is refused by count, and a child's git reads survive a foreign-owned checkout"
    exit 0
fi

run_preflight() {
    roster_loop_self_test
    check_stdin_self_test
    gate_git_reads_self_test
    check "fmt" cargo fmt -- --check
    local roster
    if ! roster=$(preflight_roster); then
        echo "GATE FAIL: preflight-roster" >&2
        FAILED+=("preflight-roster")
        RAN+=("preflight-roster:fail:0")
        return
    fi
    # Say what is NOT asked before asking the rest, the way the first
    # lint says what the workspace cannot cover: a reader of a clean
    # pre-flight should not have to open the directory to learn which
    # lints declared themselves out of it.
    echo "pre-flight: not run here, by their own headers: $(consist_exclusions | cut -f1 | sed 's|.*/||; s/\.sh$//' | tr '\n' ' ')"
    # A truncated roster is a FAILED check, not a quiet shortfall: the
    # receipt has to carry the fact that the gate did not ask everything
    # it claims to ask. `check_lint`, not `check`: a lint's exit 3 is a
    # refusal, and only the roster speaks that vocabulary.
    if ! run_roster "$roster" check_lint; then
        FAILED+=("preflight-roster-complete")
        RAN+=("preflight-roster-complete:fail:0")
    fi
}

# THE PRE-FLIGHT CERTIFIES ONLY A TREE ITS LINTS CAN READ.
#
# Every git-based lint — `pattern_scan`, `git_answer … ls-files`, `git
# grep` — reads the INDEX. An untracked file is not in it, so a dirty
# tree with a NEW file is clean here and red at the gate, which checks
# out the pushed commit where the file IS tracked. MEASURED 2026-09-18
# (backlog a5efc919): the H12 builder's `--quick` said clean, and gate
# a3b26113 went red on `the-estate-address-lives-once` over a manifest
# the pre-flight had never been able to see.
#
# ONE refusal here, not `--others` taught to every reader: the readers
# are many (lib/pattern-scan.sh, lib/git-answer.sh, no-secrets's own
# `ls-files -z`, and the lints nobody has written yet), the refusal is
# one place, and a builder has to `git add` the file before committing
# anyway — this only moves that step in front of the read that depends
# on it. The roots the lints scan are the WHOLE tree (`no-secrets` and
# `the-estate-address-lives-once` read every tracked path), so the
# question is exactly `git ls-files --others --exclude-standard`: a
# tracked-but-modified file is read from the working copy and is fine;
# an ignored file never reaches the gate workspace either and is fine.
#
# A REFUSAL, not a failed check (exit 2, the disk floor's shape): the
# lints did not run, so there is no verdict on the branch to record.
# ONLY the pre-flight modes ask it: the runner's workspace is a fresh
# checkout of the pushed sha, and `--auto` is exercised on scratch trees
# that carry untracked files by design (gate_sh.rs `auto_scope_of`).
# Pinned by boss-testing's
# a_quick_preflight_refuses_an_untracked_file.rs, which runs THIS
# script against synthetic trees.
refuse_untracked_files() {
    local untracked status
    untracked=$(git ls-files --others --exclude-standard 2>&1)
    status=$?
    if [ "$status" -ne 0 ]; then
        echo "gate: cannot tell which files are untracked (git ls-files exited ${status}: ${untracked}) — refusing rather than certifying a tree the lints may not have read." >&2
        GATE_REFUSAL="git ls-files --others exited ${status}"
        write_receipt "refused"
        exit 2
    fi
    [ -n "$untracked" ] || return 0
    echo "gate: the tree holds untracked files the lints cannot see. Refusing to certify it." >&2
    printf '%s\n' "$untracked" | sed 's/^/  /' >&2
    echo "  Every git-based lint reads the index, so an untracked file is clean here and" >&2
    echo "  red at the gate, which checks out the pushed commit (backlog a5efc919)." >&2
    echo "  remediation: git add <file>   # the gate will read it once it is tracked" >&2
    echo "            or add it to .gitignore, if it is scratch that must never ship" >&2
    GATE_REFUSAL="$(printf '%s\n' "$untracked" | grep -c '[^[:space:]]') untracked file(s) the lints cannot read"
    write_receipt "refused"
    exit 2
}

# THE PRE-FLIGHT VOUCHES FOR THIS TREE; THE GATE READS THE PUSHED COMMIT.
#
# The other half of the refusal above. `refuse_untracked_files` covers
# the file git cannot see at all, and every word of its reasoning — "an
# untracked file is clean here and red at the gate, which checks out the
# pushed commit" — applies to a file git CAN see and reads differently
# from HEAD. The lints read the WORKING COPY of a tracked path, so they
# are right about this tree and say nothing about the commit that gets
# gated.
#
# MEASURED 2026-09-21 (backlog 87454e54, gate-run 9e02228b), and it cost
# a gate. Resolving a rerail, a builder ran `cherry-pick --continue`
# (which commits), then `cargo fmt --all`, then `--lint` (clean), then
# `git push`: the fmt had moved one mis-indented closing brace and was
# never committed, so the push carried the PRE-fmt commit. The gate went
# red on `fmt`, and on `no-employee-id-literal` — the brace was where
# that lint reads the `cfg(test)` region to end, so a literal inside a
# test module read as production code. One whitespace character, two
# failed checks, neither of them about the change.
#
# A WARNING, NOT A REFUSAL, unlike the untracked case: running the
# pre-flight mid-edit to see whether the lints are happy is the normal
# builder loop, and refusing that would break it. What must not happen
# silently is pre-flight -> push with edits in between. So the note
# rides beside the verdict — named files, and what the gate will read
# instead — and the builder decides. Pinned by boss-testing's
# a_preflight_says_which_files_the_gate_will_not_read.rs, which runs
# THIS script against synthetic trees.
note_uncommitted_modifications() {
    local modified status count
    # `git diff HEAD` rather than a bare `git diff`: a staged edit is
    # still not in the commit the push carries, and `git add` without a
    # commit is the other half of the same slip.
    modified=$(git diff --name-status HEAD 2>&1)
    status=$?
    if [ "$status" -ne 0 ]; then
        # Not fatal — this is a warning, and a pre-flight that cannot
        # compare to HEAD still ran its lints. Said out loud rather than
        # swallowed, because silence here is the defect above.
        echo "pre-flight: cannot tell how this tree differs from HEAD (git diff exited ${status}: ${modified})" >&2
        echo "pre-flight: the gate reads the PUSHED commit, so commit before you push." >&2
        return 0
    fi
    [ -n "$modified" ] || return 0
    count=$(printf '%s\n' "$modified" | grep -c '[^[:space:]]')
    echo "pre-flight: ${count} tracked file(s) here differ from HEAD, and the gate reads the PUSHED commit:"
    printf '%s\n' "$modified" | sed 's/^/  /'
    echo "  The lints above read the working copy, so this verdict is about THIS tree."
    echo "  remediation: commit before you push — an edit made after the pre-flight is"
    echo "            judged by the gate and vouched for by nobody (backlog 87454e54)."
}

# `--quick` stops here. It is a PRE-FLIGHT, not a gate, and says so:
# nothing compiles, so it cannot see a clippy error, a failing test or a
# broken build. Its whole claim is "you will not lose a gate to a lint
# or a formatting slip", which is the class of red it is answering.
if [ "$QUICK" -eq 1 ]; then
    refuse_untracked_files
    run_preflight
    echo ""
    note_uncommitted_modifications
    if [ "${#FAILED[@]}" -gt 0 ]; then
        echo "pre-flight: ${#FAILED[@]} check(s) failed: ${FAILED[*]}" >&2
        echo "pre-flight: fix these before spending a gate on them." >&2
        exit 1
    fi
    if [ "${#CANNOT_READ[@]}" -gt 0 ]; then
        # Never a bare `clean` here: the count is the claim's honest
        # edge, and the WARNING lines above name each lint and why.
        echo "pre-flight: clean except ${#CANNOT_READ[@]} lint(s) that could not read the registry: ${CANNOT_READ[*]}"
        echo "pre-flight: no build ran, so this is NOT a gate; the gate runs those lints itself."
        echo "pre-flight: clippy, build and the test suites are still unproven — infra/gate.sh --lint proves clippy in seconds."
        exit 0
    fi
    echo "pre-flight: clean — no build ran, so this is NOT a gate."
    echo "pre-flight: clippy, build and the test suites are still unproven — infra/gate.sh --lint proves clippy in seconds."
    exit 0
fi

# `--lint` is `--quick` plus the one compiled check that pays for
# itself. It exists because of a measured waste: on 2026-08-28 a car
# went red on `clippy` alone, costing a gate and a re-gate — about 22
# minutes of cluster time — for two unused imports. The pre-flight had
# passed and was right to: it says in its own output that clippy is
# still unproven.
#
# David, 2026-08-28: "Inherent slowness is fine. That just incentivizes
# us to squeeze out errors around those steps to ensure they are never
# wasted." A gate takes ~11 minutes and train CI ~15; a scoped clippy
# takes seconds on a warm tree. Trading the second for the first is the
# whole argument.
#
# SCOPED, NOT WORKSPACE. It clippies exactly the crates the tree
# changed, derived by the same `crates_from_paths` the `-p` refusal
# uses — so there is one definition of "which crates did this touch",
# not two. A change that maps to no crate (docs, infra, apps) skips
# clippy and says so, because there is nothing to compile.
#
# STILL NOT A GATE. The build and the test suites remain unproven, and
# a DB-backed test cannot run here at all. This narrows the red-gate
# classes by one; it does not replace the gate.
if [ "$LINT" -eq 1 ]; then
    refuse_untracked_files
    run_preflight
    LINT_CRATES=$(crates_from_paths)
    if [ -n "$LINT_CRATES" ]; then
        LINT_SCOPE=()
        for c in $LINT_CRATES; do LINT_SCOPE+=(-p "$c"); done
        echo ""
        echo "pre-flight: clippy on ${LINT_CRATES//$'\n'/ }"
        # The SAME invocation the gate runs in car mode — a second
        # spelling here would be a check that disagrees with the check
        # it is meant to predict.
        check "clippy" cargo clippy "${LINT_SCOPE[@]}" --all-features --tests -- -D warnings
    else
        echo ""
        echo "pre-flight: no crate implied by the tree — skipping clippy (nothing to compile)"
    fi
    echo ""
    note_uncommitted_modifications
    if [ "${#FAILED[@]}" -gt 0 ]; then
        echo "pre-flight: ${#FAILED[@]} check(s) failed: ${FAILED[*]}" >&2
        echo "pre-flight: fix these before spending a gate on them." >&2
        exit 1
    fi
    echo "pre-flight: clean, and clippy saw the crates this tree changed."
    echo "pre-flight: the build and the test suites are still unproven — this is NOT a gate."
    exit 0
fi

# The shared fixture, checked in BOTH modes and named before anything
# else. Measured across the forge's CI history on 2026-08-15 (106 runs,
# 36 trains): 79% of train reds surfaced only in `test`, the slowest
# stage, and the expensive ones were not a crate's logic failing. They
# were the shared fixture failing — the schema directory, or the TestDb
# harness itself — which reds every DB-backed crate at once.
#
# Those are exactly the breaks car mode could not see. `-p <crate>`
# answers "did I break my crate"; a fixture break belongs to everyone,
# so scoping the gate to the changed crate scoped the check away and the
# first thing to notice was a train. Running it unscoped here puts a
# fixture break in front of the agent who caused it.
if [ "$AUTO_SKIP_FIXTURE" -eq 1 ]; then
    echo "gate: skipping fixture — no crate and no schema change to break it"
else
    check "fixture" cargo test -p boss-testing --features postgres --test fixture_smoke
fi

if [ "$AUTO_LINTS_ONLY" -eq 1 ]; then
    echo "gate: skipping clippy / build / test — nothing changed implies a crate"
elif [ "${#SCOPE[@]}" -eq 0 ]; then
    # Full gate — the CI shape.
    check "clippy"  cargo clippy --workspace --all-features --tests -- -D warnings
    # Default-feature build: a dangling `#[cfg(feature = ...)]` rebinds
    # onto the next item and is invisible to every --all-features step
    # (see #180). One cheap build closes the class.
    check "build (default features)" cargo build --workspace
    # The tenant bundles the image ships, judged by the `boss` the
    # build above just produced (backlog fd8ee021). Out of the
    # pre-flight for the same reason as no-snapshot-arrays below —
    # the roster runs before any binary exists — and run here so the
    # exclusion is not a lint nobody runs. Before `test`: a bundle a
    # gateway would refuse to boot from is the cheaper, louder
    # verdict, and the train's assembled-tree gate runs this branch.
    check "an-image-sourced-tenant-passes-its-check" infra/lint/an-image-sourced-tenant-passes-its-check.sh
    check "test"    cargo test --all-features
    # Kept out of the pre-flight because it reads the built
    # boss-ports-list; the build above just produced it. This line was
    # missing from 2026-08-31 to 2026-09-12: the exclusion said "CI
    # builds, then runs it", CI did not, and the lint sat red on a page
    # #161 had deleted, read by nobody (spa-lists-are-generated.toml).
    check "no-snapshot-arrays" infra/lint/no-snapshot-arrays.sh
else
    check "clippy"  cargo clippy "${SCOPE[@]}" --all-features --tests -- -D warnings
    check "build (default features)" cargo build "${SCOPE[@]}"
    check "test"    cargo test "${SCOPE[@]}" --all-features
    # A scoped car pays for the binary only when it could have moved
    # the answer: the registry crate, or a generated copy of it.
    if changed_paths | grep -qE '^(crates/core/boss-ports/|apps/(web|simulator)/src/_generated/ports\.ts$)'; then
        check "build boss-ports-list" cargo build -p boss-ports
        check "no-snapshot-arrays" infra/lint/no-snapshot-arrays.sh
    fi
    # Likewise the tenant bundles (backlog fd8ee021): a car that edits
    # a shipped bundle, the verb that judges it, or the lint itself
    # pays for the CLI. Built by name — a bundle car's derived scope is
    # its engine crate (`examples/<t>/seeds/*` -> boss-<t>-engine),
    # not boss-cli — then judged with it.
    if changed_paths | grep -qE '^(examples/[^/]+/|crates/orchestrators/boss-cli/|infra/lint/an-image-sourced-tenant-passes-its-check\.sh$)'; then
        check "build boss-cli" cargo build -p boss-cli
        check "an-image-sourced-tenant-passes-its-check" infra/lint/an-image-sourced-tenant-passes-its-check.sh
    fi
fi

# THE WEB SUITE. CI's web job runs typecheck + unit + build + the
# mocked Playwright suite; before this gate ran svelte-check alone, so
# a car could pass here and red the train on a check it never saw
# (§9a: this block and ci.yml's web job are two copies of one
# definition, kept in sync).
#
# FULL MODE RUNS IT UNCONDITIONALLY, matching CI — the full gate is the
# authoritative one and must not be narrower than the train it feeds.
# It used to be gated to `--auto` as well (`AUTO -eq 1 && web_touched`),
# which is exactly why `boss gate` runs full mode, skipped the suite,
# and let mocked-spec reds through: trains #160 (route crawl missed the
# estate page) and #161 (~35 specs after the IT consolidation moved
# surfaces) both died that way — ade5d82b. --auto keeps the old
# scoping, because a docs or Rust car iterating locally should not pay
# the suite unless it touched the web; CI and the full gate are the
# unconditional backstops. The browser is baked into boss-ci at
# /opt/ms-playwright (the keystone), so this needs no run-time download
# — the gate-runner points PLAYWRIGHT_BROWSERS_PATH there.
web_touched() {
    if changed_paths | grep -qE '^(apps/web|apps/simulator|libs/web-kit)/'; then echo yes; else echo no; fi
}
if [ "$AUTO" -eq 0 ] || [ "$(web_touched)" = "yes" ]; then
    # A clean install FIRST, with puppeteer's postinstall skipped. bun
    # aborts the WHOLE install on a failed postinstall, and puppeteer's
    # browser download is the flaky one — the exact reason ci.yml's web
    # job and svelte-check.sh both set PUPPETEER_SKIP_DOWNLOAD. The
    # gate-runner's run.sh does a best-effort warm-up install (|| true),
    # so the suite cannot trust node_modules to be complete and does its
    # own. Cached after the warm-up, so this is seconds, not minutes.
    check "web install" bash -c 'cd apps/web && PUPPETEER_SKIP_DOWNLOAD=1 bun install --frozen-lockfile'
    # web-kit FIRST: its 7 test files existed for weeks and ran in no
    # job at all - not here, not in ci.yml. One of them could not even
    # load, because it imported a module whose top-level `$state` made
    # it unloadable outside the Svelte compiler; the rest silently
    # protected nothing. A test nothing runs is not a test.
    check "web-kit unit" bash -c 'cd libs/web-kit && bun run test:unit'
    check "web-suite (unit+build+mocked)" bash -c 'cd apps/web && bun run test:unit && bun run build && bun run test:mocked'
fi

run_preflight


# The frontend type gate. Last, because it is the only check that
# installs anything, and a Rust-only car should learn about its Rust
# failures before waiting on a package install.
check "svelte-check"             infra/lint/svelte-check.sh

if [ "${#FAILED[@]}" -gt 0 ]; then
    write_receipt "failed"
    echo "" >&2
    echo "gate: ${#FAILED[@]} check(s) failed: ${FAILED[*]}" >&2
    echo "gate: receipt written to ${GATE_RECEIPT}" >&2
    exit 1
fi
write_receipt "green"
echo "gate: all checks green"
echo "gate: receipt written to ${GATE_RECEIPT}"
