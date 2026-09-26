#!/usr/bin/env bash
#
# credential-deposit — the forge host takes a broker-filled credential
# out of its k8s Secret and into the FILE its consumers read, proves it
# by effect, and records delivery so the broker may revoke the old one.
#
#   credential-deposit.sh --rule <broker rule .toml> --checkout <dir> \
#       --dest <token file> --owner <user> [--remote forgejo] [--target local]
#
# WHY IT EXISTS (design 1c90d183, David 2026-09-26; backlog c4cbc6b5).
# The forge host's checkout (/home/david/boss) held its forge token in
# the userinfo of its `forgejo` remote, and a git error that printed the
# URL printed the token into the forge-converge journal. No registry row
# named it, so the broker could not rotate it. This script is the
# host's half of the rotation:
#
#   D1  PUSH TO ITSELF. Design 835c0c9c decided a broker credential
#       reaches an off-cluster host by a deposit from the forge, which
#       holds admin; here the target IS the forge, so the deposit is a
#       local read and write by the same root process — no key, no hop,
#       no new grant (Q1, David: yes, through the admin kubeconfig the
#       host already holds). It runs FIRST in every forge-converge pass
#       and owes nothing to the forge token, so a revoked or broken
#       token cannot stop the step that repairs it (CLAUDE.md, "an arm
#       that needs the patient is not an arm"). Only the LOCAL target
#       exists; the remote target is the forced-command path backlog
#       7336cb5f builds, and it extends this script rather than writing
#       a second one.
#   D2  A FILE, NOT A URL. The value lives in --dest (0600, the owner's),
#       read at use time by a credential helper in the owner's GLOBAL git
#       config scoped to the forge's URL — the shape `boss credential
#       pull forge` uses on the pod. Global, because two consumers do not
#       run git in the checkout (the publish pushes from a clone root
#       owns; the tenant check reads other repos). The remote's userinfo
#       is stripped only AFTER the helper is proved to authenticate, so
#       the cutover cannot leave the host with no working credential.
#   D3  THE OLD TOKEN BY ITS LAST EIGHT. Every pass records the last
#       eight characters of the value the host holds on its converge
#       packet (`checkout_token_last_eight`) — the identifier the forge
#       shows beside each token — so the scoper can name the old token
#       without anyone holding its name or value.
#
# ONE PASS:
#   1. Read the Secret the broker rule declares (namespace, name, key,
#      and the credential id off its `when`) — the rule is the one
#      declaration; nothing here repeats it (CLAUDE.md §9a).
#   2. A value that differs from the file's is written to a temp file
#      beside it, PROVED (the forge answers `git ls-remote` through a
#      helper reading that temp file, and nothing else), then renamed
#      into place. With no file yet, the value in the remote URL's
#      userinfo is moved the same way (the cutover) — tried after the
#      Secret's when that one does not prove. A value that does not
#      prove changes nothing, and is named.
#   3. With a file in place: the owner's helper is (re)configured, and a
#      remote still carrying userinfo is stripped once the helper
#      authenticates through the GLOBAL config — the configuration every
#      consumer will actually use.
#   4. When the file holds the Secret's value, an open rotate-a-credential
#      packet of this credential whose `delivered` step is ready is
#      completed with the last eight — the record the broker's revoke
#      waits for. Two such packets is a refusal, never a guess, and the
#      Secret is read again after the packet is found: a value that
#      moved under the pass is recorded by the next one, never this one.
#
# NO VALUE EVER LEAVES A VARIABLE except into the owner's file: it goes
# to disk through a pipe from printf (a builtin), never an argv, and no
# message, summary field or request carries it — identifiers (last
# eight, paths, the stripped URL) only. Every git message is scrubbed of
# the values this run holds and of any URL userinfo before it is shown.
#
# Exit: 0 done (including "nothing to do"); 1 a step failed and says so
# on the converge packet; 78 a refusal of how it was invoked.
set -euo pipefail

ME=credential-deposit
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INFRA="$(cd "$HERE/.." && pwd)"

refuse() { echo "$ME: REFUSED — $*" >&2; exit 78; }
say() { echo "$ME: $*"; }

