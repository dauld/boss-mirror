#!/usr/bin/env bash
#
# install-cli-from-image — put the tree's `boss` CLI on this host, taken
# out of the cluster image built for the commit the host converged to.
#
# ONE INSTALLER FOR EVERY MANAGED HOST (backlog 9f00a805, consolidation
# H8, car 1, 2026-09-18). Written for boss-gcp under infra/gcp/, it
# lives here — beside roles.toml and node-roles.sh, what a host reads
# to know what it is for — because the forge needed the same thing.
# Measured on #448: infra/forge/*.sh is 32 scripts, 9,790 lines, the
# largest of them shell twins of CLI verbs (run-car-probe.sh for `boss
# prove --from-car`, tenant-census.sh for `boss tenant`, …), each with
# its own pin, for the one reason that the forge had no `boss` binary.
# Two callers now: infra/gcp/boss-gcp-converge.sh after its units, and
# infra/forge/install.sh for the `cluster-operator` role. The twins
# retire one verb at a time in the cars after this one.
#
# WHY THIS EXISTS (backlog 6f58e9a1; David chose option (b), 2026-09-15).
# boss-gcp's /usr/local/bin/boss had NO refresh path. Only a human
# `deploy-services.sh prod` — a full deploy of the second, older stack
# that 45641c91 retires — ever installed it, and boss-gcp-converge
# installed units only. Measured 2026-09-15 20:5xZ, ops-request 20ba7cdf:
# "REFUSED — /usr/local/bin/boss cannot say what it was built from
# (boss 0.1.0 names no commit)". That binary was older than built_from
# itself, so every host verb that shells to the CLI (publish-workflow
# today; any registry-writing verb tomorrow; the Drift tab's Approve
# through the same door) refused 78 by name, and the 17 adrift workflow
# kinds stayed unpublished — 33 fields, all tree-ahead.
#
# THE IMAGE IS THE BUILD. cluster-deploy-runner.sh builds
# `<registry>/david/boss:<short sha>` for every train that lands and
# copies every `boss`/`boss-*` release binary into it (infra/oss-
# quickstart/Dockerfile, stage 4). So "the CLI at converge_sha" already
# exists, built once, by the same pipeline that runs the cluster; this
# script takes /usr/local/bin/boss out of that image. The CLI on this
# host is then the tree's CLI BY CONSTRUCTION — the rule the pod's boss
# shim applies — and "which CLI does boss-gcp run" becomes a read on the
# converge packet (`cli_sha` beside `converge_sha`), not a question for
# a human with ssh.
#
# NO DOCKER. The first build of this step (car c142457c, held) shelled
# to `docker pull` / `docker create` / `docker cp`; measured 2026-09-16
# 00:30Z, ops-request 546c13fc: boss-gcp has no docker (docker.service
# not found), and David has said the host gets none. An OCI registry is
# HTTP + JSON + tar, so the pull is curl and tar, and no daemon, no
# login and no daemon.json are involved:
#
#   GET /v2/                       401 + Www-Authenticate: Bearer realm=
#                                  "<realm>",service="<service>"
#   GET <realm>?service=…&scope=repository:<repo>:pull
#                                  {"token": …} — no credentials: the
#                                  forge package david/boss is PUBLIC,
#                                  verified from the pod 2026-09-16
#   GET /v2/<repo>/manifests/<tag> an OCI index (one manifest per
#                                  platform + an attestation) or a bare
#                                  manifest; the platform's manifest is
#                                  fetched by digest and lists the layers
#   GET /v2/<repo>/blobs/<digest>  each layer, a gzip tar
#
# THE TAG IS THE SHORT SHA; THE FULL SHA IS THE ATTESTATION. The deploy
# runner tags with `git rev-parse --short` (cluster-deploy-runner.sh:
# "the short tag stays the image name, the full sha is the attestation")
# and the registry's tag list is 7-char shas. The first build pulled
# `<repo>:<full sha>` and refused anything shorter, so every pull would
# have been a 404 — measured 2026-09-16 00:30Z against tags/list. Now
# the tag is ${SHA:0:7}; the FULL sha is what this script is handed,
# what the generation directory is named, what the wrapper hands the
# binary as BOSS_BUILD_COMMIT, and what `boss --version` must print.
#
# LAYERS ARE READ THE WAY A RUNTIME READS THEM. A file's final state is
# the TOPMOST layer that mentions it: a later layer's copy replaces an
# earlier one, and a whiteout (`.wh.<name>` beside it, or an opaque
# `.wh..wh.opq` in it or any parent directory) deletes it. So the layers
# are walked from the top down and the first one that mentions
# usr/local/bin/boss decides: a regular member is extracted; a whiteout
# is a refusal ("the image deletes it"). Every blob is verified against
# its manifest digest (sha256, the whole file) BEFORE tar reads a byte
# of it, and the platform manifest is verified against the index the
# same way. A layer that is not gzip tar is refused by media type.
#
# THE SHA RIDES THE PATH, AND THE WRAPPER CARRIES IT TO THE BINARY. The
# image's binary is compiled with BOSS_CLI_BUILT_FROM=unknown and reads
# BOSS_BUILD_COMMIT at runtime (built_from.rs); copied out and run bare
# it prints `built from unknown`, which the floor check refuses. So:
#
#   <store>/<sha>/boss     the binary, one directory per generation
#   <store>/current        -> <sha>, flipped atomically (make-before-break,
#                          the previous generation stays as the revert)
#   <store>/boss           boss-cli-wrapper.sh: execs current/boss with
#                          BOSS_BUILD_COMMIT=<basename of current>
#   /usr/local/bin/boss    -> <store>/boss
#
# The sha is defined ONCE, as the generation directory's name; the
# wrapper reads it from there. No sidecar, no text with the sha baked
# in (CLAUDE.md §9a).
#
# A TRAIN WITHOUT A RUST CHANGE ships an image whose binaries are older
# than its tag — the Dockerfile says so — and that is fine here: the tag
# sha IS the tree sha, BOSS_BUILD_COMMIT is what built_from reports, and
# the floor check asks "was this built from a tree at or after the
# loader landed", which the tree sha answers truthfully.
#
# CONFIRMED, OR NOT INSTALLED. The staged binary is run with the sha in
# its environment and must print `built from <sha>`; then, after the
# flip, the host's own path (/usr/local/bin/boss --version) must print
# it again. Anything else is NOT CONFIRMED: the staging directory is
# removed, `current` stays where it was, the reason rides the packet
# (cli_result) and the exit is non-zero. Mostly sure is a feeling; the
# --version line is the artifact.
#
# EVERY REFUSAL NAMES THE URL AND THE HTTP CODE. Each response body is
# captured to a file and printed whole when the request is not what was
# expected (CLAUDE.md §Diagnosis: a digest or a tail throws away the
# only copy). The one failure that heals by itself is a tag the deploy
# runner has not built yet — it builds the image a few minutes after
# each train lands — which the next converge tick picks up. That one is
# NOT a refusal: it exits 75 with `cli_result` = `not yet: …`, so the
# caller can tell a wait from a fault. On the forge the converge that
# installs the CLI runs on the host that builds the image, ten minutes
# apart, so the first tick after every train lands in this state; a
# red there would be a red on every train (infra/forge/install.sh
# waits; boss-gcp's converge, every half hour, still reds and heals).
#
# IDEMPOTENT AND CHEAP WHEN NOTHING MOVED: a tick whose `current`
# already names the sha re-verifies through the wrapper and fetches
# nothing. Generations are pruned to KEEP; blobs never stay on disk.
#
# USAGE
#   install-cli-from-image.sh <full sha>
#
# ENV
#   BOSS_CLI_IMAGE_REPO   the image repository (default the converge's
#                         REGISTRY from infra/forge/forge-defaults.sh, off
#                         the registry host in /etc/boss/sor.env);
#                         "<host>/<name>"
#   BOSS_CLI_REGISTRY_SCHEME  http (default — the forge registry is plain
#                         HTTP on the LAN) or https
#   BOSS_CLI_PLATFORM     "<arch>/<os>" of the manifest to take from a
#                         multi-platform index (default from uname -m:
#                         amd64/linux on x86_64, arm64/linux on aarch64)
#   BOSS_CLI_STORE        the generation store (default /opt/boss-cli)
#   BOSS_CLI_LINK         the host path (default /usr/local/bin/boss)
#   BOSS_CLI_WRAPPER_SRC  the wrapper to install (default the
#                         boss-cli-wrapper.sh beside this script)
#   BOSS_CLI_KEEP         generations kept on disk (default 3)
#   BOSS_CLI_PULL_TIMEOUT seconds the WHOLE pull may take, every request
#                         together (default 360) — the converge unit's
#                         ceiling is 10min for the whole run, and a pull
#                         that hangs must be a named failure on the
#                         packet, not a unit timeout
#   BOSS_RUN_SUMMARY_FILE where cli_sha / cli_result / cli_action /
#                         cli_image land (infra/run-summary.sh)
#
# EXIT
#   0  the CLI at <sha> is confirmed on the host path
#   1  refused / not confirmed — cli_result on the packet says which
#   2  usage
#   75 not yet — the registry has no image for <sha>'s tag (the deploy
#      runner has not built it); nothing changed, the next tick retries
set -uo pipefail

