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
#   infra/gate.sh --auto          # car mode, scope DERIVED from the
#                                 # tree. Skips cargo entirely when
#                                 # nothing changed implies a crate —
#                                 # 74 of 164 live branches are in that
#                                 # class. Never used by CI.
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

# Incremental compilation helps REPEATED local builds; a gate build is
# cold and one-shot, so incremental only writes an incremental/ dir that
# is pure disk cost here — part of the ~80GB target/ that exhausted the
# forge CI disk and blocked trains (2026-09-04;
# docs/design/the-build-plane-manages-itself.md). Off for the gate and CI
# (this script IS the CI rust job); a human's own `cargo build` outside
# this script is untouched and keeps incremental.
export CARGO_INCREMENTAL=0

SCOPE=()
NAMED=()
AUTO=0
QUICK=0
LINT=0
while [ $# -gt 0 ]; do
    case "$1" in
        -p) shift; SCOPE+=(-p "${1:?-p needs a crate name}"); NAMED+=("$1"); shift ;;
        --auto) AUTO=1; shift ;;
        --quick) QUICK=1; shift ;;
        --lint) LINT=1; shift ;;
        *) echo "gate.sh: unknown arg: $1 (accepts -p <crate>, --auto, --quick and --lint)" >&2; exit 2 ;;
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


# DISK FLOOR, before anything compiles.
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
#   docs/design/  -> boss-docs. Those markdown files are INPUT to the
#       corpus gate (boss-docs/tests/docs_corpus_presents.rs parses
#       every one), so a docs-only change really can fail a crate's
#       tests. Path-to-crate is not the same question as which SOURCE
#       files a crate compiles.
#
#   The same reasoning, four more times — each one a file some crate's
#   test READS, so changing it can redden that crate without touching
#   a line of its source:
#     infra/gate.sh, infra/lint/*, .forgejo/workflows/ci.yml
#         -> boss-testing, which owns gate_sh.rs. That test pins that
#            ci.yml invokes this script, that this script runs every
#            check, and that every executable in infra/lint/ appears
#            here. Omitting these would let `--auto` skip the only
#            test guarding the file being edited — which this very car
#            would have done to itself.
#     infra/dispatcher/rules.toml -> boss-dispatcher, which owns
#            dispatcher_rules_seed.rs. It compares the seeded registry
#            against that file in BOTH directions, and skipping the
#            toml half is what reddened the 13-car train
#            20260815-0621.
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
    changed_paths | grep -E '^infra/postgres/schema/|^infra/dispatcher/rules\.toml$|/seeds/[^/]*\.toml$' || true
}

