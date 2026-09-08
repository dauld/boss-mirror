#!/usr/bin/env bash
#
# publish-github-pr — open the public mirror's pull request BY MACHINE.
#
# The machine half of publish-to-github v6 (design 7b59af2c, David
# 2026-09-08: the mirror at github.com/algedonic-dev/boss is "strictly a
# backup of source protocol" and the PR "should open from our dauld
# GitHub account"). When David signs the protocol's approve step, the
# dispatcher rule publish-github-pr-on-open-pr-ready files an
# ops-request for the forge host, and the root ops-runner runs THIS
# script with no arguments. It:
#
#   1. finds the one open publish-to-github packet whose `open-pr` step
#      is ready (one mirror, one open packet — the daily rule's guard);
#   2. fetches forge main from the Forgejo repository ON THIS HOST (a
#      path, no forge credential) and the mirror's main from GitHub
#      (anonymous; the repo is public);
#   3. builds the dated SNAPSHOT commit: `git commit-tree <forge main
#      tree> -p <mirror main>`. The mirror's history is publish
#      snapshots, not the forge's history — a merge of forge main
#      conflicts in hundreds of files (measured 553 on 2026-09-08), and
#      a snapshot whose tree IS forge main's tree is the honest backup;
#   4. pushes it to the dauld fork as publish/<date> (recreating the
#      fork once if it is gone), opens the PR against algedonic-dev/boss
#      as dauld, and completes the packet's open-pr step with pr_url.
#
# THE MERGE ON GITHUB STAYS DAVID'S — the second gate. Nothing here
# touches the mirror's main.
#
# THE TOKEN. dauld's GitHub token is provisioned by David's token admin
# at $BOSS_GITHUB_TOKEN_FILE (default /etc/boss-publish/github.token,
# root:root 0600, one line) and declared in the credentials registry
# (id dauld-github-token). This script only ever READS it, hands it to
# git through a credential helper and to gh through GH_TOKEN on those
# two child processes, and never prints it. A missing or world-readable
# token file is a loud refusal naming the path — never a silent skip.
#
# --check: validate inputs (tools, token file, forge repository, state
# dir) with no network and exit 0/1. What the gate lint runs.
#
# Idempotent: re-running the same day force-updates publish/<date> on
# the fork and reuses an already-open PR for that head. A verb killed
# mid-way leaves at worst a pushed branch on our own fork.
#
# Runs as root under the ops-runner with NO HOME: every path is explicit
# (state dir, GH_CONFIG_DIR) and nothing reads $HOME.
set -euo pipefail

TOKEN_FILE="${BOSS_GITHUB_TOKEN_FILE:-/etc/boss-publish/github.token}"
STATE_DIR="${BOSS_PUBLISH_STATE_DIR:-/var/lib/boss-publish}"
# The Forgejo container's bind mount (/opt/forgejo/data = /data in the
# container; repositories live under git/repositories). Root on this
# host reads it as a plain path — no token, no network.
FORGE_REPO="${BOSS_FORGE_REPO_PATH:-/opt/forgejo/data/git/repositories/david/boss.git}"
MIRROR_SLUG="${BOSS_MIRROR_SLUG:-algedonic-dev/boss}"
MIRROR_URL="${BOSS_MIRROR_URL:-https://github.com/${MIRROR_SLUG}.git}"
FORK_SLUG="${BOSS_FORK_SLUG:-dauld/boss}"
FORK_URL="${BOSS_FORK_URL:-https://github.com/${FORK_SLUG}.git}"
FORK_OWNER="${FORK_SLUG%%/*}"
AUTHOR_NAME="${BOSS_PUBLISH_AUTHOR_NAME:-dauld}"
AUTHOR_EMAIL="${BOSS_PUBLISH_AUTHOR_EMAIL:-dauld@users.noreply.github.com}"
DATE="${BOSS_PUBLISH_DATE:-$(date -u +%Y-%m-%d)}"
BRANCH="publish/${DATE}"
CLONE="${STATE_DIR}/boss.git"
export GH_CONFIG_DIR="${GH_CONFIG_DIR:-${STATE_DIR}/gh}"
ACTOR="${BOSS_OPS_ACTOR:-automation:publish-github-pr}"
BOSS_USER="{\"id\":\"$ACTOR\",\"role\":\"platform-admin\",\"access_tier\":\"operator\",\"territory_account_ids\":[],\"direct_report_ids\":[],\"department\":\"platform\"}"

me="publish-github-pr"
say() { echo "$me: $*"; }
refuse() { echo "$me: REFUSED — $*" >&2; exit 2; }
fail() { echo "$me: FAILED — $*" >&2; exit 1; }

