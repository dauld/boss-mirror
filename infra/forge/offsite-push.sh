#!/usr/bin/env bash
#
# offsite-push — push forge main and publish/* to the off-site copy on
# GitHub with a PLAIN push (no force, no --mirror, no prune), read it
# back, and only then remove Forgejo's own push mirror.
#
#   offsite-push.sh
#
# Run by forge-converge.sh on every tick, after protect-main.sh. What it
# pushes and where is declared in infra/forge/offsite-push.json.
#
# WHY IT EXISTS (backlog 21d54f4a, parent f9256445, design d812f1b7)
# ------------------------------------------------------------------
# On 2026-09-25 at 20:10:42Z forge main went back from c85941b4 (train
# #687's merge, one second old) to 777a5888, written by Forgejo itself
# (`Gitea <gitea@fake.local> update by push`). The writer was the forge's
# push mirror to github.com/dauld/boss-fork: Forgejo 16.0.2 adds a push
# mirror with `git remote add --mirror` (services/mirror/mirror_push.go),
# whose fetch refspec `+refs/*:refs/*` makes every sync write the values
# it just pushed back onto the forge's OWN refs/heads/* — so a sync that
# read main before the merge rewound main after it. Branch protection
# cannot stop it (protect-main.sh says why: the pre-receive hook never
# runs for it).
#
# A branch filter does not fix it. Triage a53e92a1 read v16.0.2's source
# and reproduced both halves: a mirror created WITH a filter is added
# without --mirror, so the write-back goes; but every sync is still
# `git push -f --mirror` (modules/git/repo.go Push), which force-pushes
# and PRUNES every branch on the target that the forge lacks, filter or
# no filter. David, 2026-09-26: "I just want to make sure we can't
# accidentally wipe ourselves" — no mirror may wipe what it mirrors. So
# the decided fix (packet 21d54f4a, decided_2026-09-26) is to REMOVE the
# Forgejo push mirror and replace it with this: our own push, declared
# in the tree, that can neither overwrite nor delete anything off-site.
#
# WHAT THE PUSH IS
# ----------------
# `git push <remote> refs/heads/<b>:refs/heads/<b>...` for each declared
# branch — no leading `+`, no --force, no --mirror, no --prune. So:
#   * a fast-forward lands; a branch new to the target is created;
#   * a NON-fast-forward (forge main rewound, or a target that moved on
#     its own) is REFUSED by git, named here, recorded on the converge's
#     packet, and exit 1 — the target keeps what it had, never
#     overwritten. The next tick asks again; a person decides;
#   * a branch the target holds and the forge does not is left alone.
# It pushes from a PRIVATE bare clone ($STATE_DIR/boss.git, root's) that
# FETCHES the declared branches from the forge's repository by path.
# The forge repository is only ever read — nothing here adds a remote to
# it or runs a push inside it, which is the shape that rewound main — and
# root never writes into a repository the Forgejo account owns
# (publish-github-pr.sh carries the reasoning; the path is derived by
# forge-repo-path.sh, the one definition both share).
#
# WHAT READS IT: dauld/boss-mirror (canonical name; GitHub 301-redirects
# boss-fork there) is the disaster-recovery copy of main and the fork
# publish-github-pr.sh opens its PRs from (docs/design/internal-forge.md
# Q4; measured_2026-09-26_mirror_consumers on the packet). publish/<date>
# is pushed to the forge FIRST by that verb (ce5339d6) and still reaches
# the fork from there, by this push rather than the mirror.
#
# A SAME-DAY RE-PUBLISH IS THE ONE EXPECTED REFUSAL. The verb force-moves
# publish/<date> on the forge to a new snapshot (not a descendant of the
# old one) and then force-pushes it straight to the fork itself. A tick
# that lands in the seconds between those two pushes finds publish/<date>
# non-fast-forward and refuses it — correctly: the fork holds a snapshot
# the forge no longer does. The verb's own fork push settles it, and the
# next tick reads it up to date.
#
# ORDER: PUSH, READ BACK, THEN REMOVE THE MIRROR
# ----------------------------------------------
# The Forgejo push mirror is deleted (DELETE /repos/{repo}/push_mirrors/
# {name}, every one listed — any Forgejo push mirror is `-f --mirror`)
# only after this run's push has succeeded and every pushed ref reads
# back off the target at the forge's value, so there is never a tick with
# no off-site copy. A push that fails or is refused removes nothing. The
# list is read back after the delete: an answer is not an effect.
#
# What the API delete removes, read off v16.0.2 (d7471ea4,
# routers/api/v1/repo/mirror.go DeletePushMirrorByRemoteName): the push
# mirror's DATABASE ROW only. Every sync starts from that row
# (services/mirror/mirror_push.go SyncPushMirror returns at "!exist"), so
# with the row gone nothing pushes and nothing writes back — the writer
# is removed. The web UI's delete also runs `git remote rm` in the
# repository (routers/web/repo/setting/setting.go, push-mirror-remove);
# the API's does not, so the old remote's config section stays in the
# forge repository, inert: no row, no sync, no push through it. It is not
# removed from here, because that is a write into the Forgejo account's
# repository, which this script never makes.
#
# CREDENTIALS — both read from files, neither ever on an argv or printed
#   GitHub: dauld's token at $BOSS_GITHUB_TOKEN_FILE (default
#     /etc/boss-publish/github.token, registry id dauld-github-token,
#     root 0600 — the one publish-github-pr.sh reads, placed by David's
#     token admin). It reaches git through a credential helper that reads
#     the file when git asks. Nothing here mints or places a credential.
#   Forgejo: the header file forge-converge.sh fills from the checkout's
#     own forge credential ($BOSS_FORGE_AUTH_HEADER_FILE), as for
#     protect-main.sh. Whether it may delete a push mirror is MEASURED by
#     the first delete — a 401/403 is named on the packet.
#
# EXIT
#   0  every declared branch is on the target at the forge's value (pushed
#      or already so), read back, and the forge carries no push mirror
#   1  the push was refused (non-fast-forward, named) or failed, the
#      pushed refs do not read back, or a mirror delete was refused or did
#      not take — stderr says which, and what was left as it was
#   2  the declaration is unreadable, or names a branch that could force,
#      rename or match more than a branch pattern should
#   4  cannot answer: a credential missing or loose, the forge repository
#      unreadable, or the forge API unreachable. Before the push, nothing
#      was written; after it (the API half), the push stands and no mirror
#      was removed
#
# ENV
#   BOSS_OFFSITE_PUSH_DECL       the declaration (default beside this file)
#   BOSS_GITHUB_TOKEN_FILE       dauld's token file
#   BOSS_OFFSITE_STATE_DIR       the private clone's home (/var/lib/boss-offsite)
#   BOSS_FORGE_REPO_PATH …       see forge-repo-path.sh
#   BOSS_FORGE_URL               Forgejo's base (from /etc/boss/sor.env)
#   BOSS_FORGE_AUTH_HEADER_FILE  the forge header file (required)
#   BOSS_OFFSITE_CURL            the curl command — the test seam
#   BOSS_RUN_SUMMARY_FILE        offsite_push lands on the packet
# Tested in crates/core/boss-testing/tests/offsite_push_sh.rs against real
# git repositories and a stub curl.

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/run-summary.sh
. "$HERE/../run-summary.sh"
# shellcheck source=infra/lib/sor.sh
. "$HERE/../lib/sor.sh"
# shellcheck source=infra/forge/forge-repo-path.sh
. "$HERE/forge-repo-path.sh"