NAME="install-cli-from-image"
SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=infra/run-summary.sh
. "$SELF_DIR/../run-summary.sh"
# gen_atomic_link — the one definition of "flip a symlink without a
# window where it is absent" (infra/generation.sh). The store variables
# it sets default to the retired prod store and are not used here.
# shellcheck source=infra/generation.sh
. "$SELF_DIR/../generation.sh"

SHA="${1:-}"
case "$SHA" in
    *[!0-9a-f]*|"") echo "usage: $(basename "$0") <full sha>" >&2; exit 2 ;;
esac
[ "${#SHA}" -eq 40 ] || { echo "usage: $(basename "$0") <full sha> — got '${SHA}' (${#SHA} chars; the full commit is the attestation, the image tag is derived from it)" >&2; exit 2; }

if [ -z "${BOSS_CLI_IMAGE_REPO:-}" ]; then
    # shellcheck source=infra/forge/forge-defaults.sh
    . "$SELF_DIR/../forge/forge-defaults.sh"
    forge_need REGISTRY
    BOSS_CLI_IMAGE_REPO="$REGISTRY"
fi
IMAGE_REPO="$BOSS_CLI_IMAGE_REPO"
SCHEME="${BOSS_CLI_REGISTRY_SCHEME:-http}"
STORE="${BOSS_CLI_STORE:-/opt/boss-cli}"
LINK="${BOSS_CLI_LINK:-/usr/local/bin/boss}"
WRAPPER_SRC="${BOSS_CLI_WRAPPER_SRC:-$SELF_DIR/boss-cli-wrapper.sh}"
KEEP="${BOSS_CLI_KEEP:-3}"
PULL_TIMEOUT="${BOSS_CLI_PULL_TIMEOUT:-360}"
# The deploy runner's tag: the first seven characters of the merge's
# full sha (cluster-deploy-runner.sh, HEAD=${HEAD_FULL:0:7} — a fixed
# substring, never `git rev-parse --short`, whose length moves with the
# repository; a test pins both). The full sha is the attestation.
TAG="${SHA:0:7}"
IMAGE="$IMAGE_REPO:$TAG"
REGISTRY_HOST="${IMAGE_REPO%%/*}"
REPO_PATH="${IMAGE_REPO#*/}"
REGISTRY_URL="$SCHEME://$REGISTRY_HOST"
MEMBER="usr/local/bin/boss"
case "${BOSS_CLI_PLATFORM:-}" in
    "") case "$(uname -m 2>/dev/null)" in
            x86_64) PLATFORM="amd64/linux" ;;
            aarch64|arm64) PLATFORM="arm64/linux" ;;
            *) PLATFORM="amd64/linux" ;;
        esac ;;
    *) PLATFORM="$BOSS_CLI_PLATFORM" ;;