RULE="" CHECKOUT="" DEST="" OWNER="" OWNER_SET=0 REMOTE=forgejo TARGET=local
while [ $# -gt 0 ]; do
    [ $# -ge 2 ] || refuse "$1 needs a value"
    case "$1" in
        --rule) RULE="$2" ;;
        --checkout) CHECKOUT="$2" ;;
        --dest) DEST="$2" ;;
        --owner) OWNER="$2"; OWNER_SET=1 ;;
        --remote) REMOTE="$2" ;;
        --target) TARGET="$2" ;;
        *) refuse "unknown argument $1 (usage: $ME --rule FILE --checkout DIR --dest FILE --owner USER [--remote NAME] [--target local])" ;;
    esac
    shift 2
done
[ "$TARGET" = local ] \
    || refuse "target '$TARGET': only the local target exists. The remote target — a forced-command deposit onto another host — is what backlog 7336cb5f builds on this script (design 835c0c9c)"
[ -n "$RULE" ] && [ -n "$CHECKOUT" ] && [ -n "$DEST" ] && [ "$OWNER_SET" = 1 ] \
    || refuse "--rule, --checkout, --dest and --owner are all required"
if [ -z "$OWNER" ] && [ -z "${GIT_CONFIG_GLOBAL:-}" ]; then
    refuse "--owner '' runs every act as THIS user and writes its global git config; name the checkout's owner, or isolate the config with GIT_CONFIG_GLOBAL (the test harness)"
fi

# --- the declaration: the broker rule ----------------------------------
[ -r "$RULE" ] || refuse "cannot read the broker rule $RULE"
ARGS_LINE="$(grep -m1 '^args = ' "$RULE" || true)"
WHEN_LINE="$(grep -m1 '^when = ' "$RULE" || true)"
rule_arg() {
    local re="(^|[{ ,])$1 = \"\\\\\"([^\"\\\\]*)\\\\\"\""
    if [[ $ARGS_LINE =~ $re ]]; then printf '%s' "${BASH_REMATCH[2]}"; fi
}
NS="$(rule_arg secret_namespace)"
NAME="$(rule_arg secret_name)"
KEY="$(rule_arg secret_key)"
FORGE_USER="$(rule_arg forge_user)"
DELIVERY="$(rule_arg delivery)"
CRED=""
when_re='subject_id = "([^"]+)"'
if [[ $WHEN_LINE =~ $when_re ]]; then CRED="${BASH_REMATCH[1]}"; fi
[ -n "$NS" ] && [ -n "$NAME" ] && [ -n "$KEY" ] && [ -n "$FORGE_USER" ] && [ -n "$CRED" ] \
    || refuse "$RULE declares no literal secret_namespace / secret_name / secret_key / forge_user, or no subject_id in its when — cannot know what to deposit"
[ "$DELIVERY" = off-host ] \
    || refuse "$RULE declares delivery = '${DELIVERY:-mount}': a mount-delivered credential's consumers read the Secret itself, and its rotation waits on no host"

# shellcheck source=infra/run-summary.sh
. "$INFRA/run-summary.sh"
# shellcheck source=infra/lib/sor.sh
. "$INFRA/lib/sor.sh"

export GIT_TERMINAL_PROMPT=0
unset GIT_ASKPASS SSH_ASKPASS

# Every act on the owner's files and config runs AS the owner: a root
# `git config --global` or a root-owned file in their home breaks their
# own git later. `runuser -u` keeps the environment (HOME and USER
# become the owner's) and takes an argv, so nothing is re-parsed by a
# shell. An empty owner runs inline — the harness, whose files it owns.
as_owner() {
    if [ -n "$OWNER" ]; then runuser -u "$OWNER" -- "$@"; else "$@"; fi
}

# read_dest — the token file's value into DEST_VALUE, READ AS THE OWNER,
# and DEST_REFUSED naming why it was not read. This script runs as root,
# but $DEST and the directory holding it are the owner's to write, so
# root never reads the path itself (review A1 of the re-review of
# 5ef6db0b, 2026-09-26): a symlink planted there to
# /etc/boss-ops/kubeconfig was read as root, and its last eight
# published on the packet as the checkout token's. A symlink is refused
# by name and not followed; anything else is read by the owner, who
# cannot read what they could not already.
DEST_VALUE="" DEST_REFUSED=""
read_dest() {
    DEST_VALUE="" DEST_REFUSED=""
    if [ -L "$DEST" ]; then
        DEST_REFUSED="$DEST is a symlink, not the deposit's own file; it was not read"
    elif [ -s "$DEST" ] && ! DEST_VALUE="$(as_owner cat -- "$DEST" 2>/dev/null)"; then
        DEST_VALUE=""
        DEST_REFUSED="$DEST could not be read as ${OWNER:-this user}"
    fi
}

