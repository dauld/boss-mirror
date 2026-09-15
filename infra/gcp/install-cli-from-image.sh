#!/usr/bin/env bash
#
# install-cli-from-image — put the tree's `boss` CLI on this host, taken
# out of the cluster image built for the commit the host converged to.
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
# `<registry>/david/boss:<sha>` for every train that lands and copies
# every `boss`/`boss-*` release binary into it (infra/oss-quickstart/
# Dockerfile, stage 4). So "the CLI at converge_sha" already exists,
# built once, by the same pipeline that runs the cluster; this script
# pulls that image and copies /usr/local/bin/boss out of it. The CLI on
# this host is then the tree's CLI BY CONSTRUCTION — the rule the pod's
# boss shim applies — and "which CLI does boss-gcp run" becomes a read
# on the converge packet (`cli_sha` beside `converge_sha`), not a
# question for a human with ssh.
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
# WHAT A PULL NEEDS THAT THIS SCRIPT CANNOT INSTALL — named in the
# refusal when the pull fails, with docker's complete output above it:
# the daemon must trust the plain-HTTP forge registry
# (/etc/docker/daemon.json `insecure-registries`), and if the package
# is private, root's docker must be logged in to it with a token scoped
# read:package (`docker login <registry>` — David does credential
# admin; the cluster's own pull uses the forgejo-registry secret). The
# converge picks the CLI step up on its next tick once either lands.
#
# IDEMPOTENT AND CHEAP WHEN NOTHING MOVED: a tick whose `current`
# already names the sha re-verifies through the wrapper and pulls
# nothing. The image is removed after extraction except the newest,
# whose layers the next pull shares; generations are pruned to KEEP.
#
# USAGE
#   install-cli-from-image.sh <full sha>
#
# ENV
#   BOSS_CLI_IMAGE_REPO   the image repository (default the forge's
#                         10.20.0.15:3000/david/boss — cluster-deploy-
#                         runner.sh's REGISTRY)
#   BOSS_CLI_STORE        the generation store (default /opt/boss-cli)
#   BOSS_CLI_LINK         the host path (default /usr/local/bin/boss)
#   BOSS_CLI_WRAPPER_SRC  the wrapper to install (default the
#                         boss-cli-wrapper.sh beside this script)
#   BOSS_CLI_KEEP         generations kept on disk (default 3)
#   BOSS_CLI_PULL_TIMEOUT seconds a pull may take (default 360) — the
#                         converge unit's ceiling is 10min for the whole
#                         run, and a pull that hangs must be a named
#                         failure on the packet, not a unit timeout
#   BOSS_RUN_SUMMARY_FILE where cli_sha / cli_result / cli_action /
#                         cli_image land (infra/run-summary.sh)
#
# EXIT
#   0  the CLI at <sha> is confirmed on the host path
#   1  refused / not confirmed — cli_result on the packet says which
#   2  usage
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
[ "${#SHA}" -eq 40 ] || { echo "usage: $(basename "$0") <full sha> — got '${SHA}' (${#SHA} chars; the image tag is the full commit)" >&2; exit 2; }

IMAGE_REPO="${BOSS_CLI_IMAGE_REPO:-10.20.0.15:3000/david/boss}"
STORE="${BOSS_CLI_STORE:-/opt/boss-cli}"
LINK="${BOSS_CLI_LINK:-/usr/local/bin/boss}"
WRAPPER_SRC="${BOSS_CLI_WRAPPER_SRC:-$SELF_DIR/boss-cli-wrapper.sh}"
KEEP="${BOSS_CLI_KEEP:-3}"
PULL_TIMEOUT="${BOSS_CLI_PULL_TIMEOUT:-360}"
IMAGE="$IMAGE_REPO:$SHA"
REGISTRY_HOST="${IMAGE_REPO%%/*}"

say() { printf '%s: %s\n' "$NAME" "$*"; }

# Each fact on the packet as soon as it is true (run-summary.sh's rule):
# the sha this run is FOR, first, so a run that dies still says what it
# was trying to install.
run_summary_field cli_sha "$SHA"
run_summary_field cli_image "$IMAGE"

