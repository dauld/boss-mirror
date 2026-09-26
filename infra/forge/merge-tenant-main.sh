#!/usr/bin/env bash
# merge-tenant-main — land a reviewed merge on an instance's tenant main.
#
#   merge-tenant-main.sh --plan <instance> <merge-branch>
#   merge-tenant-main.sh <instance> <merge-branch> <plan-sha256>
#
# WHY IT EXISTS (backlog 01d3a701, David asked for the pair 2026-09-22).
# Two tenant branches were built, merged, conflict-resolved and verified
# PASS by `boss tenant check`, and the only way to land the merge on
# tenant main — what seed-tenant.sh reads at every services-container
# start — was a human typing `git push`. The pod door refused it,
# correctly, as a merge without review. The merge is mechanical once a
# check decides it; the APPROVAL is the human's. So this is design
# 17835005's shape (David, 2026-09-21: "a rendered plan hash, signed
# with his passkey, single-use, verified before the argv is built"),
# and its second caller after commission-a-disk.
#
# THE MERGE IS AUTHORED ON A BRANCH OF THE TENANT REPO, not here: a
# conflict needs a judgement, and a judgement belongs to whoever wrote
# the branch. What this script does is make that judgement REVIEWABLE:
# the plan renders every byte each merge commit holds that the
# automatic merge of its parents does not (`git merge-tree` of the
# parents against the commit's tree). On 2026-09-22 both branches
# appended a section 6 to the tail of seeds/rules.toml and the
# resolution renumbered one to 7 — a reviewer approving the merge is
# approving that, so it is in the plan rather than inferred.
#
# --plan RENDERS AND DOES NOT ACT. It refuses (exit 78) rather than
# render a plan for something that cannot land: a branch that does not
# contain tenant main (landing it would not be a fast-forward of the
# main the reviewer saw), an octopus merge (merge-tree judges two
# parents), and a merged tree `boss tenant check` does not PASS — a
# check that cannot run is exit 1, never a pass. The plan document
# carries no clock, no run id, no URL and no scratch path, so two
# renders of one true state are byte-identical, and its sha256 (on
# stderr, `plan-sha256: <hex>`, because a hash cannot be inside the
# bytes it hashes) is what a passkey signs.
#
# THE WRITE RE-RENDERS AND COMPARES, then pushes the one commit the plan
# names. The approval channel verifies the signature over the hash
# before the argv is built; this script then proves the hash is still
# TRUE — every observed fact (main's sha, the branch's sha, the tree,
# the resolution, the check) is in the bytes, so a main that moved, a
# branch that moved or a check that now fails is a different hash and
# lands nothing. The push itself carries --force-with-lease on the main
# sha the plan names, so a main that moves in the seconds between the
# re-render and the push is refused by the forge rather than
# overwritten; the ancestry check has already made it a fast-forward.
# INERT TODAY: the verb declares requires_approval and ops-runner.sh
# refuses every such verb until the approval channel exists — the gate
# lands before the power, as commission-a-disk did.
#
# THE BOUND IS BY CONSTRUCTION. The repo and the branch it lands on are
# the instance's OWN `tenant_repo` / `tenant_ref` from
# infra/cluster/instances.toml, read through render-instance.sh
# --instances — never a parameter (the rule ea3c8234 set for the export
# verb). The URL and credential are the converge's
# (cluster-deploy-lib.sh tenant_repo_url: this checkout's forgejo
# remote with the repo path replaced), and every git message is
# redacted before it is printed.
#
# Exit 78 is a refusal (the request was wrong or no longer true);
# exit 1 is a step that genuinely failed.
set -euo pipefail

ME=merge-tenant-main
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# The checkout whose `forgejo` remote the tenant URL derives from — this
# one; a test names a fixture's.
REMOTE_OF="${BOSS_TENANT_REMOTE_OF:-$(cd "$HERE/../.." && pwd)}"
CLI="${BOSS_CLI:-boss}"
export GIT_TERMINAL_PROMPT=0

refuse() { echo "$ME: REFUSED — $*" >&2; exit 78; }
fail() { echo "$ME: FAILED — $*" >&2; exit 1; }

# AS THE CHECKOUT'S OWNER, BEFORE ANY GIT (review F2 of car 85b7b55f,
# 2026-09-26). boss-ops-runner runs its verbs as root — its unit names no
# User= — and the tenant URL below is the checkout's `forgejo` remote
# with the repo path replaced. Until design 1c90d183 that remote carried
# the forge token in its userinfo, so root authenticated by accident of
# the URL. The forge-converge deposit (credential-deposit.sh) strips it
# once the owner's credential helper is proved, and the helper lives in
# the OWNER's global git config, so root has no credential for the forge
# at all. As root this re-runs itself as the owner — read off the
# checkout, never hardcoded — and every read and the one push then
# authenticate the way every other consumer of that checkout does.
# Non-login and with an explicit HOME, the shape delete-orphan-object.sh
# uses: the owner's global config is found through HOME, and a login
# profile's output would land in the plan's bytes.
if [ "$(id -u)" = 0 ] && [ -z "${BOSS_MERGE_TENANT_AS_OWNER:-}" ]; then
    owner="${BOSS_TENANT_CHECKOUT_OWNER:-$(stat -c %U "$REMOTE_OF" 2>/dev/null || true)}"
    if [ -z "$owner" ] || [ "$owner" = UNKNOWN ]; then
        fail "cannot resolve the owner of $REMOTE_OF (stat says '${owner:-}') — the forge credential is that account's, and root holds none"
    fi
    if [ "$owner" != root ]; then
        owner_home="$(getent passwd "$owner" | cut -d: -f6 || true)"
        exec runuser -u "$owner" -- env HOME="${owner_home:-/}" PATH="$PATH" \
            BOSS_MERGE_TENANT_AS_OWNER=1 bash "$HERE/$(basename "${BASH_SOURCE[0]}")" "$@"
    fi