ME="offsite-push"
DECL="${BOSS_OFFSITE_PUSH_DECL:-$HERE/offsite-push.json}"
TOKEN_FILE="${BOSS_GITHUB_TOKEN_FILE:-/etc/boss-publish/github.token}"
STATE_DIR="${BOSS_OFFSITE_STATE_DIR:-/var/lib/boss-offsite}"
CURL="${BOSS_OFFSITE_CURL:-curl}"
CLONE="$STATE_DIR/boss.git"

say() { echo "$ME: $*" >&2; }
verdict() { run_summary_field offsite_push "$1"; }
cannot_answer() {
    say "CANNOT ANSWER — $*"
    verdict "cannot answer: $*"
    exit 4
}
failed() {
    say "FAILED — $*"
    verdict "FAILED: $*"
    exit 1
}

# --- the declaration -------------------------------------------------------
# Each branch is a plain name or a name ending in ONE `/*` — the only
# wildcard a refspec carries both sides of. No `+` (force), no `:`
# (rename or delete), nothing git would read as a second pattern.
if ! jq_doc_file "$DECL" \
    || ! jq -e '
        type == "object"
        and (.remote | type == "string" and length > 0)
        and (.branches | type == "array" and length > 0
             and all(type == "string"
                     and test("^[A-Za-z0-9._-]+(/[A-Za-z0-9._-]+)*(/\\*)?$")))' \
        "$DECL" >/dev/null 2>&1; then
    say "REFUSED — $DECL is not an object naming a remote and a non-empty list of plain branch names (a name, or a name ending in /*)"
    verdict "refused: the declaration $DECL is unreadable or names a branch it may not"
    exit 2
