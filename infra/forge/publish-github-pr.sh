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
# dir) with no network and exit 0/1. What the gate lint runs. It also
# PERFORMS the forge fetch, into a throwaway repository it deletes,
# because that fetch's permission model differs from a direct read and a
# --check that does not run it passes where the publish fails — which is
# what happened on 2026-09-11 (--check ok at 13:41, publish FAILED at
# 13:43 on the same host, mirror 271 commits behind).
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
# WHERE THE FORGE REPOSITORY IS — derived, not asserted. See the block
# below the helpers; these are its inputs.
FORGE_COMPOSE="${BOSS_FORGE_COMPOSE:-/opt/forgejo/docker-compose.yml}"
FORGE_REPO_SLUG="${BOSS_FORGE_REPO_SLUG:-david/boss}"
FORGE_DATA_FALLBACK="${BOSS_FORGE_DATA_FALLBACK:-/opt/forgejo/data}"
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
# WHERE THE FORGE REPOSITORY IS — derived from the thing that declares
# it, not asserted by this file.
# ---------------------------------------------------------------------
# Until 2026-09-11 this was one hardcoded default,
# /opt/forgejo/data/git/repositories/david/boss.git, and that string
# appeared EXACTLY ONCE in the tree — here — with the only other
# references being test overrides that substitute a tmpdir. So it had
# never been run against the real host, and the verb's real run had
# never succeeded: ops-request 04975694 ran `--check` on the forge and
# the repository path was its one and only failure (backlog ed84b5d9).
#
# Forgejo runs on that host as a container (codeberg.org/forgejo/forgejo
# :16.0.2, measured on ops-request aa0118a6, 2026-09-11) and its compose
# file DECLARES which host directory is mounted at the container's
# /data. That declaration is the one definition of where the
# repositories live, so read it (CLAUDE.md §9a: one definition, never a
# second copy in a shell default). Inside /data, the repository root is
# Forgejo's OWN `[repository] ROOT` from app.ini when that is readable;
# git/repositories is only the image's default.
#
# Every layer is reported by --check, labelled with where it came from,
# so the next reader never has to guess which one answered.
# BOSS_FORGE_REPO_PATH overrides the lot.