esac

say() { printf '%s: %s\n' "$NAME" "$*"; }

# Each fact on the packet as soon as it is true (run-summary.sh's rule):
# the sha this run is FOR, first, so a run that dies still says what it
# was trying to install.
run_summary_field cli_sha "$SHA"
run_summary_field cli_image "$IMAGE"

# One exit path for every verdict that is not "ok": the packet carries
# the verdict, the journal carries the paragraph, and `current` is
# whatever it was. A `not yet:` verdict exits 75 — the wait, not the
# fault — and every other exits 1.
refuse() { # <cli_result> <message...>
    local result="$1"; shift
    printf '%s: %s — %s\n    %s\n' "$NAME" "$(printf '%s' "${result%%:*}" | tr '[:lower:]' '[:upper:]')" "${result#*: }" "$*" >&2
    run_summary_field cli_result "$result"
    run_summary_note "$NAME: $result — $*"
    [ -n "${stage:-}" ] && rm -rf "$stage"
    case "$result" in "not yet:"*) exit 75 ;; esac
    exit 1
}

# `boss --version` prints `boss <crate> built from <sha>` (built_from.rs).
# Captured whole; first line taken in the shell — an external producer
# piped into an early-exiting reader is the SIGPIPE coin-flip.
version_of() { # <argv...> -> prints the first line, returns its status
    local line
    line="$("$@" 2>&1)"
    local rc=$?
    printf '%s\n' "${line%%$'\n'*}"
    return $rc
}

