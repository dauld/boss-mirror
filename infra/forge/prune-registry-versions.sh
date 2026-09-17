#!/usr/bin/env bash
#
# prune-registry-versions — delete the forge registry's image versions
# that no train, no rollback and no cluster can want any more, and
# record exactly what was kept, why, and what went.
#
# WHY IT EXISTS (backlog 9789a827, David 2026-09-17: the disk was not
# delivered; 'that order sounds great' — buy the headroom)
# ---------------------------------------------------------------------
# Measured 2026-09-16: /opt/forgejo/data is 93 GB of the forge's 228 GB
# disk — the Forgejo container registry every train pushes a
# `boss:<sha>` image to (~1–3 GB each, plus `boss-ci:<sha>` per train)
# — and nothing prunes the REGISTRY side. prune-registry-tags.lib.sh
# removes LOCAL docker tags, and only after the registry holds them,
# because the registry IS the rollback path; disk-floor-sweep reaches
# ~76 GB free and stops; the locomotive floor flapped all day until it
# was set to 40. The replacement disk was not delivered, so the
# registry keeps growing ~2 GB per train. Forgejo's own package cleanup
# rules are a dashboard act; this verb is the machine's.
#
# WHAT THE REGISTRY HOLDS (measured 2026-09-17 against the live forge,
# Forgejo 16.0.2: 1,423 container versions). `docker push` of a tag
# stores THREE package versions: the tag (whose one file is the OCI
# index, manifest.json) and two untagged `sha256:<digest>` children —
# the image manifest and buildx's provenance manifest — and the
# CHILDREN are where the layer files live. Deleting only the tag frees
# nothing; deleting a child another tag shares breaks that tag. So the
# index of EVERY tagged version is read (through /v2, the docker pull
# path) before anything is classified, and a child goes only when
# every tag that references it goes. The packages API declares no
# sizes (`files[].size` is null), so bytes are not claimed here: the
# record carries `df` of the data directory before and after, and the
# blobs themselves are reclaimed by Forgejo's own cleanup cron once no
# version references them — the operator reads the difference on the
# packet, this script does not guess it.
#
# THE KEEP SET, derived — never a list typed here — and a half that
# cannot be derived is a REFUSAL that deletes nothing:
#
#   1. THE LIVE IMAGES. Every image tag the cluster runs under the
#      registry's owner (`kubectl get deploy,sts,cronjob -A`, the same
#      kubectl resolution the census uses), and specifically the tag
#      deploy/boss in namespace boss serves — the cluster-watchdog's
#      own read. No deploy/boss image = refuse.
#   2. THE ROLLBACK TARGET. The last-converged stamp the deploy runner
#      writes and the watchdog rolls to BY NAME
#      (`$BOSS_FORGE_LAST_BUILT`, default the checkout owner's
#      ~/.boss-last-built — the runner runs as that user; this verb
#      runs as root under the ops runner with no HOME). Absent or not
#      a sha = refuse: "roll back" is a target, not a verb.
#   3. THE LANDED TRAINS. landed-train-shas.lib.sh, the one reader the
#      disk sweep already trusts, over the newest 2N pr-train packets
#      (N = keep_trains, default 10; 2N so that at least N landed
#      trains are covered unless more than half are still open, and
#      more is the safe direction). It prints nothing when it cannot
#      answer, and nothing = refuse.
#   4. `latest` — the quickstart tag and the chores' image, never a
#      build artifact of the converge (prune-registry-tags.lib.sh).
#   5. EVERYTHING NEWER THAN 24 h (BOSS_PRUNE_KEEP_HOURS): a push in
#      flight, a train not yet closed, a gate's hand build.
#   6. EVERYTHING IT CANNOT CLASSIFY: a tag that is not a sha
#      (`rust1.96`), a version whose created_at does not parse, a tag
#      whose index could not be read (its children are unknown), and —
#      while ANY index was unreadable — every untagged version no
#      readable index references. Unclassified is kept and named.
#
# THE CREDENTIAL. The converge pushes with david's rootless docker and
# its ambient registry login (cluster-deploy-runner.sh; mirror-base-
# images.sh points root at the same config). That login is
# `auths."<registry host>".auth` in the checkout owner's
# ~/.docker/config.json — base64 of user:token — and it is the one
# credential this verb uses, for the packages API (Basic, the token as
# the password) and for the /v2 manifest reads (a bearer minted from it
# at /v2/token, docker's own flow). IT IS NEVER PRINTED: it rides in a
# curl config file (mode 600, in a private tempdir), never in argv,
# and every line a tool says back is scrubbed of it before it reaches
# the packet. Measured 2026-09-17: the pod's read token answers the
# listing 403 for want of `read:package`, so the scope is measured here
# too — a 403 refuses NAMING THE SCOPE the forge's own message asks
# for (`read:package` on the listing, `write:package` on the first
# DELETE), a ceremony for David, and the real run stops on that first
# DELETE before a second attempt. Forgejo's package scopes are read and
# write; a delete is a write.
#
# USAGE
#   prune-registry-versions.sh --dry-run | --for-real [keep_trains]
#
# EXIT
#   0  done (or, with --dry-run, the keep set and the plan)
#   2  refused — the reason names the bound; nothing was deleted
#   1  failed part-way — the record states what was deleted; or a
#      tool this needs could not answer
#
# The record is ONE JSON line on stdout; every human-readable line is
# on stderr, so the ops runner's captured output carries both and a
# reader can jq the last line.
#
# ENV (test seams — the ops-runner passes no packet-supplied environment,
# only an argv built from the allowlist, so a packet cannot set these)
#   BOSS_JOBS_URL              the system of record (required, no default:
#                              a guessed instance answers 'no trains')
#   BOSS_FORGE_REGISTRY        the image the converge pushes
#                              (default: 10.20.0.15:3000/david/boss — the
#                              runner's own default; host, owner and the
#                              sibling boss-ci are derived from it)
#   BOSS_PRUNE_FORGE_URL       the forge's HTTP base (default: http://<host>,
#                              the registry is plain HTTP on the LAN)
#   BOSS_PRUNE_DOCKER_CONFIG   the docker config holding the login
#                              (default: <checkout owner>/.docker/config.json)
#   BOSS_FORGE_LAST_BUILT      the converge's stamp file
#                              (default: <checkout owner>/.boss-last-built)
#   BOSS_PRUNE_KEEP_HOURS      the freshness window (default: 24)
#   BOSS_PRUNE_DF_PATH         the directory df measures (default:
#                              /opt/forgejo/data; unmeasured if absent)
#   BOSS_KUBECTL / KUBECONFIG  see undeclared-objects.sh; resolved once