# Did the checks that need a live database actually pass? `fixture`
# standing up IS the database being reachable, so a fixture failure
# means every DB-backed result below it is absent rather than green.
db_checks_passed() {
    local entry name result seen=0
    for entry in ${RAN+"${RAN[@]}"}; do
        name="${entry%:*}"; result="${entry##*:}"
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
path_map() {
    sed -n -e 's|^crates/[^/]*/\([^/]*\)/.*|\1|p' \
           -e 's|^docs/design/.*|boss-docs|p' \
           -e 's|^infra/gate\.sh$|boss-testing|p' \
           -e 's|^infra/lint/.*|boss-testing|p' \
           -e 's|^\.forgejo/workflows/ci\.yml$|boss-testing|p' \
           -e 's|^infra/dispatcher/rules\.toml$|boss-dispatcher|p' \
           -e 's|^infra/platform/workflows\.toml$|boss-jobs|p' \
           | sort -u | tr '\n' ' '
}

scope_self_test() {
    local fails=0 label want got
    _case() {
        label="$1"; want="$2"; shift 2
        got=$(printf '%s\n' "$@" | path_map); got="${got% }"
        if [ "$got" != "$want" ]; then
            echo "gate.sh scope self-test FAIL: ${label} -> [${got}], wanted [${want}]" >&2
            fails=1
        fi
    }
    # The commit this rule was written for: a docs title over a
    # boss-jobs change (a6ffcb7c).
    _case "the commit that earned this rule" "boss-docs boss-jobs" \
        "docs/design/queue-visibility.md" \
        "crates/core/boss-jobs/src/registry.rs" \
        "crates/core/boss-jobs/tests/platform_bundle.rs"
    # Design docs are INPUT to boss-docs' corpus gate, so a docs-only
    # car really does have a crate to compile.
    _case "a genuinely docs-only car" "boss-docs" "docs/design/payload-encryption.md"
    # The platform bundle is DATA, but boss-jobs compiles a test that
    # parses and lints it (`the_platform_bundle_matches_the_specs_it
    # _replaced`). Without this line a protocol-only car scoped to
    # "lints + fmt only" and never ran the one test that can reject it
    # — which is how correct-the-record's second defect nearly shipped:
    # the bundle lint caught a free-text fork with no fallback, and the
    # gate would not have run that lint at all.
    _case "a protocol-only car still has a crate" "boss-jobs" \
        "infra/platform/workflows.toml"
    _case "two files, one crate" "boss-cli" \
        "crates/orchestrators/boss-cli/src/train.rs" \
        "crates/orchestrators/boss-cli/src/docs.rs"
    # The tier segment must not be mistaken for the crate name.
    _case "tier is not the crate" "boss-people" "crates/modules/boss-people/src/http.rs"
    _case "a crate's root files count" "boss-jobs" "crates/core/boss-jobs/Cargo.toml"
    # Everything outside those two trees implies nothing to scope —
    # the lints already run repo-wide.
    # gate.sh and ci.yml are READ by boss-testing's gate_sh.rs, so a
    # change to either must compile and run that crate.
    _case "the gate's own files imply boss-testing" "boss-testing" \
        "infra/gate.sh" ".forgejo/workflows/ci.yml" "infra/lint/no-secrets.sh"
    _case "the dispatcher rule file implies boss-dispatcher" "boss-dispatcher" \
        "infra/dispatcher/rules.toml"
    _case "other infra implies no crate" "" \
        "infra/forge/locomotive.sh" "infra/deploy-services.sh"
    _case "docs outside design/ imply no crate" "" "docs/invariants/x.toml" "README.md"
    _case "the web app implies no crate" "" "apps/web/src/me/MePage.svelte"
    # Schema files imply no CRATE, which is why --auto asks
    # `schema_touched` separately rather than reading it off this map.
    # Get this wrong in the other direction — map schema to some crate
    # — and every migration would compile a crate for no reason.
    _case "a migration implies no crate" "" "infra/postgres/schema/141-x.sql"
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
# THE FIXTURE IS THE SUBTLE PART. `infra/postgres/schema/**` maps to no
# crate, but the shared fixture LOADS the schema — so a schema-only
# change has no crate to compile and can still break every DB-backed
# test in the workspace. Skipping cargo entirely there would scope away
# the exact break the fixture check exists to catch, which is what the
# comment above `check "fixture"` warns about. So the derivation
# answers two questions: which crates, and whether the fixture is
# implicated.
schema_touched() {
    if changed_paths | grep -qE '^infra/postgres/schema/'; then echo yes; else echo no; fi
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
    elif [ "$(schema_touched)" = "yes" ]; then
        # No crate, but the schema moved: the fixture is the one check
        # that can see that, so it runs and nothing else cargo-shaped.
        AUTO_LINTS_ONLY=1
        echo "gate: --auto — no crate changed, but infra/postgres/schema/ did; fixture + lints only"
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
# Every check and how it went, so the receipt can say what RAN rather
# than only what broke.
RAN=()

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
    local verdict="$1" mode checks="" first=1 entry name result
    if [ "$AUTO" -eq 1 ]; then mode="auto"
    elif [ ${#NAMED[@]} -gt 0 ]; then mode="scoped"
    else mode="full"; fi
    for entry in ${RAN+"${RAN[@]}"}; do
        name="${entry%:*}"; result="${entry##*:}"
        [ "$first" -eq 1 ] || checks="${checks},"
        first=0
        checks="${checks}{\"name\":\"${name}\",\"result\":\"${result}\"}"
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
check() {
    local name="$1"; shift
    # The poll. Growth during the run is what wedges the box, so the
    # reading taken before this phase is the one that counts.
    require_headroom "to continue before '${name}'"
    echo "::group::gate: ${name}"
    if "$@"; then
        echo "::endgroup::"
        RAN+=("${name}:pass")
    else
        echo "::endgroup::"
        echo "GATE FAIL: ${name}" >&2
        FAILED+=("${name}")
        RAN+=("${name}:fail")
    fi
}

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
# `svelte-check` is NOT here — it installs packages, which is minutes,
# not seconds. `no-snapshot-arrays` is not here either; it needs a
# build, and the delivery policy already excludes it for that reason.
PREFLIGHT_LINTS=(
    # FIRST on purpose: it says what this workspace cannot cover, which
    # frames every result below it. A green pre-flight on a machine with
    # no Postgres is 118 database-backed test targets unrun, and saying
    # so before the rest is the difference between confidence and a
    # gate failure eleven minutes later (design 775f0b35 Q3).
    "workspace-declares-what-it-runs|infra/lint/workspace-declares-what-it-runs.sh"
    "the-image-carries-what-the-launcher-sources|infra/lint/the-image-carries-what-the-launcher-sources.sh"
    "seed-bypass-smell|infra/lint/seed-bypass-smell.sh"
    "no-todo-citation|infra/lint/no-todo-citation.sh"
    "no-step-kind-match|infra/lint/no-step-kind-match.sh"
    "api-path-bypass-smell|infra/lint/api-path-bypass-smell.sh"
    "dispatcher-actor-stamp|infra/lint/dispatcher-actor-stamp.sh"
    "sim-boundary-audit|infra/lint/sim-boundary-audit.sh"
    "tier-import-audit|infra/lint/tier-import-audit.sh"
    "layer-order-audit|infra/lint/layer-order-audit.sh"
    "no-wallclock|infra/lint/no-wallclock.sh"
    "one-stamp-per-transaction|infra/lint/one-stamp-per-transaction.sh"
    "emitted-kinds-are-declared|infra/lint/emitted-kinds-are-declared.sh"
    "outbox-migration-ratchet|infra/lint/outbox-migration-ratchet.sh"
    "idempotence-ratchet|infra/lint/idempotence-ratchet.sh"
    "dispatcher-rules-ratchet|infra/lint/dispatcher-rules-ratchet.sh"
    "steptype-bundle-ratchet|infra/lint/steptype-bundle-ratchet.sh"
    "schema-converge|infra/lint/schema-converge.sh"
    "a-failed-prepare-degrades-the-pod|infra/lint/a-failed-prepare-degrades-the-pod.sh"
    "migrations-append-only|infra/lint/migrations-append-only.sh"
    "migration-numbers-unique|infra/lint/migration-numbers-unique.sh"
    "no-secrets|infra/lint/no-secrets.sh"
    "no-session-paths|infra/lint/no-session-paths.sh"
    "session-key-persists|infra/lint/session-key-persists.sh"
    "invariant-register|infra/lint/invariant-register.sh"
    "crate-counts-fresh|infra/lint/crate-counts-fresh.sh"
    "registry-bump-order|infra/lint/registry-bump-retires-first.sh"
    "ci-tools-declared|infra/lint/ci-tools-declared.sh"
    "timers-leave-a-packet|infra/lint/timers-leave-a-packet.sh"
    "step-plugin-bundle|infra/lint/step-plugin-bundle-exists.sh"
    "step-plugins-own-their-keys|infra/lint/step-plugins-own-their-keys.sh"
    "one-palette|infra/lint/one-palette.sh"
    "one-date-format|infra/lint/one-date-format.sh"
    "kind-bundle-does-not-tighten|infra/lint/a-kind-bundle-does-not-tighten.sh"
    "new-style-has-a-caller|infra/lint/a-new-style-has-a-caller.sh"
    "every-spa-api-path-is-routed|infra/lint/every-spa-api-path-is-routed.sh"
    "forge-install-covers-the-ops-runner|infra/lint/forge-install-covers-the-ops-runner.sh"
    # The one roster entry allowed a network fetch: report-only (always
    # exits 0) and soft-skips when the tool or the advisory DB is
    # absent, so it cannot red a gate — it can only add a report line.
    "cargo-advisories|infra/lint/cargo-advisories.sh"
)

run_preflight() {
    check "fmt" cargo fmt -- --check
    local entry
    for entry in "${PREFLIGHT_LINTS[@]}"; do
        check "${entry%%|*}" "${entry#*|}"
    done
}

# `--quick` stops here. It is a PRE-FLIGHT, not a gate, and says so:
# nothing compiles, so it cannot see a clippy error, a failing test or a
# broken build. Its whole claim is "you will not lose a gate to a lint
# or a formatting slip", which is the class of red it is answering.
if [ "$QUICK" -eq 1 ]; then
    run_preflight
    echo ""
    if [ "${#FAILED[@]}" -gt 0 ]; then
        echo "pre-flight: ${#FAILED[@]} check(s) failed: ${FAILED[*]}" >&2
        echo "pre-flight: fix these before spending a gate on them." >&2
        exit 1
    fi
    echo "pre-flight: clean — no build ran, so this is NOT a gate."
    echo "pre-flight: clippy, build and the test suites are still unproven."
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
    check "test"    cargo test --all-features
else
    check "clippy"  cargo clippy "${SCOPE[@]}" --all-features --tests -- -D warnings
    check "build (default features)" cargo build "${SCOPE[@]}"
    check "test"    cargo test "${SCOPE[@]}" --all-features
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