rc=0
SECRET_VALUE="" READ_VALUE="" URL_TOKEN="" CUR="" TMP=""
KERR="$(mktemp)"
cleanup() {
    rm -f "$KERR"
    if [ -n "$TMP" ]; then as_owner rm -f "$TMP" 2>/dev/null || true; fi
}
trap cleanup EXIT

last8() { if [ "${#1}" -le 8 ]; then printf '%s' "$1"; else printf '%s' "${1: -8}"; fi; }

# A message with every value this run holds, and any URL userinfo,
# taken out — then one line, bounded.
scrub() {
    local s="$1" v
    for v in "$SECRET_VALUE" "$READ_VALUE" "$URL_TOKEN" "$CUR"; do
        if [ -n "$v" ]; then s="${s//"$v"/<redacted>}"; fi
    done
    printf '%s' "$s" | sed -E 's#://[^/@[:space:]]+@#://<redacted>@#g' | tr '\n' ' ' | cut -c1-300
}

# --- the checkout's remote ---------------------------------------------
URL="$(as_owner git -C "$CHECKOUT" remote get-url "$REMOTE" 2>/dev/null || true)"
if [ -z "$URL" ]; then
    echo "$ME: FAILED — $CHECKOUT has no '$REMOTE' remote; there is no forge to prove a credential against" >&2
    run_summary_field deposit_action "failed: $CHECKOUT has no '$REMOTE' remote"
    exit 1
fi
SCHEME="${URL%%://*}"
REST="${URL#*://}"
HOSTPART="${REST%%/*}"
URLPATH="${REST#"$HOSTPART"}"
USERINFO=""
case "$HOSTPART" in
    *@*) USERINFO="${HOSTPART%@*}"; HOSTPART="${HOSTPART##*@}" ;;
esac
STRIPPED="$SCHEME://$HOSTPART$URLPATH"
BASE="$SCHEME://$HOSTPART"
# Userinfo is `user:token` or, as git itself printed it on this host,
# the token alone.
case "$USERINFO" in
    *:*) URL_TOKEN="${USERINFO#*:}" ;;
    *) URL_TOKEN="$USERINFO" ;;
esac

# The helper, reading FILE at use time: the value never sits in config.
helper_for() {
    printf '!f() { echo username=%s; echo "password=$(cat '\''%s'\'')"; }; f' "$FORGE_USER" "$1"
}
VERIFY_ERR=""
# verify_with FILE — the forge answers through a helper on FILE ONLY:
# `-c credential.helper=` empties every helper configured before it.
verify_with() {
    local err
    if err="$(as_owner git -c credential.helper= -c "credential.helper=$(helper_for "$1")" \
        ls-remote --heads "$STRIPPED" 2>&1 >/dev/null)"; then
        return 0
    fi
    VERIFY_ERR="$(scrub "$err")"
    return 1
}
# verify_global — the forge answers through the owner's own config, the
# path every consumer takes.
verify_global() {
    local err
    if err="$(as_owner git ls-remote --heads "$STRIPPED" 2>&1 >/dev/null)"; then
        return 0
    fi
    VERIFY_ERR="$(scrub "$err")"
    return 1
}

# --- 1. the Secret -----------------------------------------------------
KC_STATE=""
if [ -n "${BOSS_DEPOSIT_KUBECTL:-}" ]; then
    read -r -a KUBECTL <<< "$BOSS_DEPOSIT_KUBECTL"