confirmed() { # <line> -> 0 iff it names this sha
    case "$1" in *"built from $SHA"*) return 0 ;; esac
    return 1
}

mkdir -p "$STORE" || refuse "failed: cannot create $STORE" "the generation store could not be created"

# --- the wrapper, current with the tree -------------------------------------
# Copied, never linked: /opt/boss is a checkout the converge moves, and
# a link into it would make `boss` change bytes mid-fast-forward.
[ -f "$WRAPPER_SRC" ] || refuse "failed: no wrapper at $WRAPPER_SRC" "boss-cli-wrapper.sh must ship beside this script"
if ! cmp -s "$WRAPPER_SRC" "$STORE/boss"; then
    tmp="$STORE/.boss.tmp.$$"
    cp "$WRAPPER_SRC" "$tmp" && chmod 0755 "$tmp" && mv -Tf "$tmp" "$STORE/boss" \
        || { rm -f "$tmp"; refuse "failed: cannot install the wrapper into $STORE" "copy failed"; }
    say "wrapper installed at $STORE/boss"
fi

# --- the registry, over HTTP ------------------------------------------------
# One request shape for everything, so a failure always has the same
# three facts beside it: the URL, the HTTP code, and the body in full.
# The whole pull shares ONE deadline (PULL_TIMEOUT from the first
# request), so twenty-four layers cannot each take the full allowance.
DEADLINE=0
TOKEN=""
http_get() { # <url> <out-file> <headers-file> <accept|""> -> prints the http code; 0 iff curl ran
    local url="$1" out="$2" hdrs="$3" accept="${4:-}"
    local left=$((DEADLINE - SECONDS))
    [ "$left" -gt 0 ] || { printf '000\n'; echo "curl: the pull's ${PULL_TIMEOUT}s budget is spent before $url" >&2; return 28; }
    local -a args=(-sS -o "$out" -D "$hdrs" -w '%{http_code}' --max-time "$left" -L)
    [ -n "$accept" ] && args+=(-H "Accept: $accept")
    [ -n "$TOKEN" ] && args+=(-H "Authorization: Bearer $TOKEN")
    curl "${args[@]}" "$url"
}

# Refuse with the request named and its body printed whole.
refuse_http() { # <what> <url> <code> <curl-rc> <body-file> <hint>
    local what="$1" url="$2" code="$3" rc="$4" body="$5" hint="$6"
    if [ "$rc" -ne 0 ]; then
        echo "$NAME: $what — curl exited $rc for $url (HTTP $code); curl's message is above." >&2
        [ "$rc" -eq 28 ] && echo "    (the whole pull is bounded by BOSS_CLI_PULL_TIMEOUT=${PULL_TIMEOUT}s)" >&2
        refuse "refused: $what — curl exit $rc for $url" "$hint"
    fi
    echo "$NAME: $what — $url answered HTTP $code; the body in full:" >&2
    sed 's/^/    body: /' "$body" >&2
    refuse "refused: $what — HTTP $code from $url" "$hint"
}