set -uo pipefail

ME="prune-registry-versions"
say() { echo "$ME: $*" >&2; }
refuse() { say "REFUSED — $*"; say "  Nothing was deleted."; exit 2; }

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$SELF_DIR/../.." && pwd)"
RESOLVE="$REPO/infra/cluster/undeclared-objects.sh"

# --- bound 1: the arguments -------------------------------------------------
usage() {
    say "usage: $ME --dry-run | --for-real [keep_trains]"
    say "  --dry-run   derive the keep set, read the registry, print the plan; deletes nothing"
    say "  --for-real  the same, then DELETE every version the plan names"
    say "  keep_trains how many of the newest landed trains' shas to keep (default 10)"
    exit 2
}
[ "$#" -ge 1 ] && [ "$#" -le 2 ] || usage
DRY=""
case "$1" in
    --dry-run) DRY=1 ;;
    --for-real) DRY=0 ;;
    *) say "the only modes are --dry-run and --for-real, not \`$1\`"; usage ;;
esac
KEEP_TRAINS="${2:-10}"
# The allowlist checks the shape first; the script re-checks rather
# than relying on one layer (the retire-second-stack convention).
case "$KEEP_TRAINS" in
    ''|*[!0-9]*|0*) refuse "keep_trains must be a positive whole number, not \`$KEEP_TRAINS\`" ;;
esac
[ "$KEEP_TRAINS" -le 999 ] || refuse "keep_trains of $KEEP_TRAINS is beyond the 999 the allowlist admits"
KEEP_HOURS="${BOSS_PRUNE_KEEP_HOURS:-24}"
case "$KEEP_HOURS" in
    ''|*[!0-9]*) refuse "BOSS_PRUNE_KEEP_HOURS must be a whole number of hours, not \`$KEEP_HOURS\`" ;;
esac

for tool in jq curl date; do
    command -v "$tool" >/dev/null 2>&1 || { say "$tool is not on PATH, so nothing can be read. Nothing was deleted."; exit 1; }
done

JOBS_URL="${BOSS_JOBS_URL:-}"
[ -n "$JOBS_URL" ] || refuse "BOSS_JOBS_URL is not set and there is no safe default: the landed trains are read from the system of record, and a read against a guessed instance answers 'no trains' instead of erroring"

# --- the registry, derived from the image the converge pushes --------------
# `10.20.0.15:3000/david/boss` -> host 10.20.0.15:3000, owner david, and
# the two packages this verb touches: boss (the converge's image) and
# boss-ci (the per-train CI image, .forgejo/workflows/ci.yml). Nothing
# else in the registry — boss-ci-cache, the mirrored bases — is ever a
# candidate: a version of another name is not read.
REGISTRY="${BOSS_FORGE_REGISTRY:-10.20.0.15:3000/david/boss}"
REG_HOST="${REGISTRY%%/*}"
reg_rest="${REGISTRY#*/}"
OWNER="${reg_rest%%/*}"
IMAGE_NAME="${reg_rest#*/}"
case "$REG_HOST/$OWNER/$IMAGE_NAME" in
    "$REGISTRY") ;;
    *) refuse "BOSS_FORGE_REGISTRY \`$REGISTRY\` is not of the shape host[:port]/owner/name" ;;
esac
PACKAGES="$IMAGE_NAME ${IMAGE_NAME}-ci"
FORGE_URL="${BOSS_PRUNE_FORGE_URL:-http://$REG_HOST}"

