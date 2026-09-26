#!/usr/bin/env bash
#
# forge-repo-path.sh — WHERE THE FORGE'S OWN REPOSITORY IS ON THIS HOST,
# derived from the compose file that declares it. SOURCED, not run: it
# sets FORGE_REPO (the bare repository's path) and FORGE_REPO_FROM (a
# sentence saying which layer answered, for every message that names the
# path).
#
# One definition for every reader of the forge's repository by path
# (CLAUDE.md 9a): publish-github-pr.sh (a publish and --measure), and
# offsite-push.sh (the off-site push of main and publish/*, backlog
# 21d54f4a). Moved here out of publish-github-pr.sh verbatim on
# 2026-09-26, when the second reader arrived, rather than copied.
#
# Inputs (all optional):
#   BOSS_FORGE_REPO_PATH       the path, overriding every layer below
#   BOSS_FORGE_COMPOSE         the compose file (default /opt/forgejo/docker-compose.yml)
#   BOSS_FORGE_REPO_SLUG       owner/name (default david/boss)
#   BOSS_FORGE_DATA_FALLBACK   the image default's data dir (default /opt/forgejo/data)
FORGE_COMPOSE="${BOSS_FORGE_COMPOSE:-/opt/forgejo/docker-compose.yml}"
FORGE_REPO_SLUG="${BOSS_FORGE_REPO_SLUG:-david/boss}"
FORGE_DATA_FALLBACK="${BOSS_FORGE_DATA_FALLBACK:-/opt/forgejo/data}"
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
