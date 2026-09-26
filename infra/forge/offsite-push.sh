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
# only after main reads back off the target at the forge's value, so
# there is never a tick with no off-site copy of main. A push that fails,
# or refuses main, removes nothing. A refusal on any OTHER declared ref
# (a publish/<date> mid re-publish) is named and exit 1, but does not keep
# the mirror — main is what the copy exists for (backlog b176fd60 S1).
# The list is read back after the delete: an answer is not an effect.
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
#     token admin). The file must be owned by root as well as 0600/0400.
#     It reaches git through a credential helper that reads the file when
#     git asks, and answers only https://github.com. Nothing here mints
#     or places a credential.
#   Forgejo: the header file forge-converge.sh fills from the checkout's
#     own forge credential ($BOSS_FORGE_AUTH_HEADER_FILE), as for
#     protect-main.sh. Whether it may delete a push mirror is MEASURED by
#     the first delete — a 401/403 is named on the packet.
#
# EXIT
#   0  every declared branch is on the target at the forge's value (pushed
#      or already so), read back, and the forge carries no push mirror
#   1  the push was refused (non-fast-forward, named) or failed, a pushed
#      ref does not read back, the forge lists a mirror with no name, or a
#      mirror delete was refused or did not take — stderr says which, and
#      what was left as it was. (With main read back, the mirror is still
#      removed; the exit is 1 for the other ref.)
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
#   BOSS_OFFSITE_TOKEN_OWNER_UID the uid that must own it (0 — the seam
#                                the tests use, as they do not run as root)
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
TOKEN_OWNER_UID="${BOSS_OFFSITE_TOKEN_OWNER_UID:-0}"
CLONE="$STATE_DIR/boss.git"
# A stall is a failure this script names, in seconds, rather than a hang
# the unit's ten-minute timeout kills before offsite_push is written
# (b176fd60 S4): below LOW_SPEED_LIMIT bytes/s for LOW_SPEED_TIME
# seconds, git gives up with its own words.
LOW_SPEED_LIMIT=1000
LOW_SPEED_TIME=60

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
# The mode says who may READ it; the owner says who could have WRITTEN
# it. A file another account owns can be swapped for a token of that
# account's choosing, and root would push with it (backlog b176fd60 S5).
owner="$(stat -c %u "$TOKEN_FILE" 2>/dev/null || echo '?')"
[ "$owner" = "$TOKEN_OWNER_UID" ] \
    || cannot_answer "the GitHub token file $TOKEN_FILE is owned by uid $owner — it must be owned by uid $TOKEN_OWNER_UID (root)"
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
# And no system config: a root unit reads /etc/gitconfig, where a
# `pushInsteadOf` would send this push somewhere the declaration does not
# name and a `core.hooksPath` would run a hook as root. The tests always
# set this; the script did not (b176fd60 S3).
export GIT_CONFIG_NOSYSTEM=1
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
# argv carries the file's path, never its contents. It answers ONLY
# https://github.com — git hands it the protocol and host on stdin — so
# a redirect, an insteadOf or a changed declaration can never carry
# dauld's token to another host (b176fd60 S5).
helper="!f() { test \"\$1\" = get || exit 0; p=; h=; while IFS= read -r l && [ -n \"\$l\" ]; do case \"\$l\" in protocol=*) p=\${l#protocol=} ;; host=*) h=\${l#host=} ;; esac; done; test \"\$p\" = https && test \"\$h\" = github.com || exit 0; echo username=x-access-token; echo \"password=\$(cat '$TOKEN_FILE')\"; }; f"
net=(-c credential.helper= -c "credential.helper=$helper"
     -c "http.lowSpeedLimit=$LOW_SPEED_LIMIT" -c "http.lowSpeedTime=$LOW_SPEED_TIME")
g "${net[@]}" push --porcelain "$REMOTE" "${push_specs[@]}" \
    >"$WORK/push.out" 2>"$WORK/push.err"
push_rc=$?
# Porcelain: `<flag>\t<from>:<to>\t<summary>`; `!` is a rejection.
rejected="$(awk -F'\t' '$1 == "!" { sub(/^[^:]*:/, "", $2); printf "%s %s; ", $2, $3 }' "$WORK/push.out")"
rejected_refs="$(awk -F'\t' '$1 == "!" { sub(/^[^:]*:/, "", $2); print $2 }' "$WORK/push.out")"
if [ -z "$rejected" ] && [ "$push_rc" -ne 0 ]; then
    failed "pushing ${BRANCHES[*]} to $REMOTE: git exit $push_rc (a transfer below $LOW_SPEED_LIMIT B/s for ${LOW_SPEED_TIME}s is given up here): $(head -c 300 "$WORK/push.err" | tr '\n' ' ') — nothing was removed; the Forgejo push mirror is left as it is"
fi
updated="$(awk -F'\t' '$1 == " " || $1 == "*" { sub(/:.*/, "", $2); printf "%s ", $2 }' "$WORK/push.out")"