# --- the checkout owner: whose docker login and stamp these are -----------
# The ops runner is root with no HOME; the converge runs as the
# checkout's owner and its login and stamp live in that user's home.
# Read off the directory, never hardcoded (delete-orphan-object.sh's
# as_owner; ops-request c9877f75). Only consulted for a default.
owner_home() {
    local owner
    owner="$(stat -c %U "$REPO" 2>/dev/null)"
    [ -n "$owner" ] && [ "$owner" != "UNKNOWN" ] || return 1
    getent passwd "$owner" | cut -d: -f6
}
if [ -z "${BOSS_PRUNE_DOCKER_CONFIG:-}" ] || [ -z "${BOSS_FORGE_LAST_BUILT:-}" ]; then
    OWNER_HOME="$(owner_home)" || refuse "cannot resolve the owner of $REPO, so neither the docker login nor the converge's stamp has a default path; set BOSS_PRUNE_DOCKER_CONFIG and BOSS_FORGE_LAST_BUILT"
fi
DOCKER_CONFIG_FILE="${BOSS_PRUNE_DOCKER_CONFIG:-$OWNER_HOME/.docker/config.json}"
STAMP_FILE="${BOSS_FORGE_LAST_BUILT:-$OWNER_HOME/.boss-last-built}"
DF_PATH="${BOSS_PRUNE_DF_PATH:-/opt/forgejo/data}"

TMP=$(mktemp -d) || exit 1
chmod 700 "$TMP"
trap 'rm -rf "$TMP"' EXIT

# --- bound 2: the credential ------------------------------------------------
# Parsed into variables and never printed. `scrub` removes the literal
# auth string, the token and the bearer from anything a tool said
# back, so even an error that quoted a header cannot carry it onto the
# packet.
AUTH_B64=""
TOKEN=""
BEARER=""
scrub() { # stdin -> stdout, every credential form replaced
    local line
    while IFS= read -r line || [ -n "$line" ]; do
        [ -n "$AUTH_B64" ] && line="${line//"$AUTH_B64"/***}"
        [ -n "$TOKEN" ] && line="${line//"$TOKEN"/***}"
        [ -n "$BEARER" ] && line="${line//"$BEARER"/***}"
        printf '%s\n' "$line"
    done
}
[ -f "$DOCKER_CONFIG_FILE" ] || refuse "no docker config at $DOCKER_CONFIG_FILE — the converge's registry login (docker login $REG_HOST, as the checkout owner) is the credential this verb uses, and it is not there"
if ! jq -e . < "$DOCKER_CONFIG_FILE" > /dev/null 2>&1; then
    refuse "$DOCKER_CONFIG_FILE is not JSON, so the login for $REG_HOST cannot be read"