fi
REMOTE="$(jq -r '.remote' "$DECL")"
mapfile -t BRANCHES < <(jq -r '.branches[]' "$DECL")
fetch_specs=()
push_specs=()
for b in "${BRANCHES[@]}"; do
    fetch_specs+=("+refs/heads/$b:refs/heads/$b")
    push_specs+=("refs/heads/$b:refs/heads/$b")
done

# --- every precondition, before anything is written -------------------------
if [ ! -e "$TOKEN_FILE" ]; then
    cannot_answer "no GitHub token at $TOKEN_FILE (registry id dauld-github-token; David's token admin places it, root 0600, one line)"
elif [ ! -r "$TOKEN_FILE" ] || [ ! -s "$TOKEN_FILE" ]; then
    cannot_answer "the GitHub token file $TOKEN_FILE is unreadable or empty"
fi
mode="$(stat -c %a "$TOKEN_FILE" 2>/dev/null || echo '?')"
case "$mode" in
    600|400) ;;
    *) cannot_answer "the GitHub token file $TOKEN_FILE is mode $mode — a token file must be 0600 or 0400" ;;
esac
AUTH="${BOSS_FORGE_AUTH_HEADER_FILE:-}"
if [ -z "$AUTH" ] || [ ! -s "$AUTH" ]; then
    cannot_answer "no forge credential: BOSS_FORGE_AUTH_HEADER_FILE names no non-empty header file"
fi
sor_require BOSS_FORGE_URL
API="${BOSS_FORGE_URL%/}/api/v1/repos/$FORGE_REPO_SLUG/push_mirrors"
if [ ! -d "$FORGE_REPO" ] || [ ! -r "$FORGE_REPO" ]; then
    cannot_answer "the forge repository $FORGE_REPO is not a readable directory (path came from: $FORGE_REPO_FROM)"
fi

WORK="$(mktemp -d -t offsite-push.XXXXXX)" || cannot_answer "mktemp failed"
trap 'rm -rf "$WORK"' EXIT

# The forge repository belongs to the Forgejo account and this runs as
# root, so git needs it exempted from the ownership check — through a
# config FILE, because a local fetch runs upload-pack inside the source
# and git clears `-c` crossing into it (publish-github-pr.sh measured
# this, 2026-09-11). The file is per-run, in a 0700 dir the trap removes.
printf '[safe]\n\tdirectory = %s\n\tdirectory = %s\n' "$FORGE_REPO" "$CLONE" >"$WORK/safe.gitconfig"
export GIT_CONFIG_GLOBAL="$WORK/safe.gitconfig"
export GIT_TERMINAL_PROMPT=0
g() { git -C "$CLONE" "$@"; }

# --- read the forge ----------------------------------------------------------
if [ ! -d "$CLONE" ]; then
    mkdir -p "$STATE_DIR" && chmod 700 "$STATE_DIR" \
        && git init -q --bare "$CLONE" 2>"$WORK/err" \
        || cannot_answer "no private clone at $CLONE: $(tr '\n' ' ' <"$WORK/err")"
fi
# Forced and pruned HERE only: the private clone is a copy of what the
# forge holds now, so a rewound forge main is a rewound local main — and
# the push below, which is neither, refuses it off-site.
g fetch -q --prune "$FORGE_REPO" "${fetch_specs[@]}" 2>"$WORK/err" \
    || cannot_answer "fetching ${BRANCHES[*]} from $FORGE_REPO: $(head -c 300 "$WORK/err" | tr '\n' ' ')"
g for-each-ref --format='%(objectname) %(refname)' refs/heads/ >"$WORK/local"
[ -s "$WORK/local" ] || cannot_answer "the forge holds none of ${BRANCHES[*]}"

# --- push: plain, so git itself refuses what is not a fast-forward -----------
# The helper reads the token file when git asks for a credential; the
# argv carries the file's path, never its contents.
helper="!f() { test \"\$1\" = get || exit 0; echo username=x-access-token; echo \"password=\$(cat '$TOKEN_FILE')\"; }; f"
g -c credential.helper= -c "credential.helper=$helper" push --porcelain "$REMOTE" "${push_specs[@]}" \
    >"$WORK/push.out" 2>"$WORK/push.err"
