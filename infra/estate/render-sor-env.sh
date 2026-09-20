#!/usr/bin/env bash
# render-sor-env.sh — /etc/boss/sor.env, rendered from the one source.
#
#   render-sor-env.sh                  print the env file to stdout
#   render-sor-env.sh --to <path>      write it there (atomically, 0644)
#   render-sor-env.sh --value <KEY>    print one rendered value
#   render-sor-env.sh --dev-sor-url    print the pod's spelling of the
#                                      system of record (infra/dev/sor-url)
#
# THE SOURCE is infra/estate/estate.toml, read from beside this script
# (never from cwd: the forge runs this from a detached checkout, boss-gcp
# from /opt/boss, a test from a worktree). Every key below is REQUIRED —
# a source that lost a line must fail here, at render time on the host
# that installs, not later in a unit that started without its address.
#
# WHAT IT RENDERS, and who reads each line:
#   BOSS_JOBS_URL             the system of record — boss-maintenance-
#                             wrap.sh, boss-step.sh, the ops runner, every
#                             `boss` verb (the CLI reads it as before)
#   JOBS_API                  the SAME address under the older spelling the
#                             estate observers, alert-lib and the gate
#                             runner read. Rendered from the one line so
#                             the two cannot differ.
#   BOSS_FORGE_HOST           the forge's address (journal-door-ensure,
#                             the host-readiness probe)
#   BOSS_FORGE_URL            Forgejo's web/API/clone base
#   BOSS_FORGE_REGISTRY_HOST  the OCI registry host:port — forge-defaults
#                             builds every image repo from it
#   BOSS_FORGE_JOURNAL_URL    the systemd-journal-gatewayd door
#   BOSS_MIRROR_URL           the public GitHub mirror, as a person
#                             opens it (prep-github-publish names it in
#                             its refusal; the website links it)
#   BOSS_MIRROR_SLUG          owner/repo, DERIVED from that URL — what
#                             the GitHub API and `gh pr create` want
#                             (publish-github-pr, read-publish-checks).
#                             Derived rather than declared for the same
#                             reason JOBS_API is: two lines can disagree.
#
# Backlog 5222163e (audit H10): until this file, the address was a
# literal in 47 files and the forge's in 62.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SOURCE="${BOSS_ESTATE_SOURCE:-$HERE/estate.toml}"

[ -r "$SOURCE" ] || { echo "render-sor-env: $SOURCE is missing or unreadable — nothing to render from" >&2; exit 1; }

# read_key <toml key> — the value of one `key = "value"` line, refusing
# an absent or empty one by name.
read_key() {
    local v
    v="$(sed -n "s/^$1 = \"\(.*\)\"[[:space:]]*$/\1/p" "$SOURCE" | sed -n 1p)"
    if [ -z "$v" ]; then
        echo "render-sor-env: $SOURCE declares no \`$1 = \"…\"\` line — every host would start without it; refusing to render" >&2
        exit 1
    fi
    printf '%s' "$v"
}

sor_url="$(read_key sor_url)"
sor_cluster_url="$(read_key sor_cluster_url)"
forge_host="$(read_key forge_host)"
forge_url="$(read_key forge_url)"
forge_registry="$(read_key forge_registry)"
forge_journal="$(read_key forge_journal)"
mirror_url="$(read_key mirror_url)"
# owner/repo: the URL with its scheme and host removed. A URL that is
# not https://<host>/<owner>/<repo> would render a slug the GitHub API
# refuses much later, in a publish nobody is watching — so it is refused
# here, on the host that installs (backlog f8af6040).
mirror_slug="${mirror_url#*://}"
mirror_slug="${mirror_slug#*/}"
case "$mirror_slug" in
    */*/*|/*|*/) mirror_slug="" ;;
    */*) ;;
    *) mirror_slug="" ;;
esac
if [ -z "$mirror_slug" ]; then
    echo "render-sor-env: $SOURCE declares mirror_url = \"$mirror_url\", which is not https://<host>/<owner>/<repo> — nothing can derive the mirror's slug from it; refusing to render" >&2
    exit 1
fi

render() {
    printf '# /etc/boss/sor.env — rendered from infra/estate/estate.toml by the host'"'"'s install.\n'
    printf '# Do not edit: the next converge rewrites it. Change the source.\n'
    printf 'BOSS_JOBS_URL=%s\n' "$sor_url"
    printf 'JOBS_API=%s\n' "$sor_url"
    printf 'BOSS_FORGE_HOST=%s\n' "$forge_host"
    printf 'BOSS_FORGE_URL=%s\n' "$forge_url"
    printf 'BOSS_FORGE_REGISTRY_HOST=%s\n' "$forge_registry"
    printf 'BOSS_FORGE_JOURNAL_URL=%s\n' "$forge_journal"
    printf 'BOSS_MIRROR_URL=%s\n' "$mirror_url"
    printf 'BOSS_MIRROR_SLUG=%s\n' "$mirror_slug"
}

case "${1:-}" in
    "")
        render ;;
    --dev-sor-url)
        printf '%s\n' "$sor_cluster_url" ;;
    --value)
        key="${2:?--value needs a KEY}"
        v="$(render | sed -n "s/^$key=//p")"
        [ -n "$v" ] || { echo "render-sor-env: no rendered key named $key" >&2; exit 2; }
        printf '%s\n' "$v" ;;
    --to)
        dest="${2:?--to needs a path}"
        # Atomic: a unit reading a half-written file would start with half
        # an environment. The temp file sits beside the target so the
        # rename never crosses a filesystem.
        mkdir -p "$(dirname "$dest")"
        tmp="$(mktemp "$(dirname "$dest")/.sor.env.XXXXXX")"
        render > "$tmp"
        chmod 0644 "$tmp"
        mv -f "$tmp" "$dest"
        echo "render-sor-env: wrote $dest ($(grep -c '^[A-Z_]*=' "$dest") keys) from $SOURCE" ;;
    *)
        echo "usage: render-sor-env.sh [--to <path> | --value <KEY> | --dev-sor-url]" >&2; exit 2 ;;
esac