# sha256 of a file, as "sha256:<hex>" — the digest form the manifest uses.
digest_of() { printf 'sha256:%s\n' "$(sha256sum "$1" | cut -d' ' -f1)"; }

# --- the generation ---------------------------------------------------------
# What `current` names before this run touches anything: the revert
# target, and what every refusal below leaves in place.
prev="$(readlink "$STORE/current" 2>/dev/null || true)"
action=""
stage=""
if [ -x "$STORE/$SHA/boss" ]; then
    # Already on disk: a previous tick installed (and confirmed) it, or
    # `current` was moved away by hand. Re-link below; never re-pull.
    if [ "$(readlink "$STORE/current" 2>/dev/null)" = "$SHA" ]; then
        action="unchanged"
    else
        action="relinked"
    fi
else
    for tool in curl jq tar gzip sha256sum; do
        command -v "$tool" >/dev/null 2>&1 \
            || refuse "refused: no $tool on this host" "the CLI is taken out of the cluster image $IMAGE over HTTP with curl, jq, tar, gzip and sha256sum — no docker (boss-gcp has none, ops-request 546c13fc) — and $tool is not on PATH"
    done

    # Staging is not yet a generation. Every request's headers and body
    # land under pull/ (printed whole on a refusal, gone once the binary
    # is out); the generation keeps the binary and the manifest that
    # named its layers.
    stage="$STORE/.staging-$SHA"
    pull="$stage/pull"
    rm -rf "$stage"
    mkdir -p "$pull" || { stage=""; refuse "failed: cannot create $STORE/.staging-$SHA" "staging"; }
    DEADLINE=$((SECONDS + PULL_TIMEOUT))
    say "pulling $MEMBER out of $IMAGE for $SHA (platform $PLATFORM, timeout ${PULL_TIMEOUT}s)"

    # 1. The token. The registry answers /v2/ with the realm and service
    #    of its token endpoint; asking that endpoint for a pull scope
    #    with NO credentials returns a token for a public package.
    url="$REGISTRY_URL/v2/"
    code="$(http_get "$url" "$pull/ping.body" "$pull/ping.hdr")"; rc=$?
    case "$rc:$code" in
        0:200) say "$url answered 200 without a token; pulling anonymously" ;;
        0:401)
            # Drained whole, first line taken in the shell (an early-
            # exiting reader under pipefail is the SIGPIPE coin-flip).
            auth="$(tr -d '\r' <"$pull/ping.hdr" | sed -n 's/^[Ww][Ww][Ww]-[Aa]uthenticate: *//p')"
            auth="${auth%%$'\n'*}"
            realm="$(printf '%s' "$auth" | sed -n 's/.*realm="\([^"]*\)".*/\1/p')"
            service="$(printf '%s' "$auth" | sed -n 's/.*service="\([^"]*\)".*/\1/p')"
            [ -n "$realm" ] || refuse_http "the registry's 401 names no token realm" "$url" "$code" 0 "$pull/ping.body" "Www-Authenticate was '$auth'"
            turl="$realm?service=$service&scope=repository:$REPO_PATH:pull"
            code="$(http_get "$turl" "$pull/token.json" "$pull/token.hdr")"; rc=$?
            [ "$rc" -eq 0 ] && [ "$code" = 200 ] \
                || refuse_http "the anonymous pull token for $IMAGE_REPO could not be fetched" "$turl" "$code" "$rc" "$pull/token.json" "the forge package $REPO_PATH is public and its token endpoint hands out a pull token with no credentials (verified from the pod 2026-09-16); a non-200 here is the forge or the LAN, not this host. Nothing was installed; $LINK is whatever the previous converge confirmed."
            TOKEN="$(jq -r '.token // .access_token // empty' "$pull/token.json")"
            [ -n "$TOKEN" ] || refuse_http "the token endpoint answered 200 without a token" "$turl" "$code" 0 "$pull/token.json" "expected {\"token\": …}"
            ;;
        *) refuse_http "the registry did not answer" "$url" "$code" "$rc" "$pull/ping.body" "the forge registry at $REGISTRY_URL is unreachable from this host, or answered something other than 200/401. Nothing was installed; $LINK is whatever the previous converge confirmed." ;;
    esac

    # 2. The tag's manifest — an index (one manifest per platform, plus
    #    buildx's attestation) or, for a single-platform push, the
    #    manifest itself.
    accept='application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json, application/vnd.docker.distribution.manifest.list.v2+json'
    murl="$REGISTRY_URL/v2/$REPO_PATH/manifests/$TAG"
    code="$(http_get "$murl" "$pull/tag.json" "$pull/tag.hdr" "$accept")"; rc=$?
    if [ "$rc" -eq 0 ] && [ "$code" = 404 ]; then
        # The registry answered, and has no such tag: the deploy runner
        # has not built the image for this commit yet. The body is
        # printed whole like any other answer; the verdict is a wait.
        echo "$NAME: no image $IMAGE yet — $murl answered HTTP 404; the body in full:" >&2
        sed 's/^/    body: /' "$pull/tag.json" >&2
        refuse "not yet: no image $IMAGE — HTTP 404 from $murl" "the tag is the deploy runner's short sha of the train that landed (cluster-deploy-runner.sh), and it builds the image a few minutes after each train; the next converge tick retries. Nothing was installed; $LINK is whatever the previous converge confirmed."
    fi
    if [ "$rc" -ne 0 ] || [ "$code" != 200 ]; then
        hint="the tag is the deploy runner's short sha of the train that landed (cluster-deploy-runner.sh); a 404 is answered above as not yet, so this is the forge or the LAN. Nothing was installed; $LINK is whatever the previous converge confirmed."
        refuse_http "no image $IMAGE" "$murl" "$code" "$rc" "$pull/tag.json" "$hint"
    fi
    kind="$(jq -r '.mediaType // empty' "$pull/tag.json")"
    if [ -z "$kind" ]; then
        kind="$(tr -d '\r' <"$pull/tag.hdr" | sed -n 's/^[Cc]ontent-[Tt]ype: *//p')"
        kind="${kind%%$'\n'*}"
    fi
    case "$kind" in
        application/vnd.oci.image.index.v1+json|application/vnd.docker.distribution.manifest.list.v2+json)
            arch="${PLATFORM%%/*}"; os="${PLATFORM#*/}"
            mdigest="$(jq -r --arg a "$arch" --arg o "$os" '[.manifests[] | select(.platform.architecture == $a and .platform.os == $o)] | .[0].digest // empty' "$pull/tag.json")"
            [ -n "$mdigest" ] || { echo "$NAME: the index at $murl lists no $PLATFORM manifest; the index in full:" >&2; sed 's/^/    index: /' "$pull/tag.json" >&2; refuse "refused: $IMAGE has no $PLATFORM manifest" "the platforms it lists are: $(jq -r '[.manifests[].platform | "\(.architecture)/\(.os)"] | join(", ")' "$pull/tag.json")"; }
            durl="$REGISTRY_URL/v2/$REPO_PATH/manifests/$mdigest"
            code="$(http_get "$durl" "$pull/manifest.json" "$pull/manifest.hdr" 'application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json')"; rc=$?
            [ "$rc" -eq 0 ] && [ "$code" = 200 ] \
                || refuse_http "the $PLATFORM manifest of $IMAGE could not be fetched" "$durl" "$code" "$rc" "$pull/manifest.json" "the index at $murl named it"
            got="$(digest_of "$pull/manifest.json")"
            [ "$got" = "$mdigest" ] \
                || refuse "refused: manifest digest mismatch for $IMAGE" "the index at $murl names $mdigest for the $PLATFORM manifest and the bytes fetched from $durl hash to $got; nothing from this image is trusted."
            ;;
        application/vnd.oci.image.manifest.v1+json|application/vnd.docker.distribution.manifest.v2+json)
            mv -f "$pull/tag.json" "$pull/manifest.json"
            ;;
        *) refuse_http "the manifest of $IMAGE has an unknown media type '$kind'" "$murl" "$code" 0 "$pull/tag.json" "this script reads OCI indexes/manifests and docker v2 manifests/lists" ;;
    esac
    nlayers="$(jq -r '.layers | length' "$pull/manifest.json" 2>/dev/null)"
    case "${nlayers:-empty}" in empty|0|*[!0-9]*) refuse "refused: the manifest of $IMAGE lists no layers" "manifest at $pull/manifest.json: $(head -c 400 "$pull/manifest.json")" ;; esac

    # 3. The layers, TOP DOWN. The first one that mentions the member
    #    decides — a whiteout in a higher layer means a lower layer's
    #    copy is deleted, so it must be seen first. Every blob is
    #    verified against its digest before tar reads it.
    found=""
    i=$((nlayers - 1))
    while [ "$i" -ge 0 ]; do
        ldigest="$(jq -r ".layers[$i].digest" "$pull/manifest.json")"
        ltype="$(jq -r ".layers[$i].mediaType" "$pull/manifest.json")"
        lsize="$(jq -r ".layers[$i].size" "$pull/manifest.json")"
        case "$ltype" in
            application/vnd.oci.image.layer.v1.tar+gzip|application/vnd.docker.image.rootfs.diff.tar.gzip) ;;
            *) refuse "refused: layer $i of $IMAGE is '$ltype', not a gzip tar" "this script reads gzip tar layers only (docker buildx writes those); a +zstd or unknown layer needs a different reader, not a guess. Nothing was installed." ;;
        esac
        burl="$REGISTRY_URL/v2/$REPO_PATH/blobs/$ldigest"
        blob="$pull/layer.blob"
        code="$(http_get "$burl" "$blob" "$pull/layer.hdr")"; rc=$?
        [ "$rc" -eq 0 ] && [ "$code" = 200 ] \
            || refuse_http "layer $i of $IMAGE ($lsize bytes) could not be fetched" "$burl" "$code" "$rc" "$blob" "the manifest names it; nothing was installed."
        got="$(digest_of "$blob")"
        [ "$got" = "$ldigest" ] \
            || refuse "refused: blob digest mismatch on layer $i of $IMAGE" "the manifest names $ldigest ($lsize bytes) and the $(stat -c %s "$blob" 2>/dev/null || wc -c <"$blob") bytes fetched from $burl hash to $got; nothing from this image is trusted and nothing was installed."
        # The member list, whole, then read in the shell: an early-exiting
        # grep on a 230 MB tar listing is the SIGPIPE coin-flip.
        if ! tar -tzf "$blob" >"$pull/layer.list" 2>"$pull/layer.err"; then
            refuse "refused: layer $i of $IMAGE is not a readable gzip tar" "tar said: $(tr '\n' ' ' <"$pull/layer.err")"
        fi
        if grep -qx -- "$MEMBER" "$pull/layer.list"; then
            found="$i"
            say "layer $i of $nlayers ($ldigest, $lsize bytes) carries $MEMBER; digest verified"
            tar -xzf "$blob" -C "$pull" -- "$MEMBER" 2>"$pull/layer.err" \
                || refuse "refused: tar could not extract $MEMBER from layer $i of $IMAGE" "tar said: $(tr '\n' ' ' <"$pull/layer.err")"
            [ -f "$pull/$MEMBER" ] && [ ! -L "$pull/$MEMBER" ] \
                || refuse "refused: layer $i of $IMAGE lists $MEMBER but tar extracted no regular file" "a symlink or directory at that path is not a CLI"
            mv -f "$pull/$MEMBER" "$stage/boss" && mv -f "$pull/manifest.json" "$stage/manifest.json" \
                || refuse "failed: cannot move the extracted binary into $stage" "rename failed"
            rm -rf "$pull"
            break
        fi
        # Not in this layer. A whiteout here for the member, or an opaque
        # whiteout on its directory or any parent, deletes every lower
        # layer's copy (OCI image-layer spec: whiteouts apply to lower
        # layers only — which is why a layer that carries the member is
        # decided above, before this check). The runtime would not see
        # the file, so neither may this host.
        wh="$(grep -m 1 -x -e "$(dirname "$MEMBER")/.wh.$(basename "$MEMBER")" \
                      -e "$(dirname "$MEMBER")/.wh..wh.opq" \
                      -e "usr/local/.wh.bin" -e "usr/local/.wh..wh.opq" \
                      -e "usr/.wh.local" -e "usr/.wh..wh.opq" \
                      -e ".wh.usr" -e ".wh..wh.opq" "$pull/layer.list")"
        if [ -n "$wh" ]; then
            refuse "refused: $IMAGE deletes $MEMBER (whiteout $wh in layer $i)" "a layer above every copy of the CLI removes it, so the image has no $MEMBER at runtime and this host installs none; nothing was installed."
        fi
        rm -f "$blob" "$pull/layer.list"
        i=$((i - 1))
    done
    [ -n "$found" ] || refuse "refused: no layer of $IMAGE carries $MEMBER" "$nlayers layers were listed and none has the CLI; the image is not one the Dockerfile's stage 4 built. Nothing was installed."
    chmod 0755 "$stage/boss"

    # CONFIRM THE STAGED BINARY BEFORE IT BECOMES A GENERATION: run it
    # the way the wrapper will, and read what it says it was built from.
    line="$(version_of env BOSS_BUILD_COMMIT="$SHA" "$stage/boss" --version)" || true
    if ! confirmed "$line"; then
        refuse "unconfirmed: the binary out of $IMAGE says '$line', not 'built from $SHA'" \
            "NOT CONFIRMED. With BOSS_BUILD_COMMIT=$SHA in its environment the extracted boss printed '$line'. A binary that does not name the commit it was installed for is not installed: the staging copy is removed and $STORE/current stays at ${prev:-none}."
    fi
    # A directory for this sha with no runnable boss in it (a run that
    # died mid-way) is not a generation; the confirmed staging replaces it.
    rm -rf "${STORE:?}/$SHA"
    mv -T "$stage" "$STORE/$SHA" || refuse "failed: cannot move $stage into place" "rename failed"
    stage=""
    action="installed"