else
    # The forge's admin kubeconfig — root material David placed, checked
    # (never written) by install-cluster-operator.sh — through the same
    # kubectl container every forge script uses (kubectl is not on the
    # host: infra/forge/host-absent-tools.txt).
    KC="${BOSS_OPS_DIR:-/etc/boss-ops}/kubeconfig"
    KUBECTL=(docker run --rm --network host -v "$KC:/kc:ro" alpine/k8s:1.33.3 kubectl --kubeconfig=/kc)
    # ABSENT is not REFUSED (backlog 714bc71f). A kubeconfig David has
    # not placed is root material no converge can mint or repair, so it
    # is recorded as not ready and does not red the pass — install-
    # cluster-operator.sh records the same fact as `ops_credentials`,
    # and the estate compare alarms on it. Exiting 1 here (car 78a88f65)
    # closed every forge converge failed for twelve hours, a permanent
    # red that hid any real one. A file that IS there and cannot be
    # read is the estate's fault, and stays `unreadable` (rc 1).
    if [ ! -e "$KC" ] && [ ! -L "$KC" ]; then
        KC_STATE="not ready: $KC absent — root material, placed by David (design 835c0c9c); the broker's rotation cannot be delivered until it is"
    elif [ ! -r "$KC" ]; then
        KC_STATE="unreadable: $KC is present but unreadable (the cluster-operator role's admin kubeconfig)"
    fi
fi
# read_secret — one read of the declared Secret into READ_VALUE (the
# value, or empty) and READ_STATE (what a record may say about it). A
# function because the delivery record reads it a second time (step 4).
read_secret() {
    local b64
    READ_VALUE="" READ_STATE="$KC_STATE"
    [ -z "$READ_STATE" ] || return 0
    if b64="$("${KUBECTL[@]}" -n "$NS" get secret "$NAME" -o "jsonpath={.data.$KEY}" 2>"$KERR")"; then
        if [ -z "$b64" ]; then
            READ_STATE=empty
        elif ! READ_VALUE="$(printf '%s' "$b64" | base64 -d 2>/dev/null)"; then
            READ_VALUE=""
            READ_STATE="unreadable: key $KEY of $NS/$NAME is not base64"
        elif [ -z "$READ_VALUE" ] || [[ $READ_VALUE =~ [[:space:]] ]]; then
            READ_VALUE=""
            READ_STATE="unreadable: key $KEY of $NS/$NAME is not one token"
        else
            READ_STATE="held:$(last8 "$READ_VALUE")"
        fi
    elif grep -q 'NotFound' "$KERR"; then
        READ_STATE=absent
    else
        READ_STATE="unreadable: $(scrub "$(head -n 3 "$KERR")")"
    fi
}
read_secret
SECRET_VALUE="$READ_VALUE" SECRET_STATE="$READ_STATE"
case "$SECRET_STATE" in unreadable*) rc=1 ;; esac
say "secret $NS/$NAME: $SECRET_STATE"

# --- 2. the file -------------------------------------------------------
# Every value that could become the file's, in the order they are
# tried: the Secret's when it differs from the file, then — only while
# there is no file — the remote URL's (the cutover). The first that
# PROVES is installed. The URL's value is tried even after the Secret's
# failed (review F5 of car 85b7b55f, 2026-09-26): with an empty file and
# a dead or foreign value in the Secret, the pass tried the Secret, gave
# up, and left the token in the URL — the shape that leaked — on every
# pass after. The Secret's failure is still named and still reds the run.
read_dest
CUR="$DEST_VALUE" FILE_REFUSED="$DEST_REFUSED"
[ -z "$FILE_REFUSED" ] || rc=1
CANDS=() SOURCES=()
if [ -n "$SECRET_VALUE" ] && [ "$SECRET_VALUE" != "$CUR" ]; then
    CANDS+=("$SECRET_VALUE") SOURCES+=("the Secret")
fi
if [ -z "$CUR" ] && [ -n "$URL_TOKEN" ] && [ "$URL_TOKEN" != "$SECRET_VALUE" ]; then
    CANDS+=("$URL_TOKEN") SOURCES+=("the remote URL")
fi
ACTION="unchanged" VERIFIED=0 REFUSED=""
if [ "${#CANDS[@]}" -eq 0 ] && [ -z "$CUR" ]; then
    ACTION="none: no credential in $DEST, in the Secret, or in the remote URL"
