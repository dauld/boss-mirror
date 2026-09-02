#!/bin/sh
# install-smoke, nightly, on a host that is not the CI runner.
#
# THE RE-ENABLE THIS IS (17b2ef50). The forge install-smoke workflow has
# been workflow_dispatch-only since 2026-08-18: it ran `docker compose`
# ON the CI runner host, and creating/destroying a compose bridge
# network rewrites the host's iptables NAT/FORWARD rules — every act
# job container lost its default route the moment it did (defect
# c6dc938d; the workflow header carries the exact timeline). The header
# also states the re-enable condition: "a host that is not also the CI
# runner, OR a concurrency group that guarantees it never overlaps
# another job." The concurrency route was analysed on the packet and
# rejected (shared groups either cancel a mid-gate job or head-block
# the queue). This is the other condition: boss-gcp runs the smoke on a
# systemd timer — the conductor is host-network systemd, so compose
# network churn there cannot cut any CI container's route.
#
# A RED FILES A PACKET (the estate-alarm idiom): install breakage
# becomes an urgent backlog item within hours, visible to the queue
# machinery, instead of a log line nobody reads. Green journals only —
# the nightly cadence makes a missing datapoint its own signal.
#
# FRESH CLONE EVERY RUN, deliberately: this smokes the INSTALL story
# (clone → compose build → boot → emergent data), so a warm checkout
# would test something easier than what a new operator does.
#
# Env: JOBS_API (required — where a red files its packet),
#      FORGE_URL (default http://10.20.0.15:3000/david/boss.git),
#      SMOKE_DIR (default /var/tmp/boss-install-smoke).
# sh + jq, no python (directive 26d61c97). Same posture as
# observe-host.sh: failures are LOUD and name their stage.
set -eu

: "${JOBS_API:?JOBS_API is required — a red must be able to file}"
FORGE_URL="${FORGE_URL:-http://10.20.0.15:3000/david/boss.git}"
SMOKE_DIR="${SMOKE_DIR:-/var/tmp/boss-install-smoke}"

STAGE="setup"
LOG_TAIL=""

file_red() {
    # One urgent packet naming the failing stage, with the log tail as
    # evidence. Filed best-effort: the journal keeps the full story
    # either way, and the timer retries tomorrow night.
    body=$(jq -n \
        --arg stage "$STAGE" \
        --arg tail "$LOG_TAIL" \
        '{
            kind: "backlog-item",
            title: ("install-smoke RED at stage " + $stage + " — a fresh install does not boot"),
            subject: {subject_kind: "custom", id: "bosspipeline"},
            owner_id: "emp-david",
            priority: "urgent",
            status: "open",
            tags: [],
            metadata: {
                area: "pipeline",
                install_smoke: $stage,
                detail: ("Nightly install-smoke (boss-gcp timer, 17b2ef50) failed at stage `" + $stage + "`. A fresh clone + compose build + boot did not reach emergent data with clean ordering and bounded dead-letters — the install story a new operator would hit. Log tail:\n" + $tail)
            }
        }')
    curl -sf -X POST "$JOBS_API/api/jobs" \
        -H 'content-type: application/json' \
        -H 'x-boss-user: {"id":"automation:install-smoke","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}' \
        -d "$body" >/dev/null \
        || echo "install-smoke: RED at $STAGE and the packet POST also failed — journal is the record" >&2
}

teardown() {
    # Always: leave the host clean. Diagnostics BEFORE teardown (the
    # 2026-08-09 lesson: evidence evaporates with the containers).
    cd "$SMOKE_DIR/repo/infra/oss-quickstart" 2>/dev/null && {
        LOG_TAIL=$(docker compose logs --no-color --tail=60 boss-services 2>/dev/null | tail -c 3000 || true)
        docker compose down -v --remove-orphans >/dev/null 2>&1 || true
    }
}

on_exit() {
    rc=$?
    teardown
    if [ "$rc" -ne 0 ]; then
        echo "install-smoke: RED at stage $STAGE (rc=$rc)" >&2
        file_red
    fi
    rm -rf "$SMOKE_DIR/repo"
    exit "$rc"
}
trap on_exit EXIT

STAGE="clone"
rm -rf "$SMOKE_DIR/repo"
mkdir -p "$SMOKE_DIR"
git clone --depth 1 "$FORGE_URL" "$SMOKE_DIR/repo"
cd "$SMOKE_DIR/repo/infra/oss-quickstart"

STAGE="build"
cp .env.example .env
docker compose build

STAGE="boot"
docker compose up -d --no-build

STAGE="emergent-data"
# Poll until the live sim has both closed Jobs and issued invoices —
# proof the install booted AND the seed path produced emergent state.
ok=0
i=1
while [ "$i" -le 60 ]; do
    counts=$(docker compose exec -T postgres psql -U boss -d boss -At -F' ' -c \
        "select (select count(*) from jobs where closed_on is not null),
                (select count(*) from invoices);" 2>/dev/null || echo "0 0")
    closed=$(echo "$counts" | awk '{print $1}')
    invoices=$(echo "$counts" | awk '{print $2}')
    echo "t=${i} closed_jobs=${closed:-0} invoices=${invoices:-0}"
    if [ "${closed:-0}" -gt 0 ] && [ "${invoices:-0}" -gt 0 ]; then
        ok=1
        break
    fi
    sleep 10
    i=$((i + 1))
done
[ "$ok" -eq 1 ] || { echo "sim did not produce emergent data within timeout" >&2; exit 1; }

STAGE="ordering"
docker cp ../lint/audit-ordering.sh "$(docker compose ps -q postgres)":/tmp/ord.sh
docker compose exec -T \
    -e PGHOST=127.0.0.1 -e PGUSER=boss -e PGPASSWORD=boss -e PGDATABASE=boss \
    postgres bash /tmp/ord.sh

STAGE="dead-letters"
n=$(docker compose logs boss-services 2>&1 | grep -c 'DEAD-LETTER' || true)
echo "dispatcher dead-letters observed: ${n}"
docker compose logs boss-services 2>&1 | grep 'DEAD-LETTER' | tail -10 || true
if [ "${n:-0}" -gt 20 ]; then
    echo "${n} dead-letters (threshold 20) — systemic, not transient" >&2
    exit 1
fi

STAGE="green"
echo "install-smoke: GREEN — fresh install boots, seeds emergent data, orders cleanly, dead-letters bounded"