fi
AUTH_B64=$(jq -r --arg host "$REG_HOST" '
    (.auths // {}) | to_entries[]
    | select((.key | sub("^https?://"; "") | sub("/.*$"; "")) == $host)
    | .value.auth // empty' "$DOCKER_CONFIG_FILE" 2>/dev/null | awk 'NR == 1')
if [ -z "$AUTH_B64" ]; then
    store=$(jq -r --arg host "$REG_HOST" '(.credHelpers // {})[$host] // .credsStore // empty' "$DOCKER_CONFIG_FILE" 2>/dev/null)
    if [ -n "$store" ]; then
        refuse "$DOCKER_CONFIG_FILE keeps the login for $REG_HOST in a credential store (credsStore/credHelpers: $store), not inline; this verb reads only an inline docker login"
    fi
    refuse "$DOCKER_CONFIG_FILE holds no auth for $REG_HOST — run docker login $REG_HOST as the checkout owner (the converge's own push credential), then ask again"
fi
USERINFO=$(printf '%s' "$AUTH_B64" | base64 -d 2>/dev/null)
CRED_USER="${USERINFO%%:*}"
TOKEN="${USERINFO#*:}"
if [ -z "$USERINFO" ] || [ "$USERINFO" = "$CRED_USER" ] || [ -z "$CRED_USER" ] || [ -z "$TOKEN" ]; then
    refuse "the auth for $REG_HOST in $DOCKER_CONFIG_FILE does not decode to user:token"
fi
# The curl configs: the header rides in a file the kernel never shows
# in a process listing, and the file dies with the tempdir.
BASIC_CFG="$TMP/basic.cfg"
( umask 077; printf 'header = "Authorization: Basic %s"\n' "$AUTH_B64" > "$BASIC_CFG" )
say "credential: docker login for $REG_HOST as $CRED_USER from $DOCKER_CONFIG_FILE (the converge's push credential; never printed)"

# One HTTP call: prints the status code; the body lands in the named
# file. No -f: the code is read, and a 403 is a verdict, not a failure.
http() { # <cfg> <method> <url> <body-out> [extra curl args] -> stdout: code
    local cfg="$1" method="$2" url="$3" out="$4"
    shift 4
    curl -sS --max-time 60 -K "$cfg" -X "$method" -o "$out" -w '%{http_code}' "$@" "$url" 2> "$TMP/curl.err"
}
# The scope Forgejo's 403 asks for, from its own message — copied, not
# retyped; the fallback is the scope the method needs.
scope_of() { # <body-file> <fallback>
    local named
    # awk drains its input; `head -n 1` would SIGPIPE the producer under
    # pipefail (backlog 76d04429).
    named=$(jq -r '.message // empty' "$1" 2>/dev/null | grep -oE '(read|write|delete):package' | awk 'NR == 1')
    printf '%s' "${named:-$2}"
}

# --- bound 3: the keep set, half by half -----------------------------------
# (a) the live images, cluster-wide, through the one kubectl resolution.
KUBECTL_LINE=$("$RESOLVE" --kubectl) || {
    say "REFUSED — no kubectl to read the cluster's live images with (see above); the keep set cannot be derived."
    say "  Nothing was deleted."
    exit 2
}
read -r -a KUBECTL <<<"$KUBECTL_LINE"
if ! "${KUBECTL[@]}" get deploy,sts,cronjob -A -o json > "$TMP/cluster.json" 2> "$TMP/kubectl.err"; then
    say "REFUSED — the cluster's live images cannot be read (kubectl get deploy,sts,cronjob -A); kubectl said:"
    sed 's/^/    /' "$TMP/kubectl.err" >&2
    say "  A keep set missing the live image is not a keep set. Nothing was deleted."
    exit 2
fi
# Every image under <host>/<owner>/ the cluster's templates name, as
# name:tag — deployments, statefulsets and cronjobs, main and init
# containers alike.
jq -r --arg prefix "$REG_HOST/$OWNER/" '
    [ .items[]
      | (.spec.template.spec // .spec.jobTemplate.spec.template.spec // {})
      | ((.containers // []) + (.initContainers // []))[]
      | .image // empty
      | select(startswith($prefix))
      | ltrimstr($prefix) ]
    | unique[]' "$TMP/cluster.json" > "$TMP/live.txt" 2>/dev/null
LIVE_MAIN=$(jq -r --arg prefix "$REG_HOST/$OWNER/" --arg name "$IMAGE_NAME" '
    .items[]
    | select(.kind == "Deployment" and .metadata.name == "boss" and .metadata.namespace == "boss")
    | .spec.template.spec.containers[0].image // empty
    | select(startswith($prefix + $name + ":"))
    | ltrimstr($prefix + $name + ":")' "$TMP/cluster.json" 2>/dev/null | awk 'NR == 1')
[ -n "$LIVE_MAIN" ] || refuse "the cluster lists no deploy/boss in namespace boss running $REG_HOST/$OWNER/$IMAGE_NAME:<tag> — the live image is the first half of the keep set, and it is not there to keep"
LIVE_COUNT=$(grep -c . "$TMP/live.txt" || true)
say "keep: live: $LIVE_MAIN (deploy/boss in namespace boss); every $OWNER image the cluster runs ($LIVE_COUNT): $(paste -sd ' ' "$TMP/live.txt")"

# (b) the rollback target: the converge's stamp, the watchdog's target.
STAMP=$(head -n 1 "$STAMP_FILE" 2>/dev/null | tr -d '[:space:]')
[ -n "$STAMP" ] || refuse "no rollback target: $STAMP_FILE is absent or empty (cluster-deploy-runner writes it after every converge; cluster-watchdog rolls to it by name), so the keep set cannot be derived"
if ! [[ "$STAMP" =~ ^[0-9a-f]{7,40}$ ]]; then
    refuse "the rollback target in $STAMP_FILE is \`$STAMP\`, not a sha, so the keep set cannot be derived"
fi
say "keep: stamp: $STAMP (the rollback target, $STAMP_FILE)"

# (c) the landed trains, through the lib the disk sweep trusts. It
# signs as this verb, prints keys on stdout and its own account on
# stderr, and prints NOTHING when it cannot answer — which here is a
# refusal, not a fallback: there is no age-only mode for a registry
# delete.
export BOSS_SWEEP_ACTOR="${BOSS_SWEEP_ACTOR:-automation:prune-registry-versions}"
# shellcheck source=infra/forge/landed-train-shas.lib.sh
. "$SELF_DIR/landed-train-shas.lib.sh"
LOOKBACK=$((KEEP_TRAINS * 2))
LANDED=$(landed_train_shas curl "$JOBS_URL" "$LOOKBACK" "$ME")
LANDED_COUNT=$(printf '%s\n' "$LANDED" | grep -c . || true)
[ "$LANDED_COUNT" -gt 0 ] || refuse "no landed train shas could be read from $JOBS_URL (see the lookup's own account above), so the keep set cannot be derived"
say "keep: landed: $LANDED_COUNT sha(s) from the newest $LOOKBACK pr-train packets (keep_trains $KEEP_TRAINS)"
say "keep: latest, and every version newer than $KEEP_HOURS h"

# The keep keys: seven-hex, the lib's convention — a boss tag is seven
# (git rev-parse --short), a boss-ci tag is forty (github.sha), and the
# first seven is one comparison for both. key<TAB>reason.
{
    for t in $(cat "$TMP/live.txt"); do
        tag="${t#*:}"
        [[ "$tag" =~ ^[0-9a-f]{7,40}$ ]] && printf '%s\tlive (%s)\n' "${tag:0:7}" "$t"
    done
    printf '%s\tstamp (rollback target)\n' "${STAMP:0:7}"
    printf '%s\n' "$LANDED" | grep . | sed 's/$/\tlanded train/'
} | sort -u -t "$(printf '\t')" -k1,1 > "$TMP/keys.tsv"

# --- the registry: every version of the two packages ------------------------
# A LIMIT IS NOT A FILTER: pages of 50 until an empty page, with the
# X-Total-Count the forge reports beside the count read, so the record
# says how much of the registry was seen. Versions arrive every train,
# so the two can differ by a push; a push in flight is inside the
# freshness window either way.
NOW=$(date -u +%s)
: > "$TMP/versions.raw"   # name<TAB>version<TAB>created_at, every package
: > "$TMP/versions.tsv"   # name<TAB>version<TAB>created_at<TAB>epoch, the two
page=1
LISTED=0
TOTAL=""
while :; do
    code=$(http "$BASIC_CFG" GET "$FORGE_URL/api/v1/packages/$OWNER?type=container&limit=50&page=$page" "$TMP/page.json" -D "$TMP/page.hdr")
    case "$code" in
        200) ;;
        403)
            scope=$(scope_of "$TMP/page.json" read:package)
            say "REFUSED — the registry listing answered 403: the docker login for $REG_HOST (user $CRED_USER) lacks $scope; the forge said:"
            jq -r '.message // empty' "$TMP/page.json" 2>/dev/null | scrub | sed 's/^/    /' >&2
            say "  Minting a token with $scope (and write:package for the delete) is a root ceremony, David's; then docker login $REG_HOST with it as the checkout owner."
            say "  Nothing was deleted."
            exit 2 ;;
        *)
            say "the registry listing (page $page) answered $code, not 200:"
            { scrub < "$TMP/curl.err"; head -c 400 "$TMP/page.json" | scrub; } | sed 's/^/    /' >&2
            say "  Nothing was deleted."
            exit 1 ;;
    esac
    [ -n "$TOTAL" ] || TOTAL=$(tr -d '\r' < "$TMP/page.hdr" | sed -n 's/^[Xx]-[Tt]otal-[Cc]ount: *//p' | awk 'NR == 1')
    n=$(jq -r 'if type == "array" then length else "notlist" end' "$TMP/page.json" 2>/dev/null)
    case "$n" in
        ''|notlist) say "the registry listing (page $page) is not a version list this can read. Nothing was deleted."; exit 1 ;;
        0) break ;;
    esac
    LISTED=$((LISTED + n))
    jq -r '.[] | [.name, .version, .created_at] | @tsv' "$TMP/page.json" >> "$TMP/versions.raw"
    page=$((page + 1))
    [ "$page" -le 400 ] || { say "the registry listing did not end after 400 pages; refusing to guess at the rest. Nothing was deleted."; exit 1; }
done
say "registry: $LISTED version(s) listed for $OWNER (the forge counts ${TOTAL:-?}), in $((page - 1)) page(s)"

# Only the two packages; the epoch parsed by date, unparsable -> empty,
# and empty is unclassified below.
while IFS=$'\t' read -r name ver created; do
    case " $PACKAGES " in *" $name "*) ;; *) continue ;; esac
    epoch=$(date -u -d "$created" +%s 2>/dev/null || true)
    printf '%s\t%s\t%s\t%s\n' "$name" "$ver" "$created" "$epoch" >> "$TMP/versions.tsv"