fi
for i in "${!CANDS[@]}"; do
    DIR="$(dirname "$DEST")"
    as_owner mkdir -p -m 700 "$DIR"
    TMP="$(as_owner mktemp "$DIR/.$(basename "$DEST").XXXXXX")"
    printf '%s' "${CANDS[$i]}" | as_owner sh -c 'umask 077 && cat > "$1"' sh "$TMP"
    if verify_with "$TMP"; then
        as_owner chmod 600 "$TMP"
        # -T: a link planted at $DEST is REPLACED, never moved into
        # (a link to a directory would otherwise take the file).
        as_owner mv -fT "$TMP" "$DEST"
        TMP=""
        VERIFIED=1
        ACTION="installed from ${SOURCES[$i]} (…$(last8 "${CANDS[$i]}"))"
        [ "${SOURCES[$i]}" = "the Secret" ] || ACTION="cutover: moved from the remote URL into $DEST (…$(last8 "${CANDS[$i]}"))"
        CUR="${CANDS[$i]}"
        break
    fi
    as_owner rm -f "$TMP"
    TMP=""
    REFUSED="${REFUSED:+$REFUSED; }the value from ${SOURCES[$i]} (…$(last8 "${CANDS[$i]}")) did not authenticate to $STRIPPED — $VERIFY_ERR"
    rc=1
done
if [ -n "$REFUSED" ]; then
    if [ "$VERIFIED" -eq 1 ]; then ACTION="$ACTION; $REFUSED"; else ACTION="failed: $REFUSED"; fi
fi
unset CANDS
if [ -n "$FILE_REFUSED" ]; then ACTION="refused: $FILE_REFUSED; $ACTION"; fi
say "file $DEST: $ACTION"

# --- 3. the helper and the remote --------------------------------------
REMOTE_STATE="carries-no-userinfo"
read_dest
if [ -n "$DEST_VALUE" ]; then
    WANT="$(helper_for "$DEST")"
    HAVE="$(as_owner git config --global --get-all "credential.$BASE.helper" 2>/dev/null || true)"
    if [ "$HAVE" != $'\n'"$WANT" ]; then
        # An empty entry first resets every helper configured before it,
        # so no older helper answers for the forge ahead of the file.
        as_owner git config --global --unset-all "credential.$BASE.helper" 2>/dev/null || true
        as_owner git config --global --add "credential.$BASE.helper" ""
        as_owner git config --global --add "credential.$BASE.helper" "$WANT"
        say "helper for $BASE reads $DEST"
    fi
    if [ -n "$USERINFO" ]; then
        if verify_global; then
            as_owner git -C "$CHECKOUT" remote set-url "$REMOTE" "$STRIPPED"
            VERIFIED=1
            REMOTE_STATE="stripped"
            say "remote $REMOTE is now $STRIPPED — the credential rides the helper"
        else
            REMOTE_STATE="carries-userinfo: the helper did not authenticate — $VERIFY_ERR"
            rc=1
        fi
    fi
elif [ -n "$USERINFO" ]; then
    REMOTE_STATE="carries-userinfo: no file to move it to"
fi

