#!/usr/bin/env bash
#
# protect-main — make forge main's branch protection what the tree
# declares (infra/forge/main-protection.json), then read it back.
#
#   protect-main.sh
#
# Run by forge-converge.sh on every tick. Idempotent: a rule already as
# declared is read and not written.
#
# WHY IT EXISTS (backlog f9256445, car 4 of design d812f1b7)
# ----------------------------------------------------------
# On 2026-09-25 at 20:10:41Z train #687 merged into forge main as
# c85941b4, and one second later main was back at 777a5888. The forge
# answered `branch_protections = []`: nothing on it refused a direct or a
# force push to the trunk, from any clone holding the forge credential.
# David answered Q2 of the design "yes": direct push off and force push
# off, so only a pull-request merge (the train conductor's) moves main;
# declared in the tree and applied by the forge converge, never set by
# hand in the Forgejo UI — a hand-set rule is one nobody can read back
# from the tree, and one a UI click undoes without a record.
#
# WHAT THIS DOES NOT STOP — measured, and stated plainly
# ------------------------------------------------------
# The rewind of 2026-09-25 was NOT a push. The reflog of refs/heads/main
# (forge-log, ops-request 795974c7) reads
#   20:10:41Z 777a5888..c85941b4  david (the conductor's merge)  push
#   20:10:42Z c85941b4..777a5888  Gitea <gitea@fake.local>       update by push
# `push` is receive-pack's reflog message. `update by push` is NOT: it is
# the message git writes into the repository that RUNS `git push`, when
# it moves that repository's own remote-tracking ref to the value it just
# pushed (transport.c). Forgejo v16.0.2 — the forge's image,
# codeberg.org/forgejo/forgejo:16.0.2, in a container created 2026-08-10
# and not recreated since — adds a push mirror with `git remote add
# --mirror <name> <url>` (services/mirror/mirror_push.go), which writes
# the FETCH refspec `+refs/*:refs/*`; a mirror push then maps every ref
# it pushed back onto the repo's OWN refs/heads/*. So a push-mirror sync
# (the repo's one mirror, sync_on_commit, set off by the push to another
# branch at 20:10:39Z) that read main at 777a5888 before the merge landed
# wrote main back to 777a5888 when it finished, as Forgejo's own git
# identity. Reproduced with plain git in exactly that shape on
# 2026-09-25 (the car's park record carries the rehearsal): main rewound,
# reflog `Gitea <gitea@fake.local> update by push`, and the source repo's
# pre-receive hook was never called.
#
# Branch protection lives in exactly that hook
# (routers/private/hook_pre_receive.go), which only receive-pack runs. So
# this rule CANNOT stop the writer that rewound main on 2026-09-25. It
# closes the human and clone paths David approved — a direct push, a
# force push, a deletion, `boss publish main` — and the conductor's
# ancestry arm (car 3 of the same design: a merged train whose merge_ref
# main no longer carries ends on `merge-lost`) is the ONLY guard against
# the internal writer. Removing the writer is the push mirror's own fix,
# not this file's: infra/forge/offsite-push.sh (backlog 21d54f4a) replaces
# the Forgejo push mirror with a plain push and deletes the mirror.
#
# WHY THE CONDUCTOR'S MERGE STILL PASSES
# --------------------------------------
# A PR merge reaches the same hook carrying its PullRequestID. With
# enable_push false the doer cannot push directly (step 5), so the hook
# takes step 6b: IsUserAllowedToMerge (no merge allowlist, so write
# access on the code unit decides — the conductor's account merges
# today), then CheckPullBranchProtections (approvals, status checks,
# review and outdated-branch blocks). The declaration turns every one of
# those OFF by name, so a key the forge defaults otherwise cannot close
# the path; protect_main_sh.rs pins that. Force push and deletion carry
# no key: v16.0.2 refuses both on ANY protected branch before it reads
# the rule's settings (steps 1 and 2), and a squash merge is not a force
# push — its parent is main's head. Rehearsed against a real Forgejo
# 16.0.2 on 2026-09-25: direct push refused, force push refused, the
# owner's API squash merge allowed.
#
# THE CREDENTIAL
# --------------
# The caller hands a header file (`Authorization: token …`) through
# BOSS_FORGE_AUTH_HEADER_FILE; curl reads it with `-H @file`, so the
# token is never on an argv (ps), a log line or this script's output.
# forge-converge.sh fills it from the checkout's own forge credential —
# one credential, the pattern cluster-deploy-lib.sh uses for tenant
# repos: the first run MEASURES whether that credential may administer
# the repository, and one that may not is named on the packet (HTTP 403)
# rather than guessed at. Nothing here mints or places a credential.
#
# EXIT
#   0  main's rule is as declared, read back key by key (created, edited
#      or already so — the first line says which)
#   1  a write was refused, or answered and does not read back as
#      declared: protection is NOT in force as declared, and stderr says so
#   2  the declaration is unreadable or not an object with rule_name
#   4  cannot answer: no credential, curl failed, or the forge answered a
#      read with something other than 200/404 — and NOTHING was written,
#      because a write on a guess is the wrong-target class
#
# ENV
#   BOSS_FORGE_URL               Forgejo's base (from /etc/boss/sor.env)
#   BOSS_FORGE_AUTH_HEADER_FILE  the header file above (required)
#   BOSS_FORGE_PROTECT_REPO      owner/name, default $FORGE_OWNER/boss
#   BOSS_FORGE_PROTECTION        the declaration, default beside this file
#   BOSS_FORGE_PROTECT_CURL      the curl command — the test seam
#   BOSS_RUN_SUMMARY_FILE        main_protection lands on the packet
# Tested against a stub curl in crates/core/boss-testing/tests/protect_main_sh.rs.

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/forge/forge-defaults.sh
. "$HERE/forge-defaults.sh"
# shellcheck source=infra/run-summary.sh
. "$HERE/../run-summary.sh"