done < "$TMP/versions.raw"
CANDIDATES=$(grep -c . "$TMP/versions.tsv" || true)
say "registry: $CANDIDATES version(s) belong to $PACKAGES"

# --- the indexes: what each tag's children are ------------------------------
# A bearer for /v2, minted the way docker mints one (GET /v2/token with
# the login). When the forge will not mint one, no index is readable,
# every tag's children are unknown, and this pass deletes NO tag —
# loudly, below.
BEARER_CFG="$TMP/bearer.cfg"
INDEX_OK=1
code=$(http "$BASIC_CFG" GET "$FORGE_URL/v2/token?service=container_registry" "$TMP/token.json")
if [ "$code" = 200 ]; then
    BEARER=$(jq -r '.token // empty' "$TMP/token.json" 2>/dev/null)
fi
if [ -z "$BEARER" ]; then
    INDEX_OK=0
    say "index: /v2/token answered $code with no token for the $REG_HOST login, so no manifest index can be read this pass — every tag's children are unknown, and no tag will be deleted"
else
    ( umask 077; printf 'header = "Authorization: Bearer %s"\n' "$BEARER" > "$BEARER_CFG" )
fi
ACCEPT='Accept: application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json'
: > "$TMP/children.tsv"     # name<TAB>tag<TAB>child-digest
: > "$TMP/unreadable.tsv"   # name<TAB>tag<TAB>code
if [ "$INDEX_OK" = 1 ]; then
    while IFS=$'\t' read -r name ver created epoch; do
        case "$ver" in sha256:*) continue ;; esac
        code=$(http "$BEARER_CFG" GET "$FORGE_URL/v2/$OWNER/$name/manifests/$ver" "$TMP/manifest.json" -H "$ACCEPT")
        if [ "$code" != 200 ] || ! jq -e . < "$TMP/manifest.json" > /dev/null 2>&1; then
            printf '%s\t%s\t%s\n' "$name" "$ver" "$code" >> "$TMP/unreadable.tsv"
            continue
        fi
        jq -r --arg n "$name" --arg t "$ver" '(.manifests // [])[] | .digest // empty | [$n, $t, .] | @tsv' "$TMP/manifest.json" >> "$TMP/children.tsv"
    done < "$TMP/versions.tsv"