# One exit path for every verdict that is not "ok": the packet carries
# the verdict, the journal carries the paragraph, and `current` is
# whatever it was.
refuse() { # <cli_result> <message...>
    local result="$1"; shift
    printf '%s: %s — %s\n    %s\n' "$NAME" "$(printf '%s' "${result%%:*}" | tr '[:lower:]' '[:upper:]')" "${result#*: }" "$*" >&2
    run_summary_field cli_result "$result"
    run_summary_note "$NAME: $result — $*"
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

# --- the generation ---------------------------------------------------------
# What `current` names before this run touches anything: the revert
# target, and what every refusal below leaves in place.
prev="$(readlink "$STORE/current" 2>/dev/null || true)"
action=""
if [ -x "$STORE/$SHA/boss" ]; then
    # Already on disk: a previous tick installed (and confirmed) it, or
    # `current` was moved away by hand. Re-link below; never re-pull.
    if [ "$(readlink "$STORE/current" 2>/dev/null)" = "$SHA" ]; then
        action="unchanged"
    else
        action="relinked"
    fi
else
    command -v docker >/dev/null 2>&1 \
        || refuse "refused: no docker on this host" "the CLI is taken out of the cluster image $IMAGE with docker, and this host has none on PATH. Option (b) of 6f58e9a1 assumes the daemon the retired second stack ran the sim under; without it the CLI stays what it is until docker is installed here (or the step is re-decided)."
    command -v timeout >/dev/null 2>&1 || refuse "refused: no timeout(1) on this host" "coreutils timeout bounds the pull"

    # PULL, captured then printed in full — never reduced before it is
    # stored (CLAUDE.md §Diagnosis). A failure here is the host's, not
    # the tree's, and the two facts it usually needs are named.
    log="$(mktemp -t install-cli-pull.XXXXXX)"
    say "pulling $IMAGE (timeout ${PULL_TIMEOUT}s)"
    rc=0
    timeout "$PULL_TIMEOUT" docker pull "$IMAGE" >"$log" 2>&1 || rc=$?
    sed 's/^/  docker: /' "$log"
    rm -f "$log"
    if [ "$rc" -ne 0 ]; then
        why="exit $rc"
        [ "$rc" -eq 124 ] && why="timed out after ${PULL_TIMEOUT}s"
        echo "$NAME: docker pull $IMAGE failed ($why); its complete output is above." >&2
        echo "    Two host facts a pull from the forge registry needs, neither of which this" >&2
        echo "    script can install: the daemon trusts the plain-HTTP registry" >&2
        echo "    (/etc/docker/daemon.json: {\"insecure-registries\": [\"$REGISTRY_HOST\"]}), and, if" >&2
        echo "    the package is private, root is logged in with a token scoped read:package:" >&2
        echo "    docker login $REGISTRY_HOST   (David does credential admin; the cluster's own pull" >&2
        echo "    uses the forgejo-registry secret). A tag that does not exist yet — the deploy" >&2
        echo "    runner builds the image a few minutes after each train lands — heals on the" >&2
        echo "    next tick. The CLI on this host stays what the previous converge confirmed." >&2
        refuse "refused: docker pull $IMAGE failed ($why)" "see the lines above"
    fi

    # EXTRACT into a staging directory that is not yet a generation.
    stage="$STORE/.staging-$SHA"
    rm -rf "$stage"
    mkdir -p "$stage" || refuse "failed: cannot create $stage" "staging"
    cid="$(docker create "$IMAGE" 2>&1)" \
        || { rm -rf "$stage"; refuse "failed: docker create $IMAGE" "$cid"; }
    cid="${cid%%$'\n'*}"
    cp_out="$(docker cp "$cid:/usr/local/bin/boss" "$stage/boss" 2>&1)"
    cp_rc=$?
    docker rm -f "$cid" >/dev/null 2>&1 || true
    if [ "$cp_rc" -ne 0 ]; then
        rm -rf "$stage"
        refuse "failed: docker cp of /usr/local/bin/boss out of $IMAGE" "$cp_out"
    fi
    chmod 0755 "$stage/boss"

    # CONFIRM THE STAGED BINARY BEFORE IT BECOMES A GENERATION: run it
    # the way the wrapper will, and read what it says it was built from.
    line="$(version_of env BOSS_BUILD_COMMIT="$SHA" "$stage/boss" --version)" || true
    if ! confirmed "$line"; then
        rm -rf "$stage"
        refuse "unconfirmed: the binary out of $IMAGE says '$line', not 'built from $SHA'" \
            "NOT CONFIRMED. With BOSS_BUILD_COMMIT=$SHA in its environment the extracted boss printed '$line'. A binary that does not name the commit it was installed for is not installed: the staging copy is removed and $STORE/current stays at ${prev:-none}."
    fi
    # A directory for this sha with no runnable boss in it (a run that
    # died mid-way) is not a generation; the confirmed staging replaces it.
    rm -rf "${STORE:?}/$SHA"
    mv -T "$stage" "$STORE/$SHA" || { rm -rf "$stage"; refuse "failed: cannot move $stage into place" "rename failed"; }
    action="installed"

    # The image did its job; keep only THIS tag's image so the next
    # pull shares its layers, and the host does not collect 250 MB per
    # train. Best-effort: a leftover image is a disk fact, not a wrong
    # CLI.
    docker images --format '{{.Repository}}:{{.Tag}}' "$IMAGE_REPO" 2>/dev/null \
        | grep -x -- "$IMAGE_REPO:[0-9a-f]\{40\}" | grep -vx -- "$IMAGE" \
        | while IFS= read -r old; do docker rmi "$old" >/dev/null 2>&1 || true; done
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
say "CONFIRMED — $LINK is the tree's CLI at ${SHA:0:8} ($action): $line"
exit 0