ME="protect-main"
CURL="${BOSS_FORGE_PROTECT_CURL:-curl}"
DECL="${BOSS_FORGE_PROTECTION:-$HERE/main-protection.json}"
REPO="${BOSS_FORGE_PROTECT_REPO:-$FORGE_OWNER/boss}"

say() { echo "$ME: $*" >&2; }
verdict() { # <summary-verdict>
    run_summary_field main_protection "$1"
}
cannot_answer() {
    say "CANNOT ANSWER — $*"
    say "nothing was written; main's protection is whatever the forge already holds."
    verdict "cannot answer: $*"
    exit 4
}
not_in_force() {
    say "FAILED — $*"
    say "main's protection is NOT in force as infra/forge/main-protection.json declares it."
    verdict "FAILED: $*"
    exit 1
}

# --- the declaration -------------------------------------------------------
# jq_doc_file first (infra/lib/jq.sh, sourced by run-summary.sh): on
# jq-1.6 an empty file passes `jq -e` (backlog d96e38ab).
if ! jq_doc_file "$DECL" \
    || ! jq -e 'type == "object" and (.rule_name | type == "string" and length > 0)' "$DECL" >/dev/null 2>&1; then
    say "REFUSED — $DECL is not a JSON object naming rule_name"
    verdict "refused: the declaration $DECL is unreadable"
    exit 2
fi
RULE="$(jq -r '.rule_name' "$DECL")"

# --- the credential, before any call ---------------------------------------
AUTH="${BOSS_FORGE_AUTH_HEADER_FILE:-}"
if [ -z "$AUTH" ] || [ ! -s "$AUTH" ]; then
    cannot_answer "no credential: BOSS_FORGE_AUTH_HEADER_FILE names no non-empty header file"
fi
sor_require BOSS_FORGE_URL
API="${BOSS_FORGE_URL%/}/api/v1/repos/$REPO/branch_protections"

WORK="$(mktemp -d -t protect-main.XXXXXX)" || cannot_answer "mktemp failed"
trap 'rm -rf "$WORK"' EXIT

