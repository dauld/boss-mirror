#!/usr/bin/env bash
# preflight.sh — readiness check for a BOSS OSS install.
#
# Run this on the target machine BEFORE `docker compose up` (or the
# bare-metal quickstart) to surface conflicts with a previous install or
# with services already running here — so they show up as a clear message
# + remediation instead of a cryptic mid-install failure. The classic one:
# boss-init aborting with `CREATE TABLE ... already exists` because a
# leftover Postgres volume (or a stale image carrying old code) left the DB
# half-initialized from an earlier, failed run.
#
#   ./infra/oss-quickstart/preflight.sh            # full report
#   ./infra/oss-quickstart/preflight.sh --quiet     # print only problems
#
# NON-DESTRUCTIVE: it only inspects. It never deletes a volume, removes an
# image, or kills a process — every finding prints the exact command to fix
# it so YOU decide. Exit 0 = clear to install (warnings allowed); exit 1 =
# blocking conflict(s) that will break the install if not resolved.
#
# `set -e` is deliberately off: we run every check and total the findings.
set -uo pipefail

QUIET=0
MODE=""                              # docker | bare-metal; empty = auto-detect
while [[ $# -gt 0 ]]; do
    case "$1" in
        --quiet)    QUIET=1 ;;
        --mode)     shift; MODE="${1:?--mode needs docker|bare-metal}" ;;
        --mode=*)   MODE="${1#--mode=}" ;;
        --help|-h)  sed -n '2,20p' "$0"; exit 0 ;;
        *) echo "preflight.sh: unknown arg: $1" >&2; exit 2 ;;
    esac
    shift
done

PROJECT="boss"                       # docker-compose project name (`name:` in docker-compose.yml)
GATEWAY_PORT=4443                    # host port the SPA/gateway binds (both paths)
PG_PORT=5432                         # host port the compose Postgres maps (docker path only)
COMPOSE_FILE="$(cd "$(dirname "$0")" && pwd)/docker-compose.yml"

# Auto-detect the install path when not told: a working docker → docker path,
# else bare-metal. Drives which host ports must be FREE (see [ports] below).
HAVE_DOCKER=0
command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1 && HAVE_DOCKER=1
[[ -z "$MODE" ]] && { [[ "$HAVE_DOCKER" -eq 1 ]] && MODE="docker" || MODE="bare-metal"; }

BLOCKERS=0
WARNINGS=0
if [[ -t 1 ]]; then R=$'\033[31m'; Y=$'\033[33m'; G=$'\033[32m'; Z=$'\033[0m'; else R=; Y=; G=; Z=; fi
blocker() { printf '  %s✗ %s%s\n' "$R" "$*" "$Z"; BLOCKERS=$((BLOCKERS+1)); }
warn()    { printf '  %s! %s%s\n' "$Y" "$*" "$Z"; WARNINGS=$((WARNINGS+1)); }
ok()      { [[ "$QUIET" -eq 1 ]] || printf '  %s✓ %s%s\n' "$G" "$*" "$Z"; }
hint()    { printf '      → %s\n' "$*"; }

# Who, if anyone, is listening on a host TCP port. Echoes a description or "".
port_owner() {
    local p="$1"
    if command -v ss >/dev/null 2>&1; then
        ss -ltnH "( sport = :$p )" 2>/dev/null | awk '{print $4}' | sed -n 1p
    elif command -v lsof >/dev/null 2>&1; then
        lsof -nP -iTCP:"$p" -sTCP:LISTEN 2>/dev/null | awk 'NR==2{print $1" (pid "$2")"}'
    fi
}

echo "==> BOSS install preflight"