# --- read back: an answer is not an effect -----------------------------------
# Read back even when a ref was refused: git updates the refs it accepts,
# and whether MAIN stands is what decides the mirror below.
g "${net[@]}" ls-remote --heads "$REMOTE" \
    >"$WORK/remote" 2>"$WORK/err" \
    || failed "reading $REMOTE back failed: $(head -c 300 "$WORK/err" | tr '\n' ' ') — the Forgejo push mirror is left as it is${rejected:+; and the target refused: ${rejected%; }}"
missing=""
main_back=""
read_back=0
while read -r sha ref; do
    if grep -qxF "$(printf '%s\t%s' "$sha" "$ref")" "$WORK/remote"; then
        read_back=$((read_back + 1))
        [ "$ref" = refs/heads/main ] && main_back=yes
    elif ! grep -qxF "$ref" <<<"$rejected_refs"; then
        missing="$missing $ref"
    fi
done <"$WORK/local"

# --- main decides the mirror (b176fd60 S1) -----------------------------------
# The Forgejo mirror is the writer that rewound main, and main is what the
# off-site copy exists to keep, so main standing off-site at the forge's
# value is what retires it. A refusal on any other declared ref — the
# expected one is a same-day re-publish of publish/<date> — is still named
# and still exit 1, but it no longer keeps the mirror alive every tick
# until a person steps in. Main refused, or not read back: nothing is
# removed.
if [ -z "$main_back" ]; then
    if [ -n "$rejected" ]; then
        say "REFUSED — the target would not take: ${rejected%; }"
        say "nothing was overwritten: $REMOTE keeps what it had, and the Forgejo push mirror was left as it is."
        say "a non-fast-forward means the forge and the off-site copy disagree about history — read both before anything moves."
        verdict "REFUSED: ${rejected%; }${missing:+; and$missing do not read back}"
        exit 1
    fi
    failed "pushed, but${missing:- refs/heads/main (the forge holds no main)} do not read back on $REMOTE at the forge's value — the Forgejo push mirror is left as it is"
fi
refusal=""
if [ -n "$rejected" ]; then
    say "REFUSED — the target would not take: ${rejected%; }"
    say "nothing of it was overwritten; main reads back at the forge's value, so the Forgejo push mirror is still removed."
    refusal="REFUSED: ${rejected%; }"
fi
if [ -n "$missing" ]; then
    say "FAILED —$missing do not read back on $REMOTE at the forge's value"
    refusal="${refusal:+$refusal; }FAILED:$missing do not read back"
fi
if [ -n "$updated" ]; then
    pushed="pushed ${updated% } to $REMOTE ($read_back ref(s) read back at the forge's value)"
else
    pushed="$REMOTE up to date ($read_back ref(s) read back at the forge's value)"
fi
echo "$ME: $pushed"
pushed="${refusal:+$refusal; }$pushed"

# finish <words> — the mirror half's outcome; exit 1 if a ref above was
# refused or did not read back, else 0.
finish() {
    echo "$ME: $1"
    verdict "$pushed; $1"
    [ -z "$refusal" ] || exit 1
    exit 0
}

# --- the Forgejo push mirror, removed once main above stands -----------------
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
# list_mirrors — the push mirrors' remote_names, one per line, or exit.
list_mirrors() {
    call GET "$API" || cannot_answer "$pushed; but listing the forge's push mirrors failed: $CURL_WORDS — none was removed"
    [ "$CODE" = 200 ] || cannot_answer "$pushed; but listing the forge's push mirrors answered HTTP $CODE ($(forge_words)) — none was removed"
    # jq_doc_file first: an empty 200 body must not read as "no mirrors".
    jq_doc_file "$WORK/out" && jq -e 'type == "array"' "$WORK/out" >/dev/null 2>&1 \
        || cannot_answer "$pushed; but the forge answered its push-mirror list with no list — none was removed"
    # A row with no remote_name is a mirror that is still THERE, and one
    # this cannot delete by name. `// empty` used to drop it and report
    # "no Forgejo push mirror" (b176fd60 S5).
    jq -e 'all(.[]; (.remote_name | type) == "string" and (.remote_name | length) > 0)' "$WORK/out" >/dev/null 2>&1 \
        || failed "$pushed; but the forge lists a push mirror with no remote_name (to $(jq -r '[.[] | select((.remote_name | type) != "string" or (.remote_name | length) == 0) | .remote_address // "?"] | join(", ")' "$WORK/out")) — it cannot be deleted by name and may still be pushing; none was removed"
    jq -r '.[].remote_name' "$WORK/out"
}

mirrors="$(list_mirrors)" || exit $?
if [ -z "$mirrors" ]; then
    finish "the forge carries no push mirror"
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
finish "removed Forgejo push mirror(s) $(echo "$mirrors" | paste -sd, -) — read back: none left"