# call <METHOD> <url> [body-file] — sets CODE and leaves the body in
# $WORK/out. A curl failure prints curl's own words and returns non-zero.
call() {
    local method="$1" url="$2" body="${3:-}" rc=0
    local args=(-sS -m 20 -o "$WORK/out" -w '%{http_code}' -H "@$AUTH" -H 'Content-Type: application/json' -X "$method")
    [ -n "$body" ] && args+=(--data-binary "@$body")
    : >"$WORK/out"
    CODE="$("$CURL" "${args[@]}" "$url" 2>"$WORK/err")" || rc=$?
    if [ "$rc" -ne 0 ]; then
        CURL_WORDS="curl exit $rc: $(tr '\n' ' ' <"$WORK/err")"
        return 1
    fi
    return 0
}
# The forge's own words for a refusal, one line, bounded.
forge_words() { jq -r '.message // empty' "$WORK/out" 2>/dev/null | tr '\n' ' ' | cut -c1-200; }

# drift <live-file> — the declared keys whose live value differs, one per
# line. An absent array reads as [], an absent string as "" (the API
# omits empty lists on some versions); arrays compare as sets.
drift() {
    jq -r --slurpfile d "$DECL" '
        . as $live
        | $d[0] | to_entries[]
        | .key as $k | .value as $want
        | ($live[$k]) as $have
        | (if ($want | type) == "array" then (($have // []) | sort) == ($want | sort)
           elif ($want | type) == "string" then ($have // "") == $want
           else $have == $want end) as $same
        | select($same | not) | $k' "$1"
}

# --- read ---------------------------------------------------------------------
call GET "$API/$RULE" || cannot_answer "reading $API/$RULE: $CURL_WORDS"
case "$CODE" in
    200) cp "$WORK/out" "$WORK/live.json"; action=check ;;
    404) action=create ;;
    *)   cannot_answer "reading $API/$RULE answered HTTP $CODE ($(forge_words))" ;;
esac

# --- write, only what differs -------------------------------------------------
if [ "$action" = create ]; then
    call POST "$API" "$DECL" || cannot_answer "creating the rule: $CURL_WORDS"
    case "$CODE" in
        200|201) did="created rule '$RULE' on $REPO" ;;
        *) not_in_force "creating rule '$RULE' on $REPO answered HTTP $CODE ($(forge_words)) — a 401/403 means the converge's forge credential cannot administer the repository" ;;
    esac
else
    keys="$(drift "$WORK/live.json" | paste -sd, -)"
    if [ -z "$keys" ]; then
        echo "$ME: main on $REPO already as declared (rule '$RULE', $(jq 'length' "$DECL") keys read back as declared)"
        verdict "already as declared"
        exit 0
    fi
    # rule_name names the rule in the URL; an edit carries the settings.
    jq 'del(.rule_name)' "$DECL" >"$WORK/edit.json"
    call PATCH "$API/$RULE" "$WORK/edit.json" || cannot_answer "editing the rule: $CURL_WORDS"
    case "$CODE" in
        200) did="edited rule '$RULE' on $REPO (drifted: $keys)" ;;
        *) not_in_force "editing rule '$RULE' on $REPO (drifted: $keys) answered HTTP $CODE ($(forge_words)) — a 401/403 means the converge's forge credential cannot administer the repository" ;;
    esac
fi

# --- read back: an answer is not an effect -------------------------------------
call GET "$API/$RULE" || not_in_force "$did, but the read-back failed: $CURL_WORDS"
[ "$CODE" = 200 ] || not_in_force "$did, but the rule does not read back: HTTP $CODE ($(forge_words))"
cp "$WORK/out" "$WORK/live.json"
left="$(drift "$WORK/live.json" | paste -sd, -)"
[ -z "$left" ] || not_in_force "$did, but the rule does not read back as declared: $left"

echo "$ME: $did — read back as declared"
verdict "$did — read back as declared"
exit 0