fi

# shellcheck source=cluster-deploy-lib.sh
. "$HERE/cluster-deploy-lib.sh"

PLAN=0
if [ "${1-}" = "--plan" ]; then PLAN=1; shift; fi
if [ "$PLAN" -eq 1 ]; then
    [ $# -eq 2 ] || refuse "usage: $ME --plan <instance> <merge-branch>"
else
    [ $# -eq 3 ] || refuse "usage: $ME <instance> <merge-branch> <plan-sha256>"
    [[ "$3" =~ ^[0-9a-f]{64}$ ]] || refuse "the plan hash must be 64 hex characters"
fi
INSTANCE="$1"
BRANCH="$2"
[[ "$INSTANCE" =~ ^[a-z][a-z0-9-]{0,30}$ ]] || refuse "'$INSTANCE' is not an instance name"
[[ "$BRANCH" =~ ^[A-Za-z0-9][A-Za-z0-9._/-]{0,99}$ ]] && [[ "$BRANCH" != *..* ]] \
    || refuse "'$BRANCH' is not a branch name this verb accepts"

# --- the instance's own tenant repo ------------------------------------
rows=$(bash "$HERE/../cluster/render-instance.sh" --instances) \
    || fail "render-instance.sh --instances could not read infra/cluster/instances.toml"
row=$(awk -F'\t' -v n="$INSTANCE" '$1 == n' <<< "$rows")
[ -n "$row" ] || refuse "no instance [$INSTANCE] in infra/cluster/instances.toml"
TENANT_REPO=$(cut -f7 <<< "$row")
REF=$(cut -f8 <<< "$row")
[ -n "$TENANT_REPO" ] && [ -n "$REF" ] \
    || refuse "instance [$INSTANCE] is image-sourced — it has no tenant_repo, so there is no tenant main to land on"
[ "$BRANCH" != "$REF" ] || refuse "the merge branch cannot be $REF itself"
URL=$(tenant_repo_url "$REMOTE_OF" "$TENANT_REPO") \
    || fail "cannot derive the URL for $TENANT_REPO — $REMOTE_OF has no forgejo remote"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
G="$WORK/t.git"
git init -q --bare "$G"
g() { git --git-dir="$G" -c core.quotePath=false "$@"; }

# Resolve both refs first, so a missing branch is a refusal and an
# unreadable repo is a failure — two different answers.
if ! heads=$(git ls-remote "$URL" "refs/heads/$REF" "refs/heads/$BRANCH" 2>&1); then
    fail "$TENANT_REPO is not readable with this host's credential: $(redact_url <<< "$heads")"
fi
grep -q "	refs/heads/$REF\$" <<< "$heads" || refuse "$TENANT_REPO has no branch $REF"
grep -q "	refs/heads/$BRANCH\$" <<< "$heads" || refuse "$TENANT_REPO has no branch $BRANCH"
if ! out=$(g fetch -q "$URL" "+refs/heads/$REF:refs/plan/main" "+refs/heads/$BRANCH:refs/plan/merge" 2>&1); then
    fail "fetch of $TENANT_REPO failed: $(redact_url <<< "$out")"
fi
MAIN=$(g rev-parse refs/plan/main)
MERGE=$(g rev-parse refs/plan/merge)
TREE=$(g rev-parse "refs/plan/merge^{tree}")

[ "$MAIN" != "$MERGE" ] || refuse "$BRANCH is $REF@$MAIN — there is nothing to land"
g merge-base --is-ancestor "$MAIN" "$MERGE" \
    || refuse "$BRANCH@$MERGE does not contain $REF@$MAIN — landing it would not be a fast-forward of the main a reviewer sees. Merge $REF into the branch and resolve there, where the plan can show the resolution"
# merge-tree judges two parents, so an octopus merge's resolution could
# not be rendered — and an unrendered judgement is not reviewable.
octopus=$(g rev-list --min-parents=3 "$MAIN..$MERGE")
[ -z "$octopus" ] || refuse "$BRANCH holds an octopus merge ($(head -n1 <<< "$octopus")) — its resolution cannot be rendered against two parents; merge one branch at a time"

# --- the check on the MERGED tree --------------------------------------
# Run from inside the tree with `.`, so the report's first line names no
# scratch path and the plan's bytes do not vary between renders.
mkdir "$WORK/tree"
g archive "$MERGE" | tar -x -C "$WORK/tree"
command -v "$CLI" >/dev/null 2>&1 || fail "$CLI is not on this host — the merged tree was NOT checked, and no evidence is not a pass"
check_rc=0
check=$(cd "$WORK/tree" && "$CLI" tenant check . 2>&1) || check_rc=$?
case "$check_rc" in
    0) : ;;
    1) printf '%s\n' "$check" >&2
       refuse "boss tenant check does not PASS on the merged tree $TREE — a merge that does not pass is a refusal, not a plan" ;;
    *) printf '%s\n' "$check" >&2
       fail "boss tenant check exited $check_rc without a verdict — the merged tree was NOT checked" ;;