fi

# --- flip, link, and confirm through the host's own path --------------------
if [ "$prev" != "$SHA" ]; then
    gen_atomic_link "$SHA" "$STORE/current"
fi
# The host path is re-pointed only now, with a confirmed generation
# behind it: a host still carrying the old real-file binary keeps it
# until there is something better to run (make before break).
if [ "$(readlink "$LINK" 2>/dev/null || true)" != "$STORE/boss" ]; then
    mkdir -p "$(dirname "$LINK")"
    gen_atomic_link "$STORE/boss" "$LINK"
    say "$LINK -> $STORE/boss"
fi
line="$(version_of "$LINK" --version)" || true
if ! confirmed "$line"; then
    # The staged check passed and the wrapper does not agree: a wrapper
    # or link defect. Put `current` back where it was and say so.
    if [ -n "$prev" ] && [ "$prev" != "$SHA" ]; then
        gen_atomic_link "$prev" "$STORE/current"
    fi
    refuse "unconfirmed: $LINK --version says '$line', not 'built from $SHA'" \
        "NOT CONFIRMED through the host path. The generation at $STORE/$SHA answered correctly when run directly, so the wrapper ($STORE/boss) or the link ($LINK) is at fault; current is back at ${prev:-none}."
fi

# --- prune ------------------------------------------------------------------
# Newest KEEP generations by mtime stay; `current` is never pruned.
ls -1t "$STORE" 2>/dev/null | grep -x '[0-9a-f]\{40\}' | tail -n +"$((KEEP + 1))" \
    | while IFS= read -r old; do
        [ "$old" = "$SHA" ] && continue
        rm -rf "${STORE:?}/$old" && say "pruned generation ${old:0:8}"
    done

run_summary_field cli_result ok
run_summary_field cli_action "$action"
say "CONFIRMED — $LINK is the tree's CLI at ${SHA:0:8} ($action, image $IMAGE): $line"
exit 0