# ---------------------------------------------------------------------
# Preconditions — the same list --check reports on.
# ---------------------------------------------------------------------
token_problem() {
    if [ ! -e "$TOKEN_FILE" ]; then
        echo "the dauld GitHub token is not at $TOKEN_FILE — David's token admin provisions it (root:root 0600, the token on one line); set BOSS_GITHUB_TOKEN_FILE if it lives elsewhere"
    elif [ ! -r "$TOKEN_FILE" ]; then
        echo "$TOKEN_FILE exists but is not readable by this user"
    elif [ ! -s "$TOKEN_FILE" ]; then
        echo "$TOKEN_FILE is empty"
    else
        local mode
        mode=$(stat -c %a "$TOKEN_FILE" 2>/dev/null || echo "?")
        case "$mode" in
            600|400) ;;
            *) echo "$TOKEN_FILE is mode $mode — a token file must be 0600 or 0400 (chmod 600 $TOKEN_FILE)" ;;
        esac
    fi
}

check_inputs() {
    local rc=0 problem
    for tool in git gh curl jq; do
        if ! command -v "$tool" >/dev/null 2>&1; then
            echo "$me: missing tool on PATH: $tool" >&2; rc=1
        fi
    done
    problem=$(token_problem)
    if [ -n "$problem" ]; then echo "$me: $problem" >&2; rc=1; fi
    if ! git -C "$FORGE_REPO" rev-parse --is-bare-repository >/dev/null 2>&1 \
       && ! git -C "$FORGE_REPO" rev-parse --git-dir >/dev/null 2>&1; then
        echo "$me: forge repository not found at $FORGE_REPO (BOSS_FORGE_REPO_PATH)" >&2; rc=1
    fi
    if ! mkdir -p "$STATE_DIR" 2>/dev/null || [ ! -w "$STATE_DIR" ]; then
        echo "$me: state dir $STATE_DIR is not writable (BOSS_PUBLISH_STATE_DIR)" >&2; rc=1
    fi
    return $rc
}

if [ "${1:-}" = "--check" ]; then
    # No network, no push, no packet: does this host hold what a run
    # needs, and where does it read each thing from?
    echo "$me --check"
    echo "  token file : $TOKEN_FILE"
    echo "  forge repo : $FORGE_REPO"
    echo "  mirror     : $MIRROR_URL (anonymous fetch)"
    echo "  fork       : $FORK_URL (push as $FORK_OWNER)"
    echo "  state dir  : $STATE_DIR"
    echo "  jobs api   : ${BOSS_JOBS_URL:-<unset — the ops-runner pins it on its Exec line>}"
    if check_inputs; then
        echo "$me: --check ok"
        exit 0
    fi
    echo "$me: --check FAILED — see above" >&2
    exit 1
fi

# ---------------------------------------------------------------------
# A run.
# ---------------------------------------------------------------------
[ -n "${BOSS_JOBS_URL:-}" ] || refuse "BOSS_JOBS_URL is not set; the ops-runner pins it on its Exec line and a hand run must name the system of record"
BASE="${BOSS_JOBS_URL%/}"
check_inputs || refuse "inputs incomplete (see above); nothing was fetched or pushed"

workdir=$(mktemp -d) || exit 1
trap 'rm -rf "$workdir"' EXIT

# 1. The packet. One mirror, one open publish packet (149's guard), and
#    its open-pr must be ready or active — a rule fired on readiness.
if ! curl -fsS -H "x-boss-user: $BOSS_USER" \
        "$BASE/api/jobs?kind=publish-to-github&status=open&limit=20" > "$workdir/jobs" 2> "$workdir/err"; then
    fail "jobs API unreachable at $BASE — $(cat "$workdir/err")"