esac

# --- the plan document --------------------------------------------------
# diffstat_of PATHSPEC... — main..merge's diffstat over PATHSPEC, or "none", so
# an empty section reads as measured rather than as missing.
diffstat_of() {
    local s
    s=$(g diff --no-color --stat=200 "$MAIN" "$MERGE" "$@")
    printf '%s\n' "${s:-none}"
}
render() {
    echo "plan: merge-tenant-main"
    echo "instance: $INSTANCE"
    echo "tenant repo: $TENANT_REPO"
    echo "lands on: refs/heads/$REF"
    echo "main now: $MAIN"
    echo "merge branch: $BRANCH"
    echo "lands as: $MERGE"
    echo "tree: $TREE"
    echo
    echo "== commits $REF does not yet hold ($(g rev-list --count "$MAIN..$MERGE")) =="
    g log --reverse --format='%H %s' "$MAIN..$MERGE"
    echo
    echo "== merges, and how each was resolved =="
    local merges c parents p1 p2 auto names
    merges=$(g rev-list --reverse --merges "$MAIN..$MERGE")
    [ -n "$merges" ] || echo "none — every commit is a plain commit"
    for c in $merges; do
        parents=$(g rev-list --no-walk --parents "$c" | cut -d' ' -f2-)
        read -r p1 p2 _ <<< "$parents"
        echo "merge $c $(g log -1 --format=%s "$c")"
        echo "  parent 1: $p1"
        echo "  parent 2: $p2"
        # merge-tree exits 1 on a conflict and still writes the tree it
        # would have produced, markers included — that tree is the
        # baseline the resolution is judged against.
        auto=$(g merge-tree --write-tree --name-only --no-messages "$p1" "$p2" || true)
        names=$(tail -n +2 <<< "$auto" | sed '/^$/d' | sort -u)
        if [ -z "$names" ]; then
            echo "  clean — the automatic merge had no conflict"
        else
            sed 's/^/  conflicted: /' <<< "$names"
        fi
        if g diff --quiet "$(head -n1 <<< "$auto")" "$c^{tree}"; then
            echo "  resolution: identical to the automatic merge"
        else
            echo "  resolution — the merge commit against the automatic merge of its parents:"
            g diff --no-color --no-ext-diff "$(head -n1 <<< "$auto")" "$c^{tree}" | sed 's/^/    /'
        fi
    done
    echo
    echo "== boss tenant check on the merged tree =="
    printf '%s\n' "$check"
    echo
    echo "== what reaches the live registries =="
    echo "seed-tenant.sh publishes tenant.toml and seeds/ from $REF at every services-container start:"
    diffstat_of -- tenant.toml seeds
    echo
    echo "== other files =="
    diffstat_of -- . ':!tenant.toml' ':!seeds'
    echo
    echo "== the write this plan authorises =="
    echo "git push --force-with-lease=refs/heads/$REF:$MAIN $TENANT_REPO $MERGE:refs/heads/$REF"
}

render > "$WORK/plan"
HASH=$(sha256sum "$WORK/plan" | cut -d' ' -f1)

if [ "$PLAN" -eq 1 ]; then cat "$WORK/plan"; echo "plan-sha256: $HASH" >&2; exit 0; fi

# --- the write -----------------------------------------------------------
[ "$HASH" = "$3" ] || {
    cat "$WORK/plan" >&2
    refuse "the plan now hashes to $HASH, not the approved $3 — something it names has moved since it was rendered (above: the plan as it stands). Nothing was pushed; render and approve it again"
}
echo "$ME: plan $HASH still holds — landing $MERGE on $TENANT_REPO $REF (was $MAIN)"
if ! out=$(g push --force-with-lease="refs/heads/$REF:$MAIN" "$URL" "$MERGE:refs/heads/$REF" 2>&1); then
    fail "the forge refused the push (a $REF that moved since the re-render is refused here, not overwritten): $(redact_url <<< "$out")"
fi
# A forge answer is not a forge effect: read main back.
now=$(git ls-remote "$URL" "refs/heads/$REF" 2>/dev/null | cut -f1)
[ "$now" = "$MERGE" ] || fail "the push answered but $REF reads ${now:-nothing}, not $MERGE"
echo "$ME: $TENANT_REPO $REF is $MERGE"
