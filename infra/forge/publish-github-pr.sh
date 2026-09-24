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
#      as dauld, and completes the packet's open-pr step with pr_url;
#   5. closes each OLDER open publish/<date> PR from the fork as
#      superseded by today's, which contains it (backlog d4bfe548) —
#      so there is only ever one PR to merge.
#
# THE MERGE ON GITHUB STAYS DAVID'S — the second gate. Nothing here
# touches the mirror's main; the only PRs it closes are its own older
# publish snapshots.
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

# shellcheck source=infra/lib/jq.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/jq.sh"

TOKEN_FILE="${BOSS_GITHUB_TOKEN_FILE:-/etc/boss-publish/github.token}"
STATE_DIR="${BOSS_PUBLISH_STATE_DIR:-/var/lib/boss-publish}"
# WHERE THE FORGE REPOSITORY IS — derived, not asserted. See the block
# below the helpers; these are its inputs.
FORGE_COMPOSE="${BOSS_FORGE_COMPOSE:-/opt/forgejo/docker-compose.yml}"
FORGE_REPO_SLUG="${BOSS_FORGE_REPO_SLUG:-david/boss}"
FORGE_DATA_FALLBACK="${BOSS_FORGE_DATA_FALLBACK:-/opt/forgejo/data}"
# THE MIRROR — where it is spelled: infra/estate/estate.toml, rendered
# onto this host as /etc/boss/sor.env (infra/lib/sor.sh), never here.
# Both overrides are taken FIRST and the file is read only when one is
# missing: sourcing sor.sh with BOSS_SOR_ENV named REPLACES what the
# environment carried, and the tests point this verb at fixture
# repositories through exactly these two variables. The clone URL is the
# declared URL plus `.git` — the same string the literal built until
# 2026-09-20, when it was one of four copies owned by nothing (backlog
# f8af6040), one of them inside another script's refusal message.
_mirror_slug="${BOSS_MIRROR_SLUG:-}"
_mirror_url="${BOSS_MIRROR_URL:-}"
if [ -z "$_mirror_slug" ] || [ -z "$_mirror_url" ]; then
    # shellcheck source=infra/lib/sor.sh
    . "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/sor.sh"
    sor_require BOSS_MIRROR_SLUG BOSS_MIRROR_URL
fi
MIRROR_SLUG="${_mirror_slug:-$BOSS_MIRROR_SLUG}"
MIRROR_URL="${_mirror_url:-${BOSS_MIRROR_URL}.git}"
# THE FORK — dauld/boss-mirror, which is the one repository in
# algedonic-dev/boss's fork network that dauld owns. Until 2026-09-11 this
# default read `dauld/boss`, and NOTHING in the tree ever set
# BOSS_FORK_SLUG (it appeared exactly once, here), so the default was
# always what ran. Measured against GitHub's REST API that day, with a
# control on the same connection:
#
#   repos/dauld/boss          -> 404 (David's unrelated PRIVATE repository)
#   repos/algedonic-dev/boss  -> 200  fork=false parent=none
#   repos/dauld/boss-mirror   -> 200  fork=true  parent=algedonic-dev/boss
#
# The push to dauld/boss SUCCEEDED — the token authenticates for it — and
# `gh pr create` then failed with four GraphQL errors at once ("Head sha
# can't be blank", "Base sha can't be blank", "No commits between
# algedonic-dev:main and dauld:publish/2026-09-11", "Head ref must be a
# branch"), none of which names the cause: GitHub opens a pull request
# only between two repositories in ONE fork network, and dauld/boss is
# not in it. The slug was the symptom; the check below is the defect.
# THE FORGE PUSH (ce5339d6). The forge repository carries a push mirror
# to the SAME fork this verb opens PRs from — `git push --mirror` on
# every commit, which PRUNES any branch the forge lacks. PR #238 opened
# at 22:46:59Z on 2026-09-11 and GitHub closed it at 22:49:17Z, head
# deleted, two minutes and one train later; publish/2026-09-08 survived
# because it also existed on the forge. So the snapshot goes to the forge
# FIRST, under the same branch name, and the mirror carries it from
# there. This verb runs as root under the ops-runner, and a root push
# into Forgejo's repository would leave root-owned objects the forge's
# own user cannot collect — so the push runs as the host user whose
# login shell carries the forge credential helper (the converge's own
# arrangement, forge-converge.sh), over Forgejo's HTTP, from a clone
# made readable to it. Empty BOSS_FORGE_PUSH_AS runs the push inline
# (the test harness, whose fixture repo the test's uid owns).
# The forge's clone URL: BOSS_FORGE_PUSH_URL, else the forge base from
# /etc/boss/sor.env (infra/lib/sor.sh) with the product repository's
# path — the same owner every image repo lives under.
# THE CREDENTIAL IS THE CONVERGE'S OWN. The push runs as $FORGE_PUSH_AS
# over Forgejo's HTTP, and that user has no credential helper: measured
# 2026-09-19 04:55Z on ops-request 3d9d5f58 (the second approved publish)
# — `could not read Username for 'http://10.20.0.15:3000'` after the
# ownership refusal of the first was fixed. What the converge fetches
# through is the checkout's `forgejo` remote, whose URL carries the
# credential as userinfo (cluster-deploy-lib.sh derives every tenant URL
# from it, redacting it in every message). So with no BOSS_FORGE_PUSH_URL
# the push target is that remote's URL, read AS THE OWNER (the checkout
# is theirs); the bare sor.env URL is the fallback only when the checkout
# has no such remote, and the userinfo never reaches a message.
FORGE_PUSH_AS="${BOSS_FORGE_PUSH_AS-david}"
FORGE_CHECKOUT="${BOSS_FORGE_CHECKOUT:-/home/david/boss}"
redact_url() { sed -E 's#://[^/@[:space:]]+@#://<redacted>@#g'; }
checkout_remote_url() {
    if [ -n "$FORGE_PUSH_AS" ]; then
        runuser -l "$FORGE_PUSH_AS" -c "git -C '$FORGE_CHECKOUT' remote get-url forgejo" 2>/dev/null
    else
        git -C "$FORGE_CHECKOUT" remote get-url forgejo 2>/dev/null
    fi
}
if [ -z "${BOSS_FORGE_PUSH_URL:-}" ]; then
    BOSS_FORGE_PUSH_URL="$(checkout_remote_url || true)"