# ---------------------------------------------------------------------------
# Docker remnants — only meaningful when docker is installed.
# ---------------------------------------------------------------------------
if [[ "$HAVE_DOCKER" -eq 1 ]]; then
    echo "  [docker]"

    running=$(docker ps --filter "label=com.docker.compose.project=$PROJECT" --format '{{.Names}}' 2>/dev/null)
    if [[ -n "$running" ]]; then
        blocker "BOSS containers from a previous run are still up: $(echo "$running" | tr '\n' ' ')"
        hint "stop them:  docker compose -f $COMPOSE_FILE down"
    else
        ok "no BOSS containers running"
    fi

    # A leftover data volume is THE reason boss-init can meet an already-
    # initialized DB. Current boss-init detects this and reuses the volume,
    # but a half-written DB from a failed run (or old image code) is exactly
    # the reported `relation already exists` footgun — flag it loudly.
    vols=$(docker volume ls --format '{{.Name}}' 2>/dev/null | grep -E "^${PROJECT}_postgres-data$|^${PROJECT}_boss-auth$" || true)
    if [[ -n "$vols" ]]; then
        warn "leftover data volume(s) from a prior install: $(echo "$vols" | tr '\n' ' ')"
        hint "for a guaranteed-clean DB (recommended after a failed run):  docker compose -f $COMPOSE_FILE down -v"
    else
        ok "no leftover BOSS data volumes — DB will initialize fresh"
    fi

    # A cached image built from an older checkout silently runs stale code
    # (e.g. a since-removed monolithic schema.sql). We can't read the source
    # commit out of the image, so report its age and recommend a rebuild.
    if docker image inspect "${PROJECT}:latest" >/dev/null 2>&1 || docker image inspect boss:latest >/dev/null 2>&1; then
        created=$(docker image inspect boss:latest --format '{{.Created}}' 2>/dev/null | cut -c1-10)
        warn "a boss:latest image already exists (built ${created:-unknown})"
        hint "if it predates your current checkout it runs OLD code — force a clean rebuild:  docker compose -f $COMPOSE_FILE build --no-cache"
    else
        ok "no cached boss image — it will build from your current source"
    fi
else
    [[ "$QUIET" -eq 1 ]] || echo "  [docker] not installed / not running — skipping docker checks"
fi

# ---------------------------------------------------------------------------
# Host port conflicts — both install paths bind these on the host.
# ---------------------------------------------------------------------------
echo "  [ports] (mode: $MODE)"
# Which host ports must be FREE depends on the install path. Docker compose
# brings up its OWN Postgres (binds host 5432) alongside the gateway (4443),
# so both must be free. The bare-metal path expects Postgres + NATS already
# running as prerequisites, so there only the gateway port 4443 must be free —
# flagging 5432 would be a false alarm against the required local Postgres.
PORTS_TO_CHECK=("$GATEWAY_PORT:SPA/gateway")
[[ "$MODE" == "docker" ]] && PORTS_TO_CHECK+=("$PG_PORT:Postgres (compose binds this)")
for entry in "${PORTS_TO_CHECK[@]}"; do
    p="${entry%%:*}"; what="${entry#*:}"
    owner=$(port_owner "$p")
    if [[ -n "$owner" ]]; then
        # A BOSS container/process holding it is a prior-install remnant
        # (already flagged above); anything else is a hard collision.
        blocker "port $p ($what) is already in use by: $owner"
        hint "free it, stop the prior BOSS stack, or remap the port before installing"
    else
        ok "port $p ($what) free"
    fi
done

# ---------------------------------------------------------------------------
# Bare-metal remnants — leftover background services + an initialized DB.
# (Harmless to run even on a docker host; they just won't find anything.)
# ---------------------------------------------------------------------------
echo "  [bare-metal]"
if [[ -f "$HOME/.boss-pids" ]]; then
    live=0
    while read -r pid; do [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null && live=$((live+1)); done < "$HOME/.boss-pids"
    if [[ "$live" -gt 0 ]]; then
        blocker "$live BOSS service(s) from a prior bare-metal run are still running (holding ports + DB connections)"
        hint "stop them:  kill \$(cat ~/.boss-pids)"
    else
        ok "no leftover bare-metal BOSS services"
    fi
else
    ok "no prior bare-metal run recorded (~/.boss-pids absent)"
fi

if command -v psql >/dev/null 2>&1 && pg_isready -h 127.0.0.1 -p "$PG_PORT" -q 2>/dev/null; then
    initialized=$(PGPASSWORD=boss psql -h 127.0.0.1 -U boss -d boss -At \
        -c "SELECT to_regclass('subject_kinds')" 2>/dev/null || true)
    if [[ -n "$initialized" ]]; then
        warn "the local 'boss' Postgres database is already initialized"
        hint "the bare-metal quickstart drops + recreates it — you will lose the existing demo (Ctrl-C now to keep it)"
    fi
fi

# ---------------------------------------------------------------------------
echo ""
if [[ "$BLOCKERS" -gt 0 ]]; then
    echo "==> preflight: ${R}${BLOCKERS} blocking conflict(s)${Z}, $WARNINGS warning(s)."
    echo "    Resolve the ✗ items above, then re-run. (A clean slate after a failed"
    echo "    docker run is usually:  docker compose -f $COMPOSE_FILE down -v)"
    exit 1
fi
echo "==> preflight: ${G}ready to install${Z} ($WARNINGS warning(s) — review the ! items if any)."
exit 0