fi
target=$(jq -c '
    (if type == "object" and has("data") then .data else . end)
    | map(select(.status == "open"))
    | map({id, title, step: (((.steps // []) | map(select(.spec_slug == "open-pr")) | .[0])
                            // ((.steps // []) | map(select(.title == "open-pr")) | .[0]))})
    | map(select(.step != null and (.step.status == "ready" or .step.status == "active")))
    | .[0] // empty' "$workdir/jobs")
if [ -z "$target" ]; then
    say "no open publish-to-github packet has its open-pr step ready — nothing to do"
    exit 0
fi
job_id=$(printf '%s' "$target" | jq -r '.id')
step_id=$(printf '%s' "$target" | jq -r '.step.id')
say "packet ${job_id:0:8} — open-pr ready; publishing forge main as $BRANCH"

# 2. Refs. A private bare clone under the state dir; the forge is read
#    as a path on this host, the mirror anonymously.
mkdir -p "$STATE_DIR" "$GH_CONFIG_DIR"
[ -d "$CLONE" ] || git init -q --bare "$CLONE"
g() { git -C "$CLONE" "$@"; }
set_remote() { g remote get-url "$1" >/dev/null 2>&1 && g remote set-url "$1" "$2" || g remote add "$1" "$2"; }
set_remote forge "$FORGE_REPO"
set_remote mirror "$MIRROR_URL"
set_remote fork "$FORK_URL"
g fetch -q forge "+refs/heads/main:refs/remotes/forge/main"   || fail "fetching forge main from $FORGE_REPO"
g fetch -q mirror "+refs/heads/main:refs/remotes/mirror/main" || fail "fetching mirror main from $MIRROR_URL"
forge_head=$(g rev-parse refs/remotes/forge/main)
forge_tree=$(g rev-parse "refs/remotes/forge/main^{tree}")
mirror_head=$(g rev-parse refs/remotes/mirror/main)
mirror_tree=$(g rev-parse "refs/remotes/mirror/main^{tree}")
if [ "$forge_tree" = "$mirror_tree" ]; then
    fail "the mirror's main already carries forge main's tree ($forge_head) — nothing to publish; open-pr on ${job_id:0:8} stays open for a person to close or supersede"
fi

# 3. The snapshot commit. Tree = forge main's tree, parent = the
#    mirror's main. Authored as dauld; the forge sha rides in the body.
snapshot=$(GIT_AUTHOR_NAME="$AUTHOR_NAME" GIT_AUTHOR_EMAIL="$AUTHOR_EMAIL" \
           GIT_COMMITTER_NAME="$AUTHOR_NAME" GIT_COMMITTER_EMAIL="$AUTHOR_EMAIL" \
           g commit-tree "$forge_tree" -p "$mirror_head" \
             -m "publish: $DATE" \
             -m "Snapshot of forge main $forge_head onto the public mirror. The mirror is a backup of source: each publish is one commit whose tree is forge main's tree at that moment (a merge of the forge history conflicts in hundreds of files). Opened by machine (BOSS publish-to-github v6, ops verb publish-github-pr) from packet $job_id; merged by a person.") \
    || fail "git commit-tree"
say "snapshot $snapshot (tree $forge_tree of forge $forge_head, parent mirror $mirror_head)"

# 4. The fork, the push, the PR — the only three steps that need the
#    token. It reaches git through a credential helper that reads the
#    FILE (the gate-runner's idiom) and gh through GH_TOKEN on that one
#    process; neither is ever echoed.
helper="!f() { echo username=x-access-token; echo \"password=\$(cat '$TOKEN_FILE')\"; }; f"
gh_t() { GH_TOKEN="$(cat "$TOKEN_FILE")" gh "$@"; }

if ! gh_t repo view "$FORK_SLUG" --json name >/dev/null 2>"$workdir/err"; then
    say "fork $FORK_SLUG not found ($(head -c 200 "$workdir/err" | tr '\n' ' ')) — forking $MIRROR_SLUG once"
    gh_t repo fork "$MIRROR_SLUG" --clone=false >/dev/null 2>"$workdir/err" \
        || fail "gh repo fork $MIRROR_SLUG: $(head -c 300 "$workdir/err" | tr '\n' ' ')"
fi

g -c "credential.helper=$helper" push -q --force fork "$snapshot:refs/heads/$BRANCH" 2>"$workdir/err" \
    || fail "pushing $BRANCH to $FORK_URL: $(head -c 300 "$workdir/err" | tr '\n' ' ')"
say "pushed $FORK_OWNER:$BRANCH"

pr_url=$(gh_t pr list --repo "$MIRROR_SLUG" --head "$FORK_OWNER:$BRANCH" --state open --json url --jq '.[0].url // empty' 2>"$workdir/err") \
    || fail "gh pr list: $(head -c 300 "$workdir/err" | tr '\n' ' ')"
if [ -n "$pr_url" ]; then
    say "PR already open for $FORK_OWNER:$BRANCH — reusing $pr_url"
else
    pr_url=$(gh_t pr create --repo "$MIRROR_SLUG" --base main --head "$FORK_OWNER:$BRANCH" \
        --title "publish: $DATE" \
        --body "Backup-of-source publish from the internal forge: snapshot \`$snapshot\` carries forge main \`$forge_head\` as one commit on top of the mirror's \`$mirror_head\`. Opened by machine (BOSS publish-to-github v6, packet $job_id); the merge is a person's." \
        2>"$workdir/err") || fail "gh pr create: $(head -c 300 "$workdir/err" | tr '\n' ' ')"
    say "opened $pr_url"
fi

# 5. Complete open-pr with pr_url. Merge, never replace (the
#    ops-runner's rule): PUT swaps metadata wholesale.
printf '%s' "$target" | jq -c --arg url "$pr_url" --arg snap "$snapshot" \
        --arg fh "$forge_head" --arg mh "$mirror_head" --arg br "$FORK_OWNER:$BRANCH" '
    {status: "completed",
     metadata: ((.step.metadata // {})
                + {pr_url: $url, snapshot_commit: $snap, forge_head: $fh,
                   mirror_head: $mh, head: $br, published_by: "publish-github-pr"})}' \
    > "$workdir/payload"
if ! curl -fsS -X PUT -H "content-type: application/json" -H "x-boss-user: $BOSS_USER" \
        ${BOSS_MACHINE_TOKEN:+-H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"} \
        --data-binary @"$workdir/payload" \
        "$BASE/api/jobs/$job_id/steps/$step_id" > /dev/null 2>"$workdir/err"; then
    fail "the PR is open at $pr_url but completing open-pr on ${job_id:0:8} failed — $(head -c 300 "$workdir/err" | tr '\n' ' '); record pr_url on the step by hand"
fi

say "done — $pr_url (open-pr on ${job_id:0:8} completed; the merge is David's)"