fi
if [ -z "${BOSS_FORGE_PUSH_URL:-}" ]; then
    # shellcheck source=infra/lib/sor.sh
    . "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/sor.sh"
    sor_require BOSS_FORGE_URL
    BOSS_FORGE_PUSH_URL="$BOSS_FORGE_URL/${BOSS_FORGE_OWNER:-david}/boss.git"
fi
FORGE_PUSH_URL="$BOSS_FORGE_PUSH_URL"
FORGE_PUSH_URL_SHOWN="$(printf '%s' "$FORGE_PUSH_URL" | redact_url)"
FORK_SLUG="${BOSS_FORK_SLUG:-dauld/boss-mirror}"
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
    host=$(sed -n -E 's@^[[:space:]]*-[[:space:]]*"?([^":[:space:]]+):/data(:[a-zA-Z,]+)?"?[[:space:]]*$@\1@p' "$compose" | sed -n 1p)
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

# The private bare clone both a publish and a --measure read through:
# defined here rather than in the run path below because --measure
# exits before it (one definition, not two — CLAUDE.md §9a).
g() { git -C "$CLONE" "$@"; }
set_remote() { g remote get-url "$1" >/dev/null 2>&1 && g remote set-url "$1" "$2" || g remote add "$1" "$2"; }

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
    # Named as NOT covered on purpose: --check holds no token and opens no
    # socket, so it cannot ask GitHub whether this is a fork of the mirror
    # — the question that a green --check preceded three failures on
    # without ever asking (2026-09-11). Saying so beats implying it.
    echo "  fork       : $FORK_URL (push as $FORK_OWNER; that it is a fork of $MIRROR_SLUG is checked at run time, with the token — not here)"
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
# --measure: RE-MEASURE the drift onto the OPEN publish packet.
#
# WHY (design cb38d806, backlog e1b6ddf7). `publish-to-github-daily`
# fires only on `NOT open_publish_exists("github-mirror")`, so one
# packet held at its approve sign-off retires the cadence for every day
# behind it — the fourth instance of a dedup guard silently retiring a
# cadence. The PII hold of 2026-09-17 -> 09-18 cost a week, and #239
# arrived as a 396-commit / 1319-file snapshot that no reader and no
# CodeQL run can read as a change. The answer decided in that design is
# that a hold must not cost the days behind it: the drift is measured
# again onto the packet already open, so the numbers the sign-off is
# given are today's.
#
# A MEASUREMENT, NOT A PUBLICATION. It makes the same two fetches a
# publish makes — the forge as a path on this host, the mirror
# anonymously — and stops there: no token is read, nothing is pushed,
# no pull request is opened. That is why its own verb file
# (infra/ops/verbs/mirror-drift.json) carries the word as a FIXED argv
# rather than a mode a packet selects: a rule can file the refresh
# without being able to file a publish.
#
# THE SECRETS SCAN RUNS OVER THE TREE THAT WOULD BE PUBLISHED — a
# throwaway worktree of forge main, whose own infra/lint/no-secrets.sh
# is the definition — never over this host's checkout, which is a
# different sha and belongs to another user. A scan that FINDS
# something is recorded as a finding for the reviewer (`FAILED`); a
# scan that cannot RUN answers `not yet` (75) and writes nothing, because
# no evidence is not a pass.
# ---------------------------------------------------------------------
if [ "${1:-}" = "--measure" ]; then
    not_yet() { echo "$me: not yet: $*" >&2; exit 75; }
    [ -n "${BOSS_JOBS_URL:-}" ] || refuse "BOSS_JOBS_URL is not set; the ops-runner pins it on its Exec line and a hand run must name the system of record"
    BASE="${BOSS_JOBS_URL%/}"
    for tool in git curl jq; do
        command -v "$tool" >/dev/null 2>&1 || refuse "missing tool on PATH: $tool"
    done
    problem=$(forge_repo_problem)
    [ -z "$problem" ] || refuse "$problem"
    mkdir -p "$STATE_DIR" 2>/dev/null || true
    [ -w "$STATE_DIR" ] || refuse "state dir $STATE_DIR is not writable (BOSS_PUBLISH_STATE_DIR)"

    # 0. THE PULL REQUESTS' STATE, ASKED OF GITHUB (backlog a5d4322c).
    #    On 2026-09-22 the publish region called #239 open for 86 hours
    #    and itself TROUBLED over it; GitHub said #239 had merged three
    #    days earlier. Nothing in the pipeline had ever asked: the
    #    region inferred a merge from a mirror head equalling the PR's
    #    snapshot, which GitHub's merge commit and squash never make
    #    true. This pass asks. For every publish packet whose open-pr
    #    recorded a pull request and whose `pr_state` does not already
    #    read closed, GET the public pulls API (no credential — the
    #    mirror is public, measured from the pod) and PATCH the answer
    #    onto THAT packet as `pr_state`. It runs BEFORE the open-packet
    #    lookup below because a publish packet closes at judge-checks,
    #    long before its PR merges — the PRs to ask about are on closed
    #    packets. A PR GitHub does not answer for is named and left
    #    unread: the region then says "never read", not "open".
    GITHUB_API="${BOSS_GITHUB_API:-https://api.github.com}"
    if ! curl -fsS -H "x-boss-user: $BOSS_USER" \
            "$BASE/api/jobs?kind=publish-to-github&limit=60" > "$workdir/published" 2>"$workdir/err"; then
        fail "jobs API unreachable at $BASE — $(cat "$workdir/err")"
    fi
    jq -c '(if type == "object" and has("data") then .data else . end)
        | .[] | . as $j
        | ((.steps // []) | map(select(.spec_slug == "open-pr")) | .[0].metadata.pr_url // "") as $url
        | select($url != "")
        | select((($j.metadata.pr_state // {}) | .pr_url == $url and .state == "closed") | not)
        | {id: $j.id, url: $url}' "$workdir/published" > "$workdir/unsettled" 2>"$workdir/err" \
        || fail "the jobs API answered something this verb cannot read as a packet list — $(head -c 200 "$workdir/err" | tr '\n' ' ')"
    # A limit is not a filter: say when the page did not reach the tail.
    listed=$(jq -r '(if type == "object" and has("data") then .data else . end) | length' "$workdir/published")
    listed_total=$(jq -r '.total? // empty' "$workdir/published")
    case "${listed_total:-empty}" in
        empty|*[!0-9]*) ;;
        *) [ "$listed_total" -le "$listed" ] \
            || say "--measure: read $listed of $listed_total publish packets — the $((listed_total - listed)) oldest were not asked about" ;;
    esac
    while IFS= read -r row; do
        pr_job=$(printf '%s' "$row" | jq -r '.id')
        pr_url=$(printf '%s' "$row" | jq -r '.url')
        pr_number="${pr_url##*/}"
        case "${pr_number:-empty}" in
            empty|*[!0-9]*)
                say "--measure: ${pr_job:0:8} recorded '$pr_url', which is not a pull request url — its state stays never read"
                continue ;;
        esac
        if ! curl -fsS -H "accept: application/vnd.github+json" \
                "$GITHUB_API/repos/$MIRROR_SLUG/pulls/$pr_number" > "$workdir/pr" 2>"$workdir/err"; then
            say "--measure: GitHub did not answer for $pr_url — $(head -c 200 "$workdir/err" | tr '\n' ' '); its state stays never read"
            continue
        fi
        if ! jq -c --arg url "$pr_url" --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" '
                select(.state == "open" or .state == "closed")
                | {pr_state: {pr_url: $url, number, state, merged: (.merged == true),
                              merged_at, closed_at, read_at: $ts,
                              read_by: "publish-github-pr --measure"}}' \
                "$workdir/pr" > "$workdir/pr-state" 2>"$workdir/err" || [ ! -s "$workdir/pr-state" ]; then
            say "--measure: GitHub's answer for $pr_url carries no open/closed state — its state stays never read"
            continue
        fi
        if ! curl -fsS -X PATCH -H "content-type: application/json" -H "x-boss-user: $BOSS_USER" \
                ${BOSS_MACHINE_TOKEN:+-H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"} \
                --data-binary @"$workdir/pr-state" \
                "$BASE/api/jobs/$pr_job/metadata" > /dev/null 2>"$workdir/err"; then
            fail "annotating ${pr_job:0:8} with the state of $pr_url failed — $(head -c 300 "$workdir/err" | tr '\n' ' ')"
        fi
        say "--measure: $pr_url is $(jq -r '.pr_state | "\(.state), merged=\(.merged)"' "$workdir/pr-state") — written onto ${pr_job:0:8}"
    done < "$workdir/unsettled"

    # 1. The packet. ANY open publish-to-github packet, at whatever step
    #    it is held — unlike a publish, which needs open-pr ready. One
    #    mirror, one open packet (the daily rule's guard), so the first
    #    open one is the one.
    if ! curl -fsS -H "x-boss-user: $BOSS_USER" \
            "$BASE/api/jobs?kind=publish-to-github&status=open&limit=20" > "$workdir/jobs" 2>"$workdir/err"; then
        fail "jobs API unreachable at $BASE — $(cat "$workdir/err")"
    fi
    job_id=$(jq -r '(if type == "object" and has("data") then .data else . end)
        | map(select(.status == "open")) | .[0].id // empty' "$workdir/jobs") \
        || fail "the jobs API answered something this verb cannot read as a packet list"
    if [ -z "$job_id" ]; then
        say "--measure: no open publish-to-github packet — nothing to refresh"
        exit 0
    fi

    # 2. The two refs, in the same private bare clone a publish uses.
    [ -d "$CLONE" ] || git init -q --bare "$CLONE"
    set_remote forge "$FORGE_REPO"
    set_remote mirror "$MIRROR_URL"
    forge_git -C "$CLONE" fetch -q forge "+refs/heads/main:refs/remotes/forge/main" 2>"$workdir/err" \
        || fail "fetching forge main from $FORGE_REPO ($FORGE_REPO_FROM) — git said: $(head -c 300 "$workdir/err" | tr '\n' ' ')"
    g fetch -q mirror "+refs/heads/main:refs/remotes/mirror/main" 2>"$workdir/err" \
        || fail "fetching mirror main from $MIRROR_URL — git said: $(head -c 300 "$workdir/err" | tr '\n' ' ')"
    forge_head=$(g rev-parse refs/remotes/forge/main)
    mirror_head=$(g rev-parse refs/remotes/mirror/main)
    ahead=$(g rev-list --count refs/remotes/mirror/main..refs/remotes/forge/main)
    behind=$(g rev-list --count refs/remotes/forge/main..refs/remotes/mirror/main)
    g diff --name-only refs/remotes/mirror/main refs/remotes/forge/main > "$workdir/changed"
    g diff --name-only --diff-filter=A refs/remotes/mirror/main refs/remotes/forge/main > "$workdir/new-files"
    files=$(grep -c . "$workdir/changed" || true)
    new_count=$(grep -c . "$workdir/new-files" || true)
    # The newly-public files a reviewer is asked to look at by name:
    # runbooks and infra topology describe how to reach and recover the
    # live system, which is a different disclosure from source code.
    grep -E '^(docs/runbooks/|infra/(cluster|caddy|forge)/)' "$workdir/new-files" > "$workdir/sensitive" || true
    sens_count=$(grep -c . "$workdir/sensitive" || true)

    # 3. The secrets scan, over the tree that would be published.
    tree="$workdir/tree"
    g worktree add -q --detach "$tree" refs/remotes/forge/main 2>"$workdir/err" \
        || not_yet "a worktree of forge main ($forge_head) could not be created under $workdir — git said: $(head -c 300 "$workdir/err" | tr '\n' ' '); nothing was measured"
    scan="${BOSS_SECRETS_SCAN:-$tree/infra/lint/no-secrets.sh}"
    if [ ! -f "$scan" ]; then
        g worktree remove --force "$tree" 2>/dev/null || true
        g worktree prune 2>/dev/null || true
        not_yet "no secrets scan at $scan, so what this tree would publish was never scanned; the drift was NOT written to ${job_id:0:8}"
    fi
    if ( cd "$tree" && bash "$scan" ) > "$workdir/scan" 2>&1; then
        secrets="clean"
    else
        secrets="FAILED"
    fi
    g worktree remove --force "$tree" 2>/dev/null || true
    g worktree prune 2>/dev/null || true

    # 4. The annotation. PATCH /api/jobs/{id}/metadata MERGES top-level
    #    keys, so the refresh lands beside everything the packet already
    #    carries; the job PUT would replace them.
    ts=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    if [ "$ahead" -gt 0 ]; then has_drift="true"; else has_drift="false"; fi
    jq -n --arg ahead "$ahead" --arg behind "$behind" --arg files "$files" \
          --arg new_count "$new_count" --arg sens "$sens_count" --arg secrets "$secrets" \
          --arg has_drift "$has_drift" --arg ts "$ts" \
          --arg forge_head "$forge_head" --arg mirror_head "$mirror_head" \
          --rawfile sensitive "$workdir/sensitive" '
        {drift_refresh: {
            commits_ahead: $ahead, commits_behind: $behind, files_changed: $files,
            newly_public: $new_count, newly_public_sensitive: $sens,
            newly_public_review: ($sensitive | split("\n") | map(select(. != "")) | .[0:20]),
            secrets_scan: $secrets, has_drift: $has_drift,
            forge_head: $forge_head, mirror_head: $mirror_head,
            measured_at: $ts, measured_by: "publish-github-pr --measure"},
         drift_refreshed_at: $ts}' > "$workdir/refresh" \
        || fail "the refresh could not be rendered as JSON"
    if ! curl -fsS -X PATCH -H "content-type: application/json" -H "x-boss-user: $BOSS_USER" \
            ${BOSS_MACHINE_TOKEN:+-H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"} \
            --data-binary @"$workdir/refresh" \
            "$BASE/api/jobs/$job_id/metadata" > /dev/null 2>"$workdir/err"; then
        fail "annotating ${job_id:0:8} with the refreshed drift failed — $(head -c 300 "$workdir/err" | tr '\n' ' ')"
    fi
    # An API's answer is not an API's effect — so READ THE PACKET, not
    # the write call's reply. This used to jq the PATCH's own response
    # body, and that door answers 204 with NO body: there was never
    # anything there to read (backlog b88a13d5). The result depended on
    # the host's jq. On jq-1.6 `jq -e` over an empty document exits 0,
    # so the check passed and verified nothing; elsewhere it exits
    # non-zero, so a daily rule reported FAILED on runs that had done
    # their work (ops-request 9340fd6e). A permanently-red check is one
    # nobody reads; a silently-vacuous one is worse.
    if ! curl -fsS -H "x-boss-user: $BOSS_USER" \
            ${BOSS_MACHINE_TOKEN:+-H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"} \
            "$BASE/api/jobs/$job_id" > "$workdir/readback" 2>"$workdir/err"; then
        fail "the refresh for ${job_id:0:8} was accepted but the packet could not be read back — $(head -c 300 "$workdir/err" | tr '\n' ' ')"
    fi
    # NO EVIDENCE IS NOT A PASS, and that has to be checked BEFORE the
    # comparison: an empty or unparseable read-back is what made this
    # check vacuous, and `jq -e` alone cannot tell it from a match.
    jq_doc_file "$workdir/readback" && jq -e 'type == "object"' "$workdir/readback" > /dev/null 2>&1 \
        || fail "the refresh for ${job_id:0:8} was accepted but the read-back of the packet answered nothing parseable — the measurement is not on the packet"
    # The line above has already refused a read-back holding no document,
    # so this compare cannot be handed silence.
    jq -e --arg ts "$ts" '((.data // .) | .metadata // {}) | .drift_refreshed_at == $ts' \
        "$workdir/readback" > /dev/null \
        || fail "the jobs API accepted the refresh for ${job_id:0:8} but the packet does not carry $ts — the measurement is not on the packet"

    say "--measure: refreshed ${job_id:0:8} — $ahead commit(s) ahead, $files file(s), $new_count newly public ($sens_count under runbooks/infra), secrets $secrets; forge ${forge_head:0:8} over mirror ${mirror_head:0:8} at $ts"
    exit 0
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

# A SIXTH REFUSAL — the fork must be a fork OF THE MIRROR, not merely a
# name that resolves. Until 2026-09-11 this asked `gh repo view
# "$FORK_SLUG" --json name`, which proves only that SOMETHING answers to
# that name. On 2026-09-11 something did: dauld/boss, David's unrelated
# private repository, outside algedonic-dev/boss's fork network entirely.
# So the auto-fork below was skipped (the repository "existed"), the
# snapshot was pushed into the wrong repository, and the failure surfaced
# one step later as four GraphQL errors that name no cause. CLAUDE.md
# §Doors, "a wrong target answers instead of erroring", and its corollary:
# before concluding something exists, ask a question whose answer
# distinguishes it from its namesake.
#
# The question is therefore the RELATIONSHIP. GitHub's REST repository
# object answers it in two fields — `parent.full_name` is what a fork was
# forked FROM, `source.full_name` is the root of its whole network, so a
# fork of a fork of the mirror still answers the mirror — and `gh api` is
# the stable way to read them. Three outcomes, three different things to
# do, and each says which one it took:
#
#   absent (404)       the auto-fork's case, and the only one it was ever
#                      written for. It forks UNDER $FORK_SLUG's own name
#                      (--fork-name): `gh repo fork` otherwise names the
#                      new fork after the upstream, which on this account
#                      is dauld/boss — the repository that caused this.
#   present, unrelated REFUSED, naming both slugs and what the repository
#                      says about itself. Never a push: the push is the
#                      one step here that puts our commit somewhere we did
#                      not choose, and it cannot be taken back from here.
#   present, a fork of the mirror — proceed, saying so.
fork_of_mirror() {
    # 0 = $1 is a fork whose parent or source is $MIRROR_SLUG; 1 = it is
    # not; 2 = there is no such repository (gh could not read it at all).
    # Either way $workdir/fork-says holds the repository's own words, so
    # the refusal quotes GitHub rather than paraphrasing it.
    local slug="$1" meta="$workdir/fork.json"
    if ! gh_t api "repos/$slug" > "$meta" 2>"$workdir/err"; then
        return 2
    fi
    # gh exited 0, which is not the same as gh having ANSWERED: an empty
    # body reaches `jq -r` as no document, so fork-says would be blank
    # and `jq -e` below would exit 0 — "yes, this is the fork, push to
    # it" on no evidence at all (d96e38ab). 1, not 2: 2 goes on to FORK
    # the repository, and a write is the wrong answer to a read that did
    # not happen. 1 refuses with these words and pushes nothing.
    if ! jq_doc_file "$meta"; then
        printf 'nothing readable — gh answered with no repository object' > "$workdir/fork-says"
        return 1
    fi
    jq -r '"fork=\(.fork // false) parent=\(.parent.full_name // "none") source=\(.source.full_name // "none") private=\(.private // "?")"' \
        "$meta" > "$workdir/fork-says" 2>/dev/null \
        || printf 'a repository object jq could not read' > "$workdir/fork-says"
    jq -e --arg m "$MIRROR_SLUG" '
        (.fork == true)
        and ((((.parent.full_name // "") | ascii_downcase) == ($m | ascii_downcase))
             or (((.source.full_name // "") | ascii_downcase) == ($m | ascii_downcase)))' \
        "$meta" >/dev/null 2>&1
}

fork_rc=0
fork_of_mirror "$FORK_SLUG" || fork_rc=$?
if [ "$fork_rc" -eq 0 ]; then
    say "fork $FORK_SLUG confirmed in $MIRROR_SLUG's network ($(cat "$workdir/fork-says"))"
elif [ "$fork_rc" -eq 2 ]; then
    say "fork $FORK_SLUG not found ($(head -c 200 "$workdir/err" | tr '\n' ' ')) — forking $MIRROR_SLUG once as ${FORK_SLUG#*/}"
    gh_t repo fork "$MIRROR_SLUG" --clone=false --fork-name "${FORK_SLUG#*/}" >/dev/null 2>"$workdir/err" \
        || fail "gh repo fork $MIRROR_SLUG --fork-name ${FORK_SLUG#*/}: $(head -c 300 "$workdir/err" | tr '\n' ' ')"
    # Re-read rather than assume. The repository we push to must be the
    # fork we just made, and "the command exited 0" is not that — it is
    # the same standard of evidence that failed today.
    fork_of_mirror "$FORK_SLUG" \
        || refuse "forked $MIRROR_SLUG as $FORK_SLUG, but GitHub does not then report $FORK_SLUG as a fork of $MIRROR_SLUG ($(cat "$workdir/fork-says" 2>/dev/null || printf 'it is still not there')) — nothing was pushed"
    say "forked $MIRROR_SLUG as $FORK_SLUG ($(cat "$workdir/fork-says"))"
else
    refuse "$FORK_SLUG exists on GitHub but is NOT a fork of $MIRROR_SLUG — it says $(cat "$workdir/fork-says"). A pull request can only be opened between two repositories in one fork network, so pushing $BRANCH there would succeed and then fail at gh pr create with errors that name no cause (measured 2026-09-11 against dauld/boss, a namesake outside the network). Point BOSS_FORK_SLUG at a fork of $MIRROR_SLUG, or rename $FORK_SLUG so this verb forks it itself. Nothing was pushed"
fi

# 4a. The forge first — see FORGE_PUSH_URL. Idempotent per day: --force
#     re-points the dated branch at today's snapshot, the same way the
#     fork push below does. A failure here opens nothing on GitHub.
# Readable to the pushing user: the bare clone only (public source),
# never the state dir's other contents — traversable, not listable.
chmod a+x "$STATE_DIR" 2>/dev/null || true
chmod -R a+rX "$CLONE" 2>/dev/null || true
# The push runs as $FORGE_PUSH_AS over a clone ROOT owns, and git ≥ 2.35.2
# refuses that as "dubious ownership" unless the PUSHING user's config
# exempts it. $FORGE_SAFE_CONFIG above is root's file in a 0700 workdir —
# `runuser -l` neither carries GIT_CONFIG_GLOBAL nor could that user read
# it. Measured 2026-09-18 23:05Z on ops-request c98a782f, the FIRST
# approved publish: `fatal: detected dubious ownership in repository at
# '/var/lib/boss-publish/boss.git'`, and the step sat ready five hours.
# `-c` is right here where the file was right above: a push reads the
# clone in THIS process (pack-objects stays in the same repository, so
# GIT_CONFIG_PARAMETERS survives); the fetch-source caveat does not apply.
forge_push_cmd="git -c 'safe.directory=$CLONE' -C '$CLONE' push -q --force '$FORGE_PUSH_URL' '$snapshot:refs/heads/$BRANCH'"
if [ -n "$FORGE_PUSH_AS" ]; then
    runuser -l "$FORGE_PUSH_AS" -c "$forge_push_cmd" 2>"$workdir/err" \
        || fail "pushing $BRANCH to the forge ($FORGE_PUSH_URL_SHOWN) as $FORGE_PUSH_AS: $(head -c 300 "$workdir/err" | redact_url | tr '\n' ' '). Without it on the forge, the push mirror prunes the PR's head at the next train"
    say "pushed publish/${BRANCH#publish/} to the forge as $FORGE_PUSH_AS ($FORGE_PUSH_URL_SHOWN) — the mirror carries it"
else
    bash -c "$forge_push_cmd" 2>"$workdir/err" \
        || fail "pushing $BRANCH to the forge ($FORGE_PUSH_URL_SHOWN): $(head -c 300 "$workdir/err" | redact_url | tr '\n' ' ')"
    say "pushed publish/${BRANCH#publish/} to the forge ($FORGE_PUSH_URL_SHOWN) — the mirror carries it"
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

# 4b. ONE PULL REQUEST AT A TIME (backlog d4bfe548, David 2026-09-24).
#     Every snapshot's parent is the mirror's main, which moves only
#     when David merges, so today's PR CONTAINS every older open one.
#     Measured 04:05Z that day: #242 (packet d2967a9c, closed
#     `pr-opened`) still open on GitHub while #243 carried all of it and
#     more, and #239-#242 had all stood open at once — four PRs to read
#     where one said everything. So each OLDER open `publish/<date>` PR
#     from our fork is closed with a comment naming today's, AFTER
#     today's is open. Only our fork's heads, only `publish/` dates
#     sorting before today's: a newer PR, somebody else's branch, or a
#     non-publish PR from the fork is never ours to close.
#
#     Order, for a re-run: the older packet is annotated FIRST
#     (`pr_superseded`, the intent), then the PR closed, then GitHub
#     read back and ITS answer written as `pr_state` (the effect — the
#     same key and shape `--measure` writes, which the publish region
#     reads). Any failure stops the run before open-pr completes, so a
#     re-run reuses today's PR and meets whatever is still open. A PR no
#     packet recorded is closed all the same and said so by URL.
gh_t pr list --repo "$MIRROR_SLUG" --state open --limit 100 \
        --json number,url,headRefName,headRepositoryOwner > "$workdir/open-prs" 2>"$workdir/err" \
    || fail "the PR is open at $pr_url, but listing the mirror's open pull requests failed — gh said: $(head -c 300 "$workdir/err" | tr '\n' ' '); open-pr on ${job_id:0:8} stays ready and a re-run reuses the PR"
jq_doc_file "$workdir/open-prs" && jq -e 'type == "array"' "$workdir/open-prs" > /dev/null 2>&1 \
    || fail "the PR is open at $pr_url, but gh answered the open-PR listing with no list — nothing older was read, so nothing was closed; open-pr on ${job_id:0:8} stays ready"
jq -c --arg owner "$FORK_OWNER" --arg branch "$BRANCH" --arg url "$pr_url" '
    .[] | select(((.headRepositoryOwner.login // "") | ascii_downcase) == ($owner | ascii_downcase))
        | select((.headRefName // "") | startswith("publish/"))
        | select(.headRefName < $branch and .url != $url)
        | {number, url, head: .headRefName}' "$workdir/open-prs" > "$workdir/older" \
    || fail "the open-PR listing could not be read as pull requests"
: > "$workdir/superseded"
if [ -s "$workdir/older" ]; then
    curl -fsS -H "x-boss-user: $BOSS_USER" \
            "$BASE/api/jobs?kind=publish-to-github&limit=60" > "$workdir/published" 2>"$workdir/err" \
        || fail "jobs API unreachable at $BASE while recording superseded PRs — $(cat "$workdir/err"); nothing older was closed"
    while IFS= read -r row; do
        old_n=$(printf '%s' "$row" | jq -r '.number')
        old_url=$(printf '%s' "$row" | jq -r '.url')
        old_head=$(printf '%s' "$row" | jq -r '.head')
        old_job=$(jq -r --arg url "$old_url" 'first((if type == "object" and has("data") then .data else . end)
            | .[] | select(any((.steps // [])[]; .spec_slug == "open-pr" and (.metadata.pr_url // "") == $url))
            | .id) // empty' "$workdir/published")
        ts=$(date -u +%Y-%m-%dT%H:%M:%SZ)
        if [ -n "$old_job" ]; then
            jq -n --arg old "$old_url" --arg new "$pr_url" --arg job "$job_id" --arg ts "$ts" \
                '{pr_superseded: {pr_url: $old, by_pr_url: $new, by_packet: $job, at: $ts,
                                  by: "publish-github-pr"}}' > "$workdir/sup"
            curl -fsS -X PATCH -H "content-type: application/json" -H "x-boss-user: $BOSS_USER" \
                    ${BOSS_MACHINE_TOKEN:+-H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"} \
                    --data-binary @"$workdir/sup" \
                    "$BASE/api/jobs/$old_job/metadata" > /dev/null 2>"$workdir/err" \
                || fail "recording on ${old_job:0:8} that $pr_url supersedes $old_url failed — $(head -c 300 "$workdir/err" | tr '\n' ' '); $old_url was not closed"
        fi
        gh_t pr close "$old_n" --repo "$MIRROR_SLUG" \
                --comment "Superseded by $pr_url — today's snapshot of forge main, which carries everything in this one ($old_head) and every commit since. Closed by machine (BOSS publish-to-github, ops verb publish-github-pr, packet $job_id); the one PR to merge is the newest." \
                > /dev/null 2>"$workdir/err" \
            || fail "closing $old_url as superseded by $pr_url — gh said: $(head -c 300 "$workdir/err" | tr '\n' ' ')"
        # A close is a claim until GitHub is read saying so.
        gh_t api "repos/$MIRROR_SLUG/pulls/$old_n" > "$workdir/closed" 2>"$workdir/err" \
            || fail "asked GitHub to close $old_url, and it could not be read back — gh said: $(head -c 300 "$workdir/err" | tr '\n' ' ')"
        jq_doc_file "$workdir/closed" \
            || fail "asked GitHub to close $old_url, and the read-back answered nothing parseable"
        old_state=$(jq -r '.state // "no state"' "$workdir/closed")
        [ "$old_state" = "closed" ] \
            || fail "asked GitHub to close $old_url as superseded by $pr_url, and it still reads $old_state"
        if [ -n "$old_job" ]; then
            jq -c --arg url "$old_url" --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" '
                {pr_state: {pr_url: $url, number, state, merged: (.merged == true),
                            merged_at, closed_at, read_at: $ts,
                            read_by: "publish-github-pr (superseded)"}}' \
                "$workdir/closed" > "$workdir/pr-state"
            curl -fsS -X PATCH -H "content-type: application/json" -H "x-boss-user: $BOSS_USER" \
                    ${BOSS_MACHINE_TOKEN:+-H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"} \
                    --data-binary @"$workdir/pr-state" \
                    "$BASE/api/jobs/$old_job/metadata" > /dev/null 2>"$workdir/err" \
                || fail "$old_url is closed on GitHub, but writing its state onto ${old_job:0:8} failed — $(head -c 300 "$workdir/err" | tr '\n' ' ')"
            say "superseded $old_url ($old_head) — closed on GitHub, recorded on ${old_job:0:8}"
        else
            say "superseded $old_url ($old_head) — closed on GitHub; no publish packet recorded it, so the close comment is its only record"
        fi
        printf '%s\n' "$old_url" >> "$workdir/superseded"
    done < "$workdir/older"
fi

# 5. Complete open-pr with pr_url. Merge, never replace (the
#    ops-runner's rule): PUT swaps metadata wholesale.
printf '%s' "$target" | jq -c --arg url "$pr_url" --arg snap "$snapshot" \
        --arg fh "$forge_head" --arg mh "$mirror_head" --arg br "$FORK_OWNER:$BRANCH" \
        --rawfile sup "$workdir/superseded" '
    {status: "completed",
     metadata: ((.step.metadata // {})
                + {pr_url: $url, snapshot_commit: $snap, forge_head: $fh,
                   mirror_head: $mh, head: $br, published_by: "publish-github-pr",
                   superseded_prs: ($sup | split("\n") | map(select(. != "")))})}' \
    > "$workdir/payload"
if ! curl -fsS -X PUT -H "content-type: application/json" -H "x-boss-user: $BOSS_USER" \
        ${BOSS_MACHINE_TOKEN:+-H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"} \
        --data-binary @"$workdir/payload" \
        "$BASE/api/jobs/$job_id/steps/$step_id" > /dev/null 2>"$workdir/err"; then
    fail "the PR is open at $pr_url but completing open-pr on ${job_id:0:8} failed — $(head -c 300 "$workdir/err" | tr '\n' ' '); record pr_url on the step by hand"
fi

say "done — $pr_url (open-pr on ${job_id:0:8} completed; the merge is David's)"