fi
UNREADABLE=$(grep -c . "$TMP/unreadable.tsv" || true)
if [ "$UNREADABLE" -gt 0 ]; then
    say "index: $UNREADABLE tag(s) whose manifest could not be read — kept, and every orphan is unclassified this pass:"
    awk -F'\t' '{ printf "    %s:%s (HTTP %s)\n", $1, $2, $3 }' "$TMP/unreadable.tsv" >&2
fi

# --- the classification ------------------------------------------------------
# One pass over the tags, one over the untagged children; the verdicts
# land in classes.tsv as name<TAB>version<TAB>class<TAB>reason, class
# one of keep / delete / unclassified. Every rule keeps by default: a
# version reaches `delete` only by passing every keep test.
FRESH_BEFORE=$((NOW - KEEP_HOURS * 3600))
: > "$TMP/classes.tsv"
classify_tag() { # <name> <ver> <epoch> -> class<TAB>reason
    local name="$1" ver="$2" epoch="$3" key reason
    if [ "$ver" = latest ]; then printf 'keep\tlatest\n'; return; fi
    if ! [[ "$ver" =~ ^[0-9a-f]{7,40}$ ]]; then printf 'unclassified\ttag is not a sha\n'; return; fi
    key="${ver:0:7}"
    reason=$(awk -F'\t' -v k="$key" '$1 == k { print $2; exit }' "$TMP/keys.tsv")
    if [ -n "$reason" ]; then printf 'keep\t%s\n' "$reason"; return; fi
    case "$epoch" in
        ''|*[!0-9]*) printf 'unclassified\tcreated_at does not parse\n'; return ;;
    esac
    if [ "$epoch" -ge "$FRESH_BEFORE" ]; then printf 'keep\tnewer than %s h\n' "$KEEP_HOURS"; return; fi
    if [ "$INDEX_OK" != 1 ]; then printf 'unclassified\tchildren unknown (no bearer)\n'; return; fi
    if awk -F'\t' -v n="$name" -v t="$ver" '$1 == n && $2 == t { f = 1 } END { exit !f }' "$TMP/unreadable.tsv"; then
        printf 'unclassified\tindex unreadable (HTTP %s)\n' "$(awk -F'\t' -v n="$name" -v t="$ver" '$1 == n && $2 == t { print $3; exit }' "$TMP/unreadable.tsv")"
        return
    fi
    printf 'delete\tolder than the keep set\n'
}
while IFS=$'\t' read -r name ver created epoch; do
    case "$ver" in sha256:*) continue ;; esac
    verdict=$(classify_tag "$name" "$ver" "$epoch")
    printf '%s\t%s\t%s\n' "$name" "$ver" "$verdict" >> "$TMP/classes.tsv"
done < "$TMP/versions.tsv"
classify_child() { # <name> <digest> <epoch> -> class<TAB>reason
    local name="$1" dig="$2" epoch="$3" refs kept
    case "$epoch" in
        ''|*[!0-9]*) printf 'unclassified\tcreated_at does not parse\n'; return ;;
    esac
    if [ "$epoch" -ge "$FRESH_BEFORE" ]; then printf 'keep\tnewer than %s h\n' "$KEEP_HOURS"; return; fi
    # The tags whose index names this digest, and whether any is staying.
    refs=$(awk -F'\t' -v n="$name" -v d="$dig" '$1 == n && $3 == d { print $2 }' "$TMP/children.tsv")
    if [ -n "$refs" ]; then
        kept=$(printf '%s\n' "$refs" | while IFS= read -r t; do
            awk -F'\t' -v n="$name" -v t="$t" '$1 == n && $2 == t && $3 != "delete" { print $2; exit }' "$TMP/classes.tsv"
        done | awk 'NR == 1')
        if [ -n "$kept" ]; then printf 'keep\treferenced by a kept tag (%s)\n' "$kept"; return; fi
        printf 'delete\tchild of a deleted tag (%s)\n' "$(printf '%s\n' "$refs" | awk 'NR == 1')"
        return
    fi
    if [ "$INDEX_OK" != 1 ] || [ "$UNREADABLE" -gt 0 ]; then
        printf 'unclassified\treferenced by no readable index while %s index(es) were unreadable\n' "$UNREADABLE"
        return
    fi
    printf 'delete\torphan: referenced by no tag\n'
}
while IFS=$'\t' read -r name ver created epoch; do
    case "$ver" in sha256:*) ;; *) continue ;; esac
    verdict=$(classify_child "$name" "$ver" "$epoch")
    printf '%s\t%s\t%s\n' "$name" "$ver" "$verdict" >> "$TMP/classes.tsv"