push_rc=$?
# Porcelain: `<flag>\t<from>:<to>\t<summary>`; `!` is a rejection.
rejected="$(awk -F'\t' '$1 == "!" { sub(/^[^:]*:/, "", $2); printf "%s %s; ", $2, $3 }' "$WORK/push.out")"
if [ -n "$rejected" ]; then
    say "REFUSED — the target would not take: ${rejected%; }"
    say "nothing was overwritten: $REMOTE keeps what it had, and the Forgejo push mirror was left as it is."
    say "a non-fast-forward means the forge and the off-site copy disagree about history — read both before anything moves."
    verdict "REFUSED: ${rejected%; }"
    exit 1
fi
if [ "$push_rc" -ne 0 ]; then
    failed "pushing ${BRANCHES[*]} to $REMOTE: git exit $push_rc: $(head -c 300 "$WORK/push.err" | tr '\n' ' ') — nothing was removed; the Forgejo push mirror is left as it is"
fi
updated="$(awk -F'\t' '$1 == " " || $1 == "*" { sub(/:.*/, "", $2); printf "%s ", $2 }' "$WORK/push.out")"

# --- read back: an answer is not an effect -----------------------------------
g -c credential.helper= -c "credential.helper=$helper" ls-remote --heads "$REMOTE" \
    >"$WORK/remote" 2>"$WORK/err" \
    || failed "pushed, but reading $REMOTE back failed: $(head -c 300 "$WORK/err" | tr '\n' ' ') — the Forgejo push mirror is left as it is"
missing=""
while read -r sha ref; do
    grep -qxF "$(printf '%s\t%s' "$sha" "$ref")" "$WORK/remote" || missing="$missing $ref"
done <"$WORK/local"
[ -z "$missing" ] || failed "pushed, but$missing do not read back on $REMOTE at the forge's value — the Forgejo push mirror is left as it is"
n="$(wc -l <"$WORK/local" | tr -d ' ')"
if [ -n "$updated" ]; then
    pushed="pushed ${updated% } to $REMOTE ($n ref(s) read back at the forge's value)"
else
    pushed="$REMOTE up to date ($n ref(s) read back at the forge's value)"
fi
echo "$ME: $pushed"

# --- the Forgejo push mirror, removed once the copy above stands -------------
# call <METHOD> <url> — sets CODE, body in $WORK/out.
call() {
    local rc=0
    : >"$WORK/out"
    CODE="$("$CURL" -sS -m 20 -o "$WORK/out" -w '%{http_code}' -H "@$AUTH" -X "$1" "$2" 2>"$WORK/err")" || rc=$?
    if [ "$rc" -ne 0 ]; then
        CURL_WORDS="curl exit $rc: $(tr '\n' ' ' <"$WORK/err")"
        return 1
    fi
}
forge_words() { jq -r '.message // empty' "$WORK/out" 2>/dev/null | tr '\n' ' ' | cut -c1-200; }
# list_mirrors — the push mirrors' remote_names, one per line, or exit 4.
list_mirrors() {
    call GET "$API" || cannot_answer "$pushed; but listing the forge's push mirrors failed: $CURL_WORDS — none was removed"
    [ "$CODE" = 200 ] || cannot_answer "$pushed; but listing the forge's push mirrors answered HTTP $CODE ($(forge_words)) — none was removed"
    # jq_doc_file first: an empty 200 body must not read as "no mirrors".
    jq_doc_file "$WORK/out" && jq -e 'type == "array"' "$WORK/out" >/dev/null 2>&1 \
        || cannot_answer "$pushed; but the forge answered its push-mirror list with no list — none was removed"
    jq -r '.[].remote_name // empty' "$WORK/out"
}

mirrors="$(list_mirrors)" || exit $?
if [ -z "$mirrors" ]; then
    echo "$ME: the forge carries no push mirror"
    verdict "$pushed; no Forgejo push mirror"
    exit 0
fi
while read -r name; do
    case "$name" in
        ''|*[!A-Za-z0-9._-]*) failed "$pushed; but the forge lists a push mirror named '$name', which this will not put in a URL" ;;
    esac
    call DELETE "$API/$name" || failed "$pushed; but deleting push mirror $name failed: $CURL_WORDS"
    case "$CODE" in
        200|204) ;;
        *) failed "$pushed; but deleting push mirror $name answered HTTP $CODE ($(forge_words)) — a 401/403 means the converge's forge credential cannot administer the repository" ;;
    esac
done <<<"$mirrors"
left="$(list_mirrors)" || exit $?
[ -z "$left" ] || failed "$pushed; deleted push mirror(s) $(echo "$mirrors" | paste -sd, -), but the forge still lists $(echo "$left" | paste -sd, -) — does not read back"
removed="removed Forgejo push mirror(s) $(echo "$mirrors" | paste -sd, -) — read back: none left"
echo "$ME: $removed"
verdict "$pushed; $removed"
exit 0