# --- 4. the delivery record --------------------------------------------
DELIVERY_STATE="nothing to record"
BOSS_USER="{\"id\":\"automation:credential-deposit\",\"role\":\"platform-admin\",\"access_tier\":\"operator\",\"territory_account_ids\":[],\"direct_report_ids\":[],\"department\":\"platform\"}"
API_CURL="$INFRA/boss-api-curl.sh"
[ -x "$API_CURL" ] || API_CURL=boss-api-curl.sh
record_delivery() {
    local jobs rows total pending n job step body l8 hdrs
    if [ -z "${BOSS_JOBS_URL:-}" ]; then
        DELIVERY_STATE="not recorded: BOSS_JOBS_URL is unset (/etc/boss/sor.env)"
        return 0
    fi
    hdrs=(-H "x-boss-user: $BOSS_USER")
    if [ -n "${BOSS_MACHINE_TOKEN:-}" ]; then hdrs+=(-H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"); fi
    if ! jobs="$("$API_CURL" -fsS "${hdrs[@]}" \
        "$BOSS_JOBS_URL/api/jobs?kind=rotate-a-credential&status=open&limit=500" 2>/dev/null)"; then
        # Not a failure of the deposit: the value is installed and the
        # next pass records it. The packet line says so.
        DELIVERY_STATE="not recorded: the jobs API did not answer; the next pass retries"
        return 0
    fi
    rows="$(jq '(if type == "object" and has("data") then .data else . end) | length' <<< "$jobs")"
    total="$(jq '(if type == "object" then .total else null end) // empty' <<< "$jobs")"
    if [ -n "$total" ] && [ "$total" != "$rows" ]; then
        DELIVERY_STATE="refused: the open-rotation read held $rows of $total rows — a limit is not a filter"
        rc=1
        return 0
    fi
    pending="$(jq -c --arg c "$CRED" '[(if type == "object" and has("data") then .data else . end)[]
        | select(.status == "open" and (.subject.id // .subject_id) == $c)
        | {job: .id, step: ((.steps // [])[] | select(.spec_slug == "delivered" and (.status == "ready" or .status == "active")) | .id)}]' <<< "$jobs")"
    n="$(jq 'length' <<< "$pending")"
    if [ "$n" -eq 0 ]; then
        DELIVERY_STATE="no open rotation of $CRED awaits delivery"
        return 0
    fi
    if [ "$n" -gt 1 ]; then
        DELIVERY_STATE="refused: $n open rotations of $CRED await delivery ($(jq -r '[.[].job[:8]] | join(", ")' <<< "$pending")) — not guessing which"
        rc=1
        return 0
    fi
    if [ "$VERIFIED" -eq 0 ] && ! verify_global; then
        DELIVERY_STATE="not recorded: the installed value did not authenticate — $VERIFY_ERR"
        rc=1
        return 0
    fi
    # THE SECRET AGAIN, AFTER THE PACKET WAS FOUND (review F3 of car
    # 85b7b55f, 2026-09-26). This pass read the Secret at its start; the
    # broker may since have installed a NEW value and readied this very
    # step — it readies only after its install, so a read made now sees
    # whatever value readied it. Recording the start-of-pass value would
    # complete `delivered` with the OLD last eight: the broker refuses
    # the mismatch, and a completed step cannot be recorded again, so
    # the rotation could never finish. A value that moved is recorded
    # by the next pass, which installs it first.
    read_secret
    if [ "$READ_VALUE" != "$SECRET_VALUE" ]; then
        case "$READ_STATE" in
            unreadable*)
                DELIVERY_STATE="not recorded: the Secret could not be read again before the record — $READ_STATE"
                rc=1
                ;;
            *)
                DELIVERY_STATE="not recorded: the Secret moved during this pass (…$(last8 "$SECRET_VALUE") when read, $READ_STATE now); the next pass installs it and records that"
                ;;
        esac
        return 0
    fi
    job="$(jq -r '.[0].job' <<< "$pending")"
    step="$(jq -r '.[0].step' <<< "$pending")"
    l8="$(last8 "$SECRET_VALUE")"
    body="$(jq -nc --arg l8 "$l8" --arg to "${BOSS_NODE_ID:-$(hostname)}:$DEST" \
        '{delivered_last_eight: $l8, delivered_to: $to}')"
    if ! "$API_CURL" -fsS -X PATCH "${hdrs[@]}" -H "content-type: application/json" \
            -d "$body" "$BOSS_JOBS_URL/api/jobs/$job/steps/$step/metadata" >/dev/null 2>&1 \
        || ! "$API_CURL" -fsS -X PUT "${hdrs[@]}" -H "content-type: application/json" \
            -d '{"status":"completed"}' "$BOSS_JOBS_URL/api/jobs/$job/steps/$step" >/dev/null 2>&1; then
        DELIVERY_STATE="not recorded: the write to ${job:0:8}'s delivered step failed; the next pass retries"
        rc=1
        return 0
    fi
    DELIVERY_STATE="recorded on ${job:0:8}: delivered …$l8"
}
if [ -n "$SECRET_VALUE" ] && [ "$CUR" = "$SECRET_VALUE" ]; then
    record_delivery
fi
say "delivery: $DELIVERY_STATE"

# --- the record --------------------------------------------------------
HELD="none"
read_dest
if [ -n "$DEST_VALUE" ]; then HELD="$(last8 "$DEST_VALUE")"; fi
run_summary_field checkout_token_last_eight "$HELD"
run_summary_field deposit_secret "$SECRET_STATE"
run_summary_field deposit_action "$ACTION"
run_summary_field deposit_remote "$REMOTE_STATE"
run_summary_field deposit_delivery "$DELIVERY_STATE"
say "the checkout's token ends in $HELD"
exit "$rc"