done < "$TMP/versions.tsv"

# --- the plan, on the packet ------------------------------------------------
for name in $PACKAGES; do
    awk -F'\t' -v n="$name" '$1 == n { c[$3]++ } END { printf "%s: %d keep, %d delete, %d unclassified\n", n, c["keep"], c["delete"], c["unclassified"] }' "$TMP/classes.tsv" | sed "s/^/$ME: plan: /" >&2
done
awk -F'\t' '$3 == "delete" { printf "would DELETE %s:%s — %s\n", $1, $2, $4 }' "$TMP/classes.tsv" | sed "s/^/$ME: /" >&2
awk -F'\t' '$3 == "unclassified" { printf "unclassified %s:%s — %s (kept)\n", $1, $2, $4 }' "$TMP/classes.tsv" | sed "s/^/$ME: /" >&2

disk_avail_kb() { # -> KB free where the registry lives, or "null"
    if [ -d "$DF_PATH" ]; then df -Pk "$DF_PATH" 2>/dev/null | awk 'NR == 2 { print $4 }' | grep -E '^[0-9]+$' || echo null
    else echo null; fi
}
DF_BEFORE=$(disk_avail_kb)
[ "$DF_BEFORE" != null ] || say "df: $DF_PATH is not here, so the bytes are unmeasured"

# --- the deletes (--for-real only): tags first, then children ---------------
# Tags first so that a run that dies part-way leaves untagged children
# (collectable as orphans next pass) rather than a tag whose index
# points at nothing. Every DELETE is judged by its code: 204 is gone,
# 404 was already gone, 403 is the scope refusal that ends the run
# before a second attempt, anything else stops the run with the count
# so far on the record.
: > "$TMP/deleted.tsv"   # name<TAB>version<TAB>outcome (deleted|gone|failed)
REFUSED_SCOPE=""
FAIL_CODE=""
WRITE_SCOPE="unmeasured: a dry run cannot prove write:package without deleting"
if [ "$DRY" = 0 ]; then
    WRITE_SCOPE="unmeasured: nothing to delete"
    { awk -F'\t' '$3 == "delete" && $2 !~ /^sha256:/' "$TMP/classes.tsv"
      awk -F'\t' '$3 == "delete" && $2 ~ /^sha256:/' "$TMP/classes.tsv"; } > "$TMP/todo.tsv"
    while IFS=$'\t' read -r name ver cls reason; do
        code=$(http "$BASIC_CFG" DELETE "$FORGE_URL/api/v1/packages/$OWNER/container/$name/$ver" "$TMP/delete.json")
        case "$code" in
            204) printf '%s\t%s\tdeleted\n' "$name" "$ver" >> "$TMP/deleted.tsv"; WRITE_SCOPE="write:package proven by DELETE" ;;
            404) printf '%s\t%s\tgone\n' "$name" "$ver" >> "$TMP/deleted.tsv"; say "DELETE $name:$ver answered 404 — already gone" ;;
            403)
                REFUSED_SCOPE=$(scope_of "$TMP/delete.json" write:package)
                say "REFUSED — DELETE $name:$ver answered 403: the docker login for $REG_HOST (user $CRED_USER) lacks $REFUSED_SCOPE; the forge said:"
                jq -r '.message // empty' "$TMP/delete.json" 2>/dev/null | scrub | sed 's/^/    /' >&2
                say "  Minting a token with read:package and $REFUSED_SCOPE is a root ceremony, David's; then docker login $REG_HOST with it as the checkout owner. No further DELETE was attempted."
                break ;;
            *)
                FAIL_CODE="$code"
                printf '%s\t%s\tfailed\n' "$name" "$ver" >> "$TMP/deleted.tsv"
                say "FAILED — DELETE $name:$ver answered $code, not 204; stopping here:"
                { scrub < "$TMP/curl.err"; head -c 400 "$TMP/delete.json" | scrub; } | sed 's/^/    /' >&2
                break ;;
        esac
    done < "$TMP/todo.tsv"
fi
DF_AFTER=null
[ "$DRY" = 0 ] && DF_AFTER=$(disk_avail_kb)