# The host directory bound to the container's /data. Compose's short
# syntax (`- ./data:/data[:ro]`) and long syntax (`source:`/`target:`)
# both appear in Forgejo's published examples, so both are read. A NAMED
# volume (`forgejo-data:/data`) is not a host path and is declined.
compose_data_dir() {
    local compose="$1" here host
    [ -r "$compose" ] || return 1
    here=$(cd "$(dirname "$compose")" 2>/dev/null && pwd) || return 1
    host=$(sed -n -E 's@^[[:space:]]*-[[:space:]]*"?([^":[:space:]]+):/data(:[a-zA-Z,]+)?"?[[:space:]]*$@\1@p' "$compose" | head -n 1)
    if [ -z "$host" ]; then
        host=$(awk '
            /^[[:space:]]*-?[[:space:]]*source:[[:space:]]*[^[:space:]]+[[:space:]]*$/ {
                s = $NF; gsub(/"/, "", s)
            }
            /^[[:space:]]*target:[[:space:]]*\/data[[:space:]]*$/ {
                if (s != "") { print s; exit }
            }' "$compose")
    fi
    case "$host" in
        /*)       printf '%s\n' "$host" ;;
        ./*|../*) printf '%s\n' "$here/${host#./}" ;;
        *)        return 1 ;;
    esac
}

# Forgejo's own [repository] ROOT, read off app.ini under the data dir
# and translated from the container's /data to the host directory. Only
# the [repository] section's ROOT — app.ini has other ROOT-ish keys.
forge_repo_root() {
    local data="$1" ini root
    ini="$data/gitea/conf/app.ini"
    if [ -r "$ini" ]; then
        root=$(awk '
            /^[[:space:]]*\[/ { sec = $0 }
            sec ~ /^[[:space:]]*\[repository\]/ && /^[[:space:]]*ROOT[[:space:]]*=/ {
                sub(/^[^=]*=[[:space:]]*/, ""); sub(/[[:space:]]+$/, ""); print; exit
            }' "$ini")
        case "$root" in
            /data/*) printf '%s\n' "$data${root#/data}"; return 0 ;;
        esac
    fi
    printf '%s\n' "$data/git/repositories"
}

if [ -n "${BOSS_FORGE_REPO_PATH:-}" ]; then
    FORGE_REPO="$BOSS_FORGE_REPO_PATH"
    FORGE_REPO_FROM="BOSS_FORGE_REPO_PATH in the environment"
elif FORGE_DATA=$(compose_data_dir "$FORGE_COMPOSE"); then
    FORGE_REPO_ROOT=$(forge_repo_root "$FORGE_DATA")
    FORGE_REPO="$FORGE_REPO_ROOT/$FORGE_REPO_SLUG.git"
    FORGE_REPO_FROM="derived: $FORGE_COMPOSE mounts $FORGE_DATA at the container's /data, repository root $FORGE_REPO_ROOT, slug $FORGE_REPO_SLUG"
else
    FORGE_REPO="$FORGE_DATA_FALLBACK/git/repositories/$FORGE_REPO_SLUG.git"
    FORGE_REPO_FROM="fallback — $FORGE_COMPOSE is not readable, so the host's /data mount could not be read and this path is a GUESS at the image default; name the real one with BOSS_FORGE_REPO_PATH"
fi

# EVERY READ OF THE FORGE REPOSITORY GOES THROUGH THIS ONE CHANNEL,
# --check and the run alike, so --check can never pass on a repository
# the run cannot read. The working directory is created HERE, above
# --check, because the channel is a file and this is where it lives.
workdir=$(mktemp -d) || { echo "$me: FAILED — no working directory under ${TMPDIR:-/tmp}" >&2; exit 1; }
trap 'rm -rf "$workdir"' EXIT

# safe.directory, scoped to this one path. The ops-runner executes verbs
# AS ROOT and this repository belongs to the Forgejo container's
# account, so since git 2.35.2 every command refuses it as "dubious
# ownership" — the same refusal, measured on this host class on
# ops-request c9877f75 (2026-09-10), that made delete-orphan-object's
# read impossible. That script drops to the owner instead; this one
# cannot, because the fetch's DESTINATION is root's own state dir under
# /var/lib, which the owner cannot write. The hazard the drop protects
# against — a root WRITE leaving root-owned objects in somebody else's
# repository — is not reachable here: a fetch only reads the source, and
# nothing in this script writes under $FORGE_REPO. That is also why the
# exemption names $FORGE_REPO exactly and never `*`: it is right for a
# READER and wrong where root writes, which is why
# infra/forge/delete-orphan-object.sh rejects it by name.
#
# WHY A FILE AND NOT `-c safe.directory=…`. Until 2026-09-11 it was the
# `-c` form, and `-c` CANNOT exempt a fetch SOURCE. A local fetch runs
# `git upload-pack` IN THE SOURCE REPOSITORY, and git clears the
# command-line config when it crosses into another repository — git's own
# trace says so, verbatim:
#
#   run_command: unset GIT_CONFIG_PARAMETERS … git-upload-pack '<src>'
#
# so that child runs its ownership check with no exemption at all. A
# DIRECT read in this process does honour `-c`, which is exactly how
# `--check` passed while the publish failed. Measured three ways on git
# 2.39.5, one destination exempted by a protected file so the only
# variable was how the SOURCE was exempted:
#
#   source via `-c` only (the old shape)  exit 128, dubious ownership
#   source via GIT_CONFIG_GLOBAL file     the fetch succeeds
#   source not exempted (control)         exit 128, dubious ownership
#
# GIT_CONFIG_GLOBAL is protected configuration AND is not in the set git
# clears crossing repositories, so the child inherits it. The file lives
# in $workdir — per-run, 0700, removed by the trap — never a fixed path
# under /tmp that another run or another user could have planted.
FORGE_SAFE_CONFIG="$workdir/forge-safe.gitconfig"
printf '[safe]\n\tdirectory = %s\n' "$FORGE_REPO" > "$FORGE_SAFE_CONFIG"
forge_git() { GIT_CONFIG_GLOBAL="$FORGE_SAFE_CONFIG" git "$@"; }

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

# The deepest prefix of a path that exists — so "absent" can say how far
# the layout WAS right instead of only that the whole path was wrong.
deepest_existing() {
    local p="$1"
    while [ -n "$p" ] && [ "$p" != "/" ] && [ "$p" != "." ]; do
        if [ -e "$p" ]; then printf '%s' "$p"; return 0; fi
        p=$(dirname "$p")
    done
    printf '/'
}

# FOUR findings, four sentences. Until 2026-09-11 all four reported
# "forge repository not found at <path>" — the same words whether the
# path was absent, was a file, was unreadable by the caller, or was
# readable and refused by git. That is CLAUDE.md §Doors ("a wrong target
# answers instead of erroring") in its most literal form: the first
# reading of the live refusal could not tell a wrong path from a
# permission boundary, so the obvious next step was to guess a second
# path. The verb runs as root through the ops-runner, so "unreadable by
# root" and "unreadable by david" are different findings and the message
# names which user could not read it.
forge_repo_problem() {
    local who anc err
    who="$(id -un 2>/dev/null || id -u 2>/dev/null || printf '?')"
    if [ ! -e "$FORGE_REPO" ]; then
        anc=$(deepest_existing "$FORGE_REPO")
        if [ -r "$anc" ] && [ -x "$anc" ]; then
            echo "no forge repository at $FORGE_REPO — nothing exists there; the deepest path that does exist is $anc. Path came from: $FORGE_REPO_FROM"
        else
            echo "no forge repository at $FORGE_REPO — nothing exists there that this user ($who) can see, and $anc is not searchable by $who, so 'absent' here may mean 'hidden'. Path came from: $FORGE_REPO_FROM"
        fi
    elif [ ! -d "$FORGE_REPO" ]; then
        echo "the forge repository path $FORGE_REPO exists but is not a directory — a bare git repository was expected. Path came from: $FORGE_REPO_FROM"
    elif [ ! -r "$FORGE_REPO" ] || [ ! -x "$FORGE_REPO" ]; then
        echo "the forge repository $FORGE_REPO is a directory but is not readable by this user ($who) — a permission finding, not a wrong path. Path came from: $FORGE_REPO_FROM"
    elif ! err=$(forge_git -C "$FORGE_REPO" rev-parse --git-dir 2>&1 >/dev/null); then
        echo "the forge repository $FORGE_REPO is a readable directory but git refused it as a repository (read as $who). git said: ${err%%$'\n'*}"
    fi
}

# A FIFTH finding, and the one the four above could not reach: the path
# is a git repository this user can read, and the FETCH still fails.
# `git -C <repo> rev-parse` and `git fetch <repo>` do not share a
# permission model — the first reads in this process, the second spawns
# upload-pack inside <repo> — so a check that only does the first passes
# where the run fails. This performs the run's fetch, refspec and all,
# into a bare repository under $workdir that the trap deletes. Measured
# against this tree (188 MB git dir) it costs 0.65 s and 11 MB transient;
# the cost of NOT doing it was 271 commits.
forge_fetch_problem() {
    local probe="$workdir/forge-read-probe.git" err="$workdir/probe-err" who sha
    who="$(id -un 2>/dev/null || id -u 2>/dev/null || printf '?')"
    if ! git init -q --bare "$probe" 2>"$err"; then
        printf 'NOT ATTEMPTED — no probe repository under %s' "$workdir" > "$workdir/fetch-note"
        echo "the fetch probe repository could not be created under $workdir (as $who). git said: $(head -c 200 "$err" | tr '\n' ' ')"
        return 0
    fi
    if ! forge_git -C "$probe" fetch -q "$FORGE_REPO" "+refs/heads/main:refs/boss-publish/probe" 2>"$err"; then
        printf 'FAILED as %s — %s' "$who" "$(head -c 300 "$err" | tr '\n' ' ')" > "$workdir/fetch-note"
        echo "the forge repository $FORGE_REPO is a git repository $who can read, but the FETCH the publish performs fails from it — a different permission model, not a wrong path. git said: $(head -c 300 "$err" | tr '\n' ' '). Path came from: $FORGE_REPO_FROM"
        return 0
    fi
    sha=$(git -C "$probe" rev-parse refs/boss-publish/probe 2>/dev/null || printf '?')
    # Freed here, not only by the trap: the forge host has hit its disk
    # floor before (backlog 99696e43) and the probe's pack is the one
    # thing in this verb measured in megabytes.
    rm -rf "$probe"
    printf 'ok — refs/heads/main is %s, fetched as %s into a throwaway repository' "$sha" "$who" > "$workdir/fetch-note"
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
    problem=$(forge_repo_problem)
    if [ -n "$problem" ]; then
        echo "$me: $problem" >&2; rc=1
    else
        # Only once the path IS a readable repository is the fetch a
        # question worth asking; before that it restates the finding above.
        problem=$(forge_fetch_problem)
        if [ -n "$problem" ]; then echo "$me: $problem" >&2; rc=1; fi
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
    echo "               ($FORGE_REPO_FROM)"
    echo "  mirror     : $MIRROR_URL (anonymous fetch)"
    echo "  fork       : $FORK_URL (push as $FORK_OWNER)"
    echo "  state dir  : $STATE_DIR"
    echo "  jobs api   : ${BOSS_JOBS_URL:-<unset — the ops-runner pins it on its Exec line>}"
    if check_inputs; then rc=0; else rc=1; fi
    echo "  forge fetch: $(cat "$workdir/fetch-note" 2>/dev/null || printf 'NOT ATTEMPTED — the forge repository findings above say why')"
    if [ "$rc" -eq 0 ]; then
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
# Git's own words on both fetches: a verdict somebody must go re-derive
# is not a verdict (CLAUDE.md §Diagnosis).
forge_git -C "$CLONE" fetch -q forge "+refs/heads/main:refs/remotes/forge/main" 2>"$workdir/err" \
    || fail "fetching forge main from $FORGE_REPO ($FORGE_REPO_FROM) — git said: $(head -c 300 "$workdir/err" | tr '\n' ' ')"
g fetch -q mirror "+refs/heads/main:refs/remotes/mirror/main" 2>"$workdir/err" \
    || fail "fetching mirror main from $MIRROR_URL — git said: $(head -c 300 "$workdir/err" | tr '\n' ' ')"
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