# --- the record ---------------------------------------------------------------
# Per package: versions, kept (by reason), delete (planned), deleted /
# gone / failed (done), unclassified, index_unreadable. Bytes are not
# declared by the packages API (measured: size null), so `declared_bytes`
# is null and `df` carries the measurement.
PACKAGES_JSON=$(jq -n -R -c --rawfile classes "$TMP/classes.tsv" --rawfile done "$TMP/deleted.tsv" --rawfile unreadable "$TMP/unreadable.tsv" --arg pkgs "$PACKAGES" '
    def rows(s): [ s | split("\n")[] | select(length > 0) | split("\t") ];
    (rows($classes)) as $c | (rows($done)) as $d | (rows($unreadable)) as $u
    | [ ($pkgs | split(" "))[] | . as $n
        | { key: $n, value: {
            versions: ([ $c[] | select(.[0] == $n) ] | length),
            kept: ([ $c[] | select(.[0] == $n and .[2] == "keep") ] | length),
            kept_by: ([ $c[] | select(.[0] == $n and .[2] == "keep") | .[3] | sub(" \\(.*\\)$"; "") ] | group_by(.) | map({ (.[0]): length }) | add // {}),
            delete: ([ $c[] | select(.[0] == $n and .[2] == "delete") ] | length),
            deleted: ([ $d[] | select(.[0] == $n and .[2] == "deleted") ] | length),
            gone: ([ $d[] | select(.[0] == $n and .[2] == "gone") ] | length),
            failed: ([ $d[] | select(.[0] == $n and .[2] == "failed") ] | length),
            unclassified: ([ $c[] | select(.[0] == $n and .[2] == "unclassified") ] | length),
            index_unreadable: ([ $u[] | select(.[0] == $n) ] | length) } } ]
    | from_entries')
LIVE_JSON=$(jq -n -R -c --rawfile live "$TMP/live.txt" '$live | split("\n") | map(select(length > 0))')
DELETED_TOTAL=$(awk -F'\t' '$3 == "deleted"' "$TMP/deleted.tsv" | grep -c . || true)
PLANNED_TOTAL=$(awk -F'\t' '$3 == "delete"' "$TMP/classes.tsv" | grep -c . || true)
jq -n -c \
    --arg verb "$ME" --argjson dry "$( [ "$DRY" = 1 ] && echo true || echo false )" \
    --arg registry "$REG_HOST" --arg owner "$OWNER" --arg pkgs "$PACKAGES" \
    --arg live_main "$LIVE_MAIN" --argjson live "$LIVE_JSON" --arg stamp "$STAMP" \
    --argjson landed "$LANDED_COUNT" --argjson keep_trains "$KEEP_TRAINS" --argjson lookback "$LOOKBACK" \
    --argjson hours "$KEEP_HOURS" --argjson listed "$LISTED" --arg total "${TOTAL:-}" \
    --argjson packages "$PACKAGES_JSON" --argjson planned "$PLANNED_TOTAL" --argjson deleted "$DELETED_TOTAL" \
    --arg write_scope "$WRITE_SCOPE" --arg refused "$REFUSED_SCOPE" --arg fail "$FAIL_CODE" \
    --argjson df_before "$DF_BEFORE" --argjson df_after "$DF_AFTER" --arg df_path "$DF_PATH" \
    --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" '
    { verb: $verb, dry_run: $dry, registry: $registry, owner: $owner, packages_read: ($pkgs | split(" ")),
      keep: { live_main: $live_main, live: $live, stamp: $stamp, landed_trains: $landed,
              keep_trains: $keep_trains, trains_read: $lookback, latest: true, newer_than_hours: $hours },
      listed: $listed, forge_total: (if $total == "" then null else ($total | tonumber? // $total) end),
      packages: $packages, planned: $planned, deleted: $deleted,
      declared_bytes: null,
      bytes_note: "the packages API declares no sizes (files[].size is null, Forgejo 16.0.2); blobs are freed by Forgejo cleanup once unreferenced — read df on the packet",
      disk_path: $df_path, disk_avail_kb_before: $df_before, disk_avail_kb_after: $df_after,
      write_scope: $write_scope }
    + (if $refused == "" then {} else { refused: $refused } end)
    + (if $fail == "" then {} else { failed_http: $fail } end)
    + { at: $at }'

if [ "$DRY" = 1 ]; then
    say "DRY RUN — would delete $PLANNED_TOTAL version(s) across $PACKAGES; the keep set held $(grep -c . "$TMP/keys.tsv" || true) sha(s), latest, and $KEEP_HOURS h. Nothing was deleted."
    exit 0
fi
if [ -n "$REFUSED_SCOPE" ]; then
    say "  $DELETED_TOTAL of $PLANNED_TOTAL deleted before the refusal. Nothing was deleted."
    exit 2
fi
if [ -n "$FAIL_CODE" ]; then
    say "  deleted $DELETED_TOTAL of $PLANNED_TOTAL planned before the failure (HTTP $FAIL_CODE); the rest stay for the next pass."
    exit 1
fi
say "OK — deleted $DELETED_TOTAL of $PLANNED_TOTAL planned version(s) across $PACKAGES; df $DF_PATH before ${DF_BEFORE} KB, after ${DF_AFTER} KB free. The blobs are Forgejo's to free (its cleanup cron), so the difference lands later; read df on this packet, not a claim here."
exit 0
