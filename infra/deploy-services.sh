#!/usr/bin/env bash
# Unified deploy script for the Boss API services.
#
# Replaces deploy-scratch-services.sh and the scattered per-service
# setup.sh files. One data table, two environments:
#
#   sudo ./infra/deploy-services.sh prod
#   sudo ./infra/deploy-services.sh scratch
#   sudo ./infra/deploy-services.sh both
#
# Services:
#   - Paired (prod + scratch): shipping, messages, inventory,
#     commerce, people, assets, catalog, calendar, jobs. These share
#     a DB + NATS and the scratch variant runs on +1000 ports
#     against boss_scratch.
#   - Solo (prod only): sim, ml, docs. No scratch variants (sim is
#     the scratch driver; ml/docs are read-only stateless consumers).
#
# Per run the script:
#   1. Writes /etc/boss-<name>-api[-scratch].toml
#   2. Writes /etc/systemd/system/boss-<name>-api[-scratch].service
#   3. systemctl daemon-reload
#   4. Stages binaries (+ carried web-dist / step-plugins) into a
#      generation at /usr/local/boss/releases/<sha>/
#   5. Converges the schema on the target database(s) —
#      infra/postgres/migrate.sh, manifest order, idempotent. Runs
#      before the flip, so a migration error aborts the deploy with
#      nothing activated and nothing restarted.
#   6. Atomically flips the `current` symlink (previous generation stays
#      on disk as the revert target — see infra/generation.sh)
#   7. systemctl enable + restart each unit (units exec through the
#      `current` symlink), arms boss-deploy-confirm, prunes to the 3
#      newest generations (logging what was pruned, with sizes)
#   8. Probes /api/<name>/health and reports the status code
#
# Idempotent — safe to re-run. Re-deploying the sha that is already
# live re-activates the standing generation (stage is skipped when the
# build stamp matches) and restarts; it never clobbers a live
# generation mid-copy — staging is a dot-dir, activation is one rename.
#
# Modes beyond deploy:
#
#   sudo ./infra/deploy-services.sh check prod|scratch|both
#       diff generated configs/units against disk, write nothing
#   ./infra/deploy-services.sh probe [prod|scratch|both]
#       strict health probes — exit 1 on any failure. This is the
#       roster boss-deploy-confirm evaluates, so the confirm cannot
#       drift from the deploy list (defaults to prod).
#   sudo ./infra/deploy-services.sh revert
#       flip current <-> previous and restart the prod fleet — the
#       make-before-break rollback (seconds, no build)
#   sudo ./infra/deploy-services.sh prune
#       prune the generation store to the newest 3, printing sizes

set -euo pipefail

usage() {
    echo "usage: $0 <prod|scratch|both>" >&2
    echo "       $0 check <prod|scratch|both>" >&2
    echo "       $0 probe [prod|scratch|both]" >&2
    echo "       $0 revert" >&2
    echo "       $0 prune" >&2
    exit 2
}

if [[ $# -lt 1 ]]; then
    usage
fi

MODE="apply"
case "${1:-}" in
    check)  MODE="check";  shift; [[ $# -ge 1 ]] || usage ;;
    probe)  MODE="probe";  shift ;;
    revert) MODE="revert"; shift ;;
    prune)  MODE="prune";  shift ;;
esac

if [[ "$MODE" == "revert" || "$MODE" == "prune" ]]; then
    # No env argument: both operate on the generation store itself.
    # TARGET drives the restart roster on revert — prod, because the
    # confirm (the caller that matters) is prod-scoped; scratch units
    # pick the reverted generation up on their next restart.
    TARGET="prod"
else
    TARGET="${1:-}"
    if [[ "$MODE" == "probe" && -z "$TARGET" ]]; then
        TARGET="prod"
    fi
    case "$TARGET" in
        prod|scratch|both) ;;
        *)
            echo "error: target must be 'prod', 'scratch', or 'both', got '$TARGET'" >&2
            exit 2
            ;;
    esac
fi

NATS_URL="nats://127.0.0.1:4222"
# Services connect directly to PG :5432. The roster has grown to ~24
# services × sqlx pool-10, so the cluster runs with max_connections=400
# (raised from PG's default 100 via `ALTER SYSTEM SET max_connections`).
# At the default 100 a high-warp regen compresses a sim-year of writes
# into minutes, every pool tries to fill at once, policy-api gets starved
# of DB connections, and jobs-api fail-closes its policy checks (403
# policy-unreachable) — which the JetStream redelivery layer then
# amplifies into sustained saturation. 400 gives the pools room so the
# contention stays in the transient-blip regime redelivery is built for.
# A pgbouncer-in-front-of-PG layer was prototyped + removed pre-v0.1 (the
# isolation pays off at much larger concurrency than this has; the SCRAM
# image + statement-cache compatibility surface isn't worth carrying).
PROD_DB_URL="postgres://boss:boss@127.0.0.1/boss"
SCRATCH_DB_URL="postgres://boss:boss@127.0.0.1/boss_scratch"
REPO_ROOT="/opt/boss"
# Attachment bytes for the file_refs surface. One definition, shared
# with infra/backup.sh — see infra/files-root.sh for why.
# shellcheck source=infra/files-root.sh
. "$(dirname "$0")/files-root.sh"

# ---------------------------------------------------------------------
# Per-machine deploy configuration
# ---------------------------------------------------------------------
# WHERE THE SYSTEM OF RECORD LIVES IS A PROPERTY OF THIS BOX, not of
# whoever ran the deploy — so it belongs in a file on the box rather
# than an environment variable somebody has to remember.
#
# Measured 2026-08-17: the conductor invokes this script as
# `sudo -n .../deploy-services.sh prod`, and sudo strips the
# environment. boss-train.service carries
# BOSS_JOBS_URL=http://10.20.0.34:7900, that value never reached here,
# and the timer drop-in was written with the local default instead. So
# every nightly maintenance packet still filed against the demo
# instance — the wiring was repo-borne and visible, and still pointed
# at the wrong database.
#
# `set -a` so plain `KEY=value` lines export without each needing the
# word `export`, which is what makes this a config file rather than a
# shell script someone has to get right. Absent file = defaults hold,
# which is correct for a fresh box and for a deploy running ON the
# cluster, where the SoR is localhost.
#
# Template: infra/deploy.env.example.
DEPLOY_ENV_FILE="${BOSS_DEPLOY_ENV:-/etc/boss/deploy.env}"
if [ -f "$DEPLOY_ENV_FILE" ]; then
    set -a
    # shellcheck disable=SC1090
    . "$DEPLOY_ENV_FILE"
    set +a
    echo "deploy: config from $DEPLOY_ENV_FILE"
fi

# Generation store paths + atomic-flip helpers, shared with
# deploy-web.sh and deploy-confirm.sh so they cannot drift.
# shellcheck source=infra/generation.sh
. "$(dirname "$0")/generation.sh"

# Port tables sourced from `boss-ports-list` — single source of
# truth shared with the Rust binaries (boss_ports::PAIRED / SOLO).
# Falls back to a hardcoded snapshot if the binary isn't built yet
# (fresh checkout, pre-build); the snapshot has to stay in sync if
# you edit it directly. The 7060/7250 collision (commits `bb60c58`
# + `8bf0f0a`) was caused by exactly this kind of hand-sync drift.
PORTS_BIN="$REPO_ROOT/target/release/boss-ports-list"
# `sim-control` is in the port registry for the boss-simulator proxy's
# benefit, but it is NOT a deployable unit — it's the brewery-sim
# daemon's embedded control/telemetry server, alive only while the
# daemon runs. Filter it at the source so none of the deploy loops
# (unit planning, TO_DEPLOY, health probes) trip on it. If the
# registry grows more embedded ports, consider a `deployable` flag on
# boss_ports::PortSpec instead of extending this filter.
if [[ -x "$PORTS_BIN" ]]; then
    mapfile -t PAIRED_SERVICES < <("$PORTS_BIN" --paired)
    mapfile -t SOLO_SERVICES   < <("$PORTS_BIN" --solo | grep -v '^sim-control:')
else
    echo "warning: $PORTS_BIN not built; using fallback snapshot" >&2
    PAIRED_SERVICES=(
        "shipping:7100:8100"
        "messages:7200:8200"
        "inventory:7300:8300"
        "commerce:7400:8400"
        "people:7500:8500"
        "assets:7600:8600"
        "catalog:7750:8750"
        "calendar:7860:8860"
        "jobs:7900:8900"
    )
    SOLO_SERVICES=(
        # sim:7060 retired 2026-05-03 with boss-sim-api (HumanWorker step 9b).
        # clock:7060 reclaimed 2026-05-30 — the authoritative "what
        # time is it" service. See boss-clock + boss-clock-client.
        "clock:7060"
        "ml:7070"
        "ledger:7080"
        "content:7090"
        # events:7150 — audit_log tail/stream/export. Split out of
        # boss-people-api 2026-06 (see crates/core/boss-events/src/
        # bin/boss_events_api.rs + boss-ports.events entry).
        "events:7150"
        "policy:7250"
        "docs:7050"
        # accounts:7550 — accounts + account_notes + account_team +
        # account_next_actions + account_risk_scores + support_cases.
        # Split out of boss-people-api 2026-06; mirrors the
        # one-binary-per-domain pattern every other core domain uses.
        "accounts:7550"
        # dispatcher:7950 — auto-assigns ready Steps to role-matched
        # Employees by subscribing to jobs.step.* NATS events. Core
        # service so sim runs + real-human runs go through the same
        # assignment path (no sim-side dispatch).
        "dispatcher:7950"
        # search:7960 — global search read surface (boss-search).
        # Mirrors the PortSpec in boss-ports; the two lists are kept
        # in step by hand, per that crate's header.
        "search:7960"
        # observability:7880 + simulator:7010 were in the port registry
        # and missing here — caught by the agreement test in
        # boss-ports, which is the point of that test existing.
        "observability:7880"
        "simulator:7010"
        # views:7961 — the View registry + the endpoint that runs one
        # (boss-views). Same hand-kept pairing with boss-ports.
        "views:7961"
        "classes:7800"
        "locations:7820"
        "subject-kinds:7830"
        "campaigns:7845"
        "customers:7855"
        "products:7840"
    )
fi

# Timer + service unit pairs. Each entry is
# `unit-stem:source-dir-relative-to-infra` — the source dir
# holds `<unit-stem>.service` + `<unit-stem>.timer`. The deploy
# loop installs both, daemon-reloads, and `enable --now`s the
# timer (the service is `Type=oneshot`, fires from the timer).
#
# Adding a new timer = author the .service + .timer in the
# right place under infra/, then add a row here. New timers
# land via `sudo ./infra/deploy-services.sh prod` instead of a
# `sudo install` treadmill that's been the source of every
# "this timer was authored but never installed" gap so far
# (audit-integrity, ml-inference-batch, ledger-recognize,
# conservation-invariants — all caught by hand).
TIMERS=(
    # The estate's host observer (a5d14977): the cluster CronJob covers
    # kubernetes-nodes; this covers the machine it runs on — the
    # conductor VM whose registry note said "nothing watches it".
    "boss-estate-observe-host:estate"
    # The unit half (729329c6): the host observer answers what the
    # machine is; this answers what its units are doing, so "is
    # boss-train.service alive" is a SoR read instead of a human on
    # the host.
    "boss-estate-observe-units:estate"
    "boss-messages-events-purge:."
    "boss-audit-integrity-check:."
    "boss-ledger-recognize:."
    "boss-ledger-replay-check:."
    "boss-ml-inference-batch:ml"
    "boss-conservation-invariants:lint"
    "boss-files-gc:."
    # event_facts is a projection of audit_log with no other refresh
    # path — before this it only moved on a full boss-rebuild-all, and
    # sat tens of thousands of events behind on a live box.
    "boss-views-catchup:."
    # search_index has no incremental form — events are capped per
    # Subject, so an append cannot be correct — and outside
    # boss-rebuild-all nothing refreshed it. Measured stale at 0.5% job
    # coverage on a live box: search could not find 99% of the corpus.
    "boss-search-reindex:."
    # The PR train's timers are gone (protocol-cadence): the schedule
    # is rows in the cadence_rules registry (114-cadence-rules.sql),
    # executed by the boss-train daemon below. See RETIRED_TRAIN_TIMERS.
    # boss-backup: previously deferred over destination + retention
    # review — but the UNIT is live on this box regardless, and the
    # maintenance family (internal-forge Q6) needs deploy to own the
    # unit file so the Pre/Post visibility hooks actually reach
    # systemd. Managing the unit is not resolving retention; that
    # review stands, now with a Job making every run visible.
    "boss-backup:."
    # The deploy dead-man switch (deployment-as-network Q4): armed by
    # `systemctl restart boss-deploy-confirm.timer` at generation flip,
    # it reads the probe roster at +2 and +8 minutes and flips
    # current -> previous (revert) on a failed reading. A separate
    # unit, never in-process waiting inside the deployer — a dead-man
    # that dies with the deployer reverts nothing.
    "boss-deploy-confirm:."
)

# Long-running daemons that aren't `boss-*-api` services. Each
# entry is `unit-stem:source-dir-relative-to-infra`. The deploy
# loop installs the unit, installs the binary, and `enable --now`s
# the service. Distinct from TIMERS (oneshot + .timer) and the
# port-table-driven *-api services.
#
# v1.0.10 F15: boss-step-effects-runner retired — step-completion
# side effects now route through the dispatcher's rule registry
# (infra/dispatcher/rules.toml).
#
# boss-event-relay: drains the transactional event outbox into
# audit_log + NATS (docs/design/transactional-audit-log.md). Inert
# until an emitter is migrated to record_event_in_tx, but deployed
# from day one so the pipeline never hits the
# service-outside-the-deploy-list rot class (cybernetics, 2026-07-13).
DAEMONS=(
    "boss-event-relay:events"
    # The train conductor's cadence loop (`boss train cadence`,
    # docs/design/protocol-cadence.md): evaluates the cadence_rules
    # registry against boss-clock time and fires the train verbs the
    # rows name, recording every firing in cadence_firings. This unit
    # replaces the boss-pr-train / boss-pr-train-reconcile timer pair
    # — systemd keeps the process alive, the rules own the schedule.
    "boss-train:train"
)

# Units whose scheduling knowledge moved into the cadence_rules
# registry (protocol-cadence). Left enabled they would keep firing the
# 06:00/18:00 boarding beside the rules, so the deploy retires them
# from boxes that still carry them. The flock made the overlap safe;
# the double schedule is what must not survive.
RETIRED_TRAIN_TIMERS=(
    boss-pr-train
    boss-pr-train-reconcile
)

port_of() {
    local name="$1" env="$2"
    for entry in "${PAIRED_SERVICES[@]}"; do
        IFS=: read -r n prod scratch <<<"$entry"
        if [[ "$n" == "$name" ]]; then
            [[ "$env" == "prod" ]] && echo "$prod" || echo "$scratch"
            return 0
        fi
    done
    for entry in "${SOLO_SERVICES[@]}"; do
        IFS=: read -r n prod <<<"$entry"
        if [[ "$n" == "$name" ]]; then
            echo "$prod"
            return 0
        fi
    done
    echo "port_of: unknown service '$name'" >&2
    return 1
}

# Human-readable description for a service unit.
description_of() {
    case "$1" in
        shipping)  echo "Shipping API" ;;
        messages)  echo "Messages API" ;;
        inventory) echo "Inventory API" ;;
        commerce)  echo "Commerce API" ;;
        people)    echo "People API" ;;
        assets)     echo "Assets API" ;;
        catalog)   echo "Catalog API" ;;
        calendar)  echo "Calendar API" ;;
        jobs)      echo "Jobs API (coordination primitive)" ;;
        sim)       echo "Simulator API" ;;
        ml)        echo "ML Platform API" ;;
        ledger)    echo "Ledger API" ;;
        content)   echo "HR Content API" ;;
        policy)    echo "Policy API" ;;
        clock)     echo "Clock API" ;;
        docs)          echo "Docs API" ;;
        classes)       echo "Class Registry API" ;;
        locations)     echo "Locations Registry API" ;;
        subject-kinds) echo "Subject Kind Registry API" ;;
        campaigns)     echo "Marketing Campaigns API" ;;
        customers)     echo "DTC Customers API" ;;
        products)      echo "Finished Product Catalog API" ;;
        events)        echo "Audit Log Read API (tail / stream / export)" ;;
        accounts)      echo "Accounts API (notes / team / next-actions / risk / cases)" ;;
        search)        echo "Global Search API (one index over Subjects, Jobs and events)" ;;
        views)         echo "Views API (saved compositions over the information layer)" ;;
        dispatcher)    echo "Dispatch Service (auto-assigns ready Steps to role-matched Employees)" ;;
        simulator)     echo "Simulator UX service (SPA + control API)" ;;
        *)             echo "$1 API" ;;
    esac
}

# Emit the TOML config body for a paired service in a given env.
emit_paired_config() {
    local name="$1" env="$2"
    local port db_url
    port=$(port_of "$name" "$env")
    [[ "$env" == "prod" ]] && db_url="$PROD_DB_URL" || db_url="$SCRATCH_DB_URL"

    cat <<EOF
# Managed by infra/deploy-services.sh — edits will be overwritten.
postgres_url = "$db_url"
# Loopback-only: the gateway is the sole trust boundary and is
# co-located, so backend ports must not be reachable off-host
# (SECURITY.md §Deployment trust model).
http_bind = "127.0.0.1:$port"
nats_url = "$NATS_URL"
EOF
    case "$name" in
        commerce)
            echo "people_api_url = \"http://127.0.0.1:$(port_of people "$env")\""
            # Hard-requires the Class registry: the invoice status
            # CHECK was retired for registry-backed validation
            # (subject_kind='invoice', member_attribute='status').
            echo "classes_api_url = \"http://127.0.0.1:$(port_of classes prod)\""
            ;;
        messages)
            # Daily messages_events retention sweep. Mirrors the
            # `[messages] events_retention_days` row in
            # examples/<tenant>/seeds/tenant.toml; the deploy
            # surface lifts it into /etc/boss-messages-api.toml so
            # the boss-messages-events-purge binary (and its
            # systemd timer) can read it. Leave commented to
            # disable the sweep.
            echo "events_retention_days = 90"
            # Hard-requires the Class registry: the message kind
            # CHECK was retired for registry-backed validation
            # (subject_kind='message', member_attribute='kind').
            echo "classes_api_url = \"http://127.0.0.1:$(port_of classes prod)\""
            ;;
        people)
            echo "assets_api_url = \"http://127.0.0.1:$(port_of assets "$env")\""
            # boss-people-api hard-requires classes + locations
            # registry URLs at startup (the schema CHECKs were
            # retired in favor of registry-backed validation).
            # Both are prod-only solo services, so the same port
            # serves prod + scratch.
            echo "classes_api_url = \"http://127.0.0.1:$(port_of classes prod)\""
            echo "locations_api_url = \"http://127.0.0.1:$(port_of locations prod)\""
            ;;
        assets)
            echo "people_api_url = \"http://127.0.0.1:$(port_of people "$env")\""
            # Device-insights projection fans out to three services.
            echo "catalog_api_url = \"http://127.0.0.1:$(port_of catalog "$env")\""
            echo "jobs_api_url = \"http://127.0.0.1:$(port_of jobs "$env")\""
            echo "inventory_api_url = \"http://127.0.0.1:$(port_of inventory "$env")\""
            # Hard-requires the Class registry: asset events validate their
            # intake-source / warranty-coverage / condition at ingest.
            echo "classes_api_url = \"http://127.0.0.1:$(port_of classes prod)\""
            ;;
        catalog)
            echo "assets_api_url = \"http://127.0.0.1:$(port_of assets "$env")\""
            # Hard-requires the Class registry: the marketing-asset
            # `kind`, DeviceCategory, and document-kind CHECKs were
            # retired for registry-backed validation.
            echo "classes_api_url = \"http://127.0.0.1:$(port_of classes prod)\""
            ;;
        inventory)
            # Warehouse-status projection fans out to three services.
            echo "jobs_api_url = \"http://127.0.0.1:$(port_of jobs "$env")\""
            echo "assets_api_url = \"http://127.0.0.1:$(port_of assets "$env")\""
            echo "shipping_api_url = \"http://127.0.0.1:$(port_of shipping "$env")\""
            # Hard-requires the Class registry: the discrepancy_kind
            # CHECK was retired for registry-backed validation.
            echo "classes_api_url = \"http://127.0.0.1:$(port_of classes prod)\""
            ;;
        shipping)
            # Hard-requires the Class registry: the carrier CHECK was
            # retired for registry-backed validation (carrier is
            # identity-first optional, validated only when present).
            echo "classes_api_url = \"http://127.0.0.1:$(port_of classes prod)\""
            ;;
        jobs)
            # boss-jobs-api seeds the executive-role cache at startup
            # so the escalation router filters recipients by tenant-
            # defined `metadata.is_executive` Class membership.
            echo "classes_api_url = \"http://127.0.0.1:$(port_of classes prod)\""
            ;;
    esac
}

# Emit the TOML config body for a solo (prod-only) service.
emit_solo_config() {
    local name="$1"
    local port
    port=$(port_of "$name" prod)
    case "$name" in
        sim)
            cat <<EOF
# Managed by infra/deploy-services.sh — edits will be overwritten.
http_bind = "127.0.0.1:$port"
scratch_postgres_url = "$SCRATCH_DB_URL"
scratch_api_url = "scratch://127.0.0.1"
repo_root = "$REPO_ROOT"
EOF
            ;;
        ml)
            cat <<EOF
# Managed by infra/deploy-services.sh — edits will be overwritten.
postgres_url = "$PROD_DB_URL"
http_bind = "127.0.0.1:$port"
EOF
            ;;
        ledger)
            # NATS publisher gates the ledger.* upstream events the
            # audit_log → financial_facts projection relies
            # on. Without it the live writes still land but
            # rebuild_facts has no audit_log rows to project from.
            # classes_api_url seeds the executive-role cache for the
            # /api/it/providers admin gate.
            cat <<EOF
# Managed by infra/deploy-services.sh — edits will be overwritten.
postgres_url = "$PROD_DB_URL"
http_bind = "127.0.0.1:$port"
nats_url = "$NATS_URL"
classes_api_url = "http://127.0.0.1:$(port_of classes prod)"
EOF
            ;;
        content)
            # NATS publisher gates content.bulletin.* emission to
            # audit_log; without it bulletins write to the projection
            # but never enter the event stream.
            #
            # `policy_api_url` IS NOT OPTIONAL HERE, and the binary
            # will not tell you so. `build_files_router` falls back to
            # `PermissivePolicyClient` when it is absent — uploads and
            # downloads with NO policy enforcement, announced only by a
            # tracing::warn nobody reads. That is a second
            # unauthenticated machine door (7fcd78fa is the first), and
            # it would open the moment the `[files]` block below
            # appeared. The two lines belong together.
            #
            # `[files]` turns on the per-packet attachment surface:
            # file_refs rows attach bytes to a subject, job, step or
            # event, keyed `sha256/<hex>` so a reference is immutable by
            # construction and identical bytes dedupe. Built, tested and
            # shipped long ago; never switched on, so it had zero
            # callers. Bytes land under `root`, which
            # `LocalDiskStorage::new` creates on startup if absent.
            cat <<EOF
# Managed by infra/deploy-services.sh — edits will be overwritten.
postgres_url = "$PROD_DB_URL"
http_bind = "127.0.0.1:$port"
nats_url = "$NATS_URL"
policy_api_url = "http://127.0.0.1:$(port_of policy prod)"

[files]
root = "$BOSS_FILES_ROOT"
EOF
            ;;
        search|views)
            # boss-search-api / boss-views-api take everything from
            # env vars + clap defaults (BOSS_POSTGRES_URL,
            # BOSS_POLICY_URL). Stub config for uniformity; emit_unit
            # injects the env and omits the --config flag.
            cat <<EOF
# Managed by infra/deploy-services.sh — edits will be overwritten.
# ($name takes all settings from env vars.)
port = $port
EOF
            ;;
        clock)
            # boss-clock-api takes everything from env vars
            # (BOSS_CLOCK_MODE, BOSS_SIM_TICK_*, BOSS_POSTGRES_URL).
            # We emit a stub config so the uniform --config flag
            # stays happy; emit_unit() injects the env.
            cat <<EOF
# Managed by infra/deploy-services.sh — edits will be overwritten.
# (boss-clock-api takes all settings from env vars.)
port = $port
EOF
            ;;
        policy)
            # boss-policy-api reads its port from env; a config file isn't
            # required today but we emit a stub so deploy-services.sh's
            # uniform --config flag stays happy. The port-env injection
            # is handled in emit_unit().
            cat <<EOF
# Managed by infra/deploy-services.sh — edits will be overwritten.
# (boss-policy-api takes its port from BOSS_POLICY_PORT env var.)
port = $port
EOF
            ;;
        docs)
            cat <<EOF
# Managed by infra/deploy-services.sh — edits will be overwritten.
postgres_url = "$PROD_DB_URL"
http_bind = "127.0.0.1:$port"
repo_root = "$REPO_ROOT"
EOF
            ;;
        classes|locations|subject-kinds|events)
            # Read-only registry / read-surface services. Just need
            # a Postgres URL + bind. No NATS (no event publishing —
            # they're lookup tables / read-side mirrors), no
            # cross-service deps. boss-events-api falls under this
            # shape: it reads audit_log + emits no new events.
            cat <<EOF
# Managed by infra/deploy-services.sh — edits will be overwritten.
postgres_url = "$PROD_DB_URL"
http_bind = "127.0.0.1:$port"
EOF
            ;;
        accounts)
            # Accounts service: publisher (NATS) for account-team
            # changes + notes + risk events, Postgres for projection
            # state, classes lookups for account_team_role validation,
            # assets for open-ticket-count on the account detail view.
            cat <<EOF
# Managed by infra/deploy-services.sh — edits will be overwritten.
postgres_url = "$PROD_DB_URL"
http_bind = "127.0.0.1:$port"
nats_url = "$NATS_URL"
classes_api_url = "http://127.0.0.1:$(port_of classes prod)"
assets_api_url = "http://127.0.0.1:$(port_of assets prod)"
EOF
            ;;
        products)
            # Finished-product catalog + per-location on-hand. NATS
            # publisher gates the products.* state events the
            # rebuild path (and downstream /shop / /products UI)
            # consume. classes_api_url backs product_kind +
            # package_unit validation against the `product` Classes.
            cat <<EOF
# Managed by infra/deploy-services.sh — edits will be overwritten.
postgres_url = "$PROD_DB_URL"
http_bind = "127.0.0.1:$port"
nats_url = "$NATS_URL"
classes_api_url = "http://127.0.0.1:$(port_of classes prod)"
EOF
            ;;
        campaigns)
            # Marketing campaigns (Q4 domain home). No nats_url:
            # campaigns emits via the transactional outbox (#118);
            # boss-event-relay moves staged events onward.
            cat <<EOF
# Managed by infra/deploy-services.sh — edits will be overwritten.
postgres_url = "$PROD_DB_URL"
http_bind = "127.0.0.1:$port"
EOF
            ;;
        customers)
            # DTC customers (Q4 domain home). Same outbox-only shape
            # as campaigns.
            cat <<EOF
# Managed by infra/deploy-services.sh — edits will be overwritten.
postgres_url = "$PROD_DB_URL"
http_bind = "127.0.0.1:$port"
EOF
            ;;
        dispatcher)
            # boss-dispatcher reads its config from env vars (see
            # DispatcherConfig::default in crates/core/boss-dispatcher).
            # The emit_unit special case below pipes the same values
            # in as Environment= directives. This TOML stub exists
            # only so the deploy plan check has a path to diff
            # against — the binary doesn't open it.
            cat <<EOF
# Managed by infra/deploy-services.sh — edits will be overwritten.
# boss-dispatcher is env-var driven; see the systemd unit for the
# canonical values.
EOF
            ;;
        observability)
            # Cross-VM Cybernetics aggregator. Single-VM v1 deploys
            # leave [[vms]] empty and rely on [demo_agents] to populate
            # /api/snapshot with synthetic agent telemetry; the SPA
            # renders an honest "demo mode" banner on /ops in that
            # case. Multi-VM deploys would set [[vms]] entries and
            # remove [demo_agents] — out of scope for v1 single-VM.
            cat <<EOF
# Managed by infra/deploy-services.sh — edits will be overwritten.
bind = "127.0.0.1:$port"
nats_url = "$NATS_URL"

[demo_agents]
tick_seconds = 8
EOF
            ;;
        simulator)
            # boss-simulator is env-var driven (BOSS_SIM_BIND /
            # BOSS_SIM_STATIC_DIR / BOSS_JOBS_URL / BOSS_CLOCK_URL); see
            # the systemd unit. This stub keeps the config path uniform.
            cat <<EOF
# Managed by infra/deploy-services.sh — edits will be overwritten.
# boss-simulator is env-var driven; see the systemd unit.
EOF
            ;;
        *)
            echo "emit_solo_config: unknown service '$name'" >&2
            return 1
            ;;
    esac
}

# Service-name shape. Most boss services ship as `boss-<name>-api`
# with config at /etc/boss-<name>-api.toml — but a handful (today:
# observability, which is a NATS aggregator + dashboard backend
# rather than a CRUD API) ship without the `-api` suffix. Centralise
# the mapping so unit names, config paths, binary names, and health-
# probe URLs all stay in sync.
stem_for() {
    case "$1" in
        observability) echo "boss-observability" ;;
        dispatcher)    echo "boss-dispatcher" ;;
        simulator)     echo "boss-simulator" ;;
        *)             echo "boss-$1-api" ;;
    esac
}

# Emit the systemd unit body for a service in a given env.
# `kind` is "paired" or "solo"; `env` is "prod" or "scratch".
emit_unit() {
    local kind="$1" name="$2" env="$3"
    local unit_name desc cfg_path after working_dir=""
    local label
    local stem
    stem=$(stem_for "$name")
    if [[ "$env" == "scratch" ]]; then
        unit_name="${stem}-scratch.service"
        cfg_path="/etc/${stem}-scratch.toml"
        label=" (scratch stack)"
    else
        unit_name="${stem}.service"
        cfg_path="/etc/${stem}.toml"
        label=""
    fi

    desc="Boss $(description_of "$name")${label}"

    # After= ordering. Paired services all use NATS + Postgres.
    # Solo services use Postgres only (ml, docs, sim).
    after="network-online.target postgresql.service"
    if [[ "$kind" == "paired" ]]; then
        after="$after nats-server.service"
    fi

    # sim and docs need the repo as CWD so they can read local files.
    if [[ "$name" == "sim" || "$name" == "docs" ]]; then
        working_dir="WorkingDirectory=$REPO_ROOT"
    fi

    # Per-service environment injection. Policy takes its port from env;
    # the config file is a stub for uniformity.
    local extra_env=""
    if [[ "$name" == "policy" ]]; then
        extra_env="Environment=BOSS_POLICY_PORT=$(port_of policy prod)"
    elif [[ "$name" == "clock" ]]; then
        # boss-clock-api reads everything from env vars; the
        # deploy emits wall mode by default. Operators flip a
        # demo deploy to sim via
        # `sudo systemctl edit boss-clock-api` adding
        # `Environment=BOSS_CLOCK_MODE=sim` +
        # `BOSS_SIM_TICK_INTERVAL_MS=10000` for the canonical
        # 1-day-per-10-wall-sec demo cadence.
        extra_env=$'Environment=BOSS_CLOCK_MODE=wall\nEnvironment=BOSS_SIM_TICK_SIZE_SECONDS=86400\nEnvironment=BOSS_SIM_TICK_INTERVAL_MS=0\nEnvironment=BOSS_POSTGRES_URL='"$PROD_DB_URL"
    elif [[ "$name" == "search" || "$name" == "views" ]]; then
        # Both are clap+env binaries: they take no --config (see the
        # exec_start branch below) and require the Postgres URL from
        # the environment. Without this the generated unit starts a
        # binary with no `--postgres-url` and it exits immediately.
        # The policy URL falls back to the boss_ports default, same as
        # every other consumer.
        extra_env="Environment=BOSS_POSTGRES_URL=$PROD_DB_URL"
    elif [[ "$name" == "dispatcher" ]]; then
        # boss-dispatcher reads everything from env vars: URLs for every
        # downstream API its handlers post into (commerce/products/
        # shipping/ledger/inventory/people/jobs), plus the Postgres URL —
        # it loads its rule registry from the `dispatcher_rules` table.
        extra_env=$'Environment=BOSS_NATS_URL='"$NATS_URL"
        extra_env=$"${extra_env}"$'\nEnvironment=BOSS_JOBS_URL=http://127.0.0.1:'"$(port_of jobs prod)"
        extra_env=$"${extra_env}"$'\nEnvironment=BOSS_PEOPLE_URL=http://127.0.0.1:'"$(port_of people prod)"
        extra_env=$"${extra_env}"$'\nEnvironment=BOSS_INVENTORY_URL=http://127.0.0.1:'"$(port_of inventory prod)"
        extra_env=$"${extra_env}"$'\nEnvironment=BOSS_COMMERCE_URL=http://127.0.0.1:'"$(port_of commerce prod)"
        extra_env=$"${extra_env}"$'\nEnvironment=BOSS_PRODUCTS_URL=http://127.0.0.1:'"$(port_of products prod)"
        extra_env=$"${extra_env}"$'\nEnvironment=BOSS_SHIPPING_URL=http://127.0.0.1:'"$(port_of shipping prod)"
        extra_env=$"${extra_env}"$'\nEnvironment=BOSS_LEDGER_URL=http://127.0.0.1:'"$(port_of ledger prod)"
        extra_env=$"${extra_env}"$'\nEnvironment=BOSS_POSTGRES_URL='"$PROD_DB_URL"
        # Step-assignment distribution strategy, named as explicit data
        # rather than left to the binary's default. `spread` fans each
        # ready step across its role's active holders by a stable hash of
        # the step id (deterministic); `lowest-id` is the legacy single-
        # holder behavior. Unknown value → dispatcher warns + falls back to
        # spread, so a typo here can't take assignment down.
        extra_env=$"${extra_env}"$'\nEnvironment=BOSS_DISPATCH_STRATEGY=spread'
        # Drives the collections loop: the webhook.notify rule pushes
        # step.done.billing here, where the brewery-sim's callback receiver
        # (BOSS_SIM_CALLBACK_BIND=127.0.0.1:7099) consumes it. Without this the
        # counterparty stays dark and zero collections fire (AR balloons).
        extra_env=$"${extra_env}"$'\nEnvironment=BOSS_EVENT_WEBHOOK_URL=http://127.0.0.1:7099/callback'
    elif [[ "$name" == "simulator" ]]; then
        # boss-simulator serves the apps/simulator bundle from this dir
        # — IN the generation, like the gateway's web-dist, so a revert
        # rolls the cockpit back with the code (deployment-as-network
        # Q2). infra/deploy-web.sh builds and stages it. It used to
        # point at a fixed /var/lib path that only a hand-run script
        # ever wrote, so the playground had no bundle at all and the
        # service served its "no SPA bundle found" stub.
        # BOSS_SIM_BIND + the jobs/clock URLs fall back to boss_ports
        # defaults.
        extra_env="Environment=BOSS_SIM_STATIC_DIR=$BOSS_GEN_ROOT/current/simulator-dist"
    fi

    # Env-driven services don't take a --config flag. `search` and
    # `views` are clap+env binaries like clock/dispatcher/simulator;
    # omitting them here generated `--config /etc/boss-search-api.toml`,
    # which those binaries reject outright ("unexpected argument
    # '--config' found") — so the unit would have failed to start on
    # the next deploy. Both only run today because their units were
    # written by hand.
    #
    # ExecStart goes THROUGH the `current` symlink (deployment-as-
    # network Q1): a restart always execs the live generation, and a
    # revert re-points every service with one symlink flip. Hand-
    # authored units keep /usr/local/bin/<name> paths — those are
    # symlinks into current/bin/ (see ensure_bin_links), so they ride
    # the same flip.
    local exec_start
    if [[ "$name" == "clock" || "$name" == "dispatcher" || "$name" == "simulator" \
          || "$name" == "search" || "$name" == "views" ]]; then
        exec_start="ExecStart=$BOSS_GEN_ROOT/current/bin/${stem}"
    else
        exec_start="ExecStart=$BOSS_GEN_ROOT/current/bin/${stem} --config $cfg_path"
    fi

    cat <<EOF
[Unit]
Description=$desc
After=$after
Wants=network-online.target

[Service]
Type=simple
Environment=RUST_LOG=info
${extra_env:+$extra_env
}${exec_start}
${working_dir:+$working_dir
}Restart=on-failure
RestartSec=2

[Install]
WantedBy=multi-user.target
EOF
}

# ---------------------------------------------------------------------------
# Plan / apply
# ---------------------------------------------------------------------------

plan_one() {
    local kind="$1" name="$2" env="$3"
    local cfg_body unit_body cfg_path unit_path
    local stem
    stem=$(stem_for "$name")

    if [[ "$kind" == "paired" ]]; then
        cfg_body=$(emit_paired_config "$name" "$env")
    else
        cfg_body=$(emit_solo_config "$name")
    fi
    unit_body=$(emit_unit "$kind" "$name" "$env")

    if [[ "$env" == "scratch" ]]; then
        cfg_path="/etc/${stem}-scratch.toml"
        unit_path="/etc/systemd/system/${stem}-scratch.service"
    else
        cfg_path="/etc/${stem}.toml"
        unit_path="/etc/systemd/system/${stem}.service"
    fi

    if [[ "$MODE" == "check" ]]; then
        diff_file "$cfg_path" "$cfg_body"
        diff_file "$unit_path" "$unit_body"
        return 0
    fi

    printf '%s\n' "$cfg_body"  > "$cfg_path"
    printf '%s\n' "$unit_body" > "$unit_path"
    echo "  wrote $cfg_path"
    echo "  wrote $unit_path"
}

diff_file() {
    local path="$1" expected="$2"
    if [[ ! -f "$path" ]]; then
        echo "MISSING  $path"
        return 0
    fi
    local actual
    actual=$(cat "$path")
    if [[ "$actual" == "$expected" ]]; then
        echo "ok       $path"
    else
        echo "DIFF     $path"
        diff -u <(echo "$actual") <(echo "$expected") | sed 's/^/    /' || true
    fi
}

run_env() {
    local env="$1"
    echo "==> ${MODE} env=$env"
    for entry in "${PAIRED_SERVICES[@]}"; do
        IFS=: read -r name _ _ <<<"$entry"
        plan_one paired "$name" "$env"
    done
    if [[ "$env" == "prod" ]]; then
        for entry in "${SOLO_SERVICES[@]}"; do
            IFS=: read -r name _ <<<"$entry"
            plan_one solo "$name" prod
        done
    fi
}

# ---------------------------------------------------------------------------
# Generation store + probe machinery (deployment-as-network Q1–Q4)
# ---------------------------------------------------------------------------

declare -a TO_DEPLOY=()
add_units_for_env() {
    local env="$1"
    local stem
    for entry in "${PAIRED_SERVICES[@]}"; do
        IFS=: read -r name _ _ <<<"$entry"
        stem=$(stem_for "$name")
        if [[ "$env" == "scratch" ]]; then
            TO_DEPLOY+=("${stem}-scratch.service")
        else
            TO_DEPLOY+=("${stem}.service")
        fi
    done
    if [[ "$env" == "prod" ]]; then
        for entry in "${SOLO_SERVICES[@]}"; do
            IFS=: read -r name _ <<<"$entry"
            stem=$(stem_for "$name")
            TO_DEPLOY+=("${stem}.service")
        done
    fi
}

# Health probes. probe_one prints the status code AND records failures
# in PROBE_FAILED — the deploy path reports them, `probe` mode (what
# boss-deploy-confirm evaluates) exits nonzero on any of them. One
# roster for both, so the confirm cannot drift from the deploy list.
declare -a PROBE_FAILED=()
probe_one() {
    local kind="$1" name="$2" env="$3"
    local port
    port=$(port_of "$name" "$env")
    # Most services mount routes under /api/<name>; boss-docs-api is
    # named "docs" internally but serves at /api/design/*; boss-
    # observability is a non-`-api` service that mounts /api/health
    # directly (no per-service prefix) — it's a NATS aggregator, not
    # a domain-CRUD service.
    local url
    case "$name" in
        docs)          url="http://127.0.0.1:${port}/api/design/health" ;;
        simulator)     url="http://127.0.0.1:${port}/simulator/api/health" ;;
        observability) url="http://127.0.0.1:${port}/api/health" ;;
        *)             url="http://127.0.0.1:${port}/api/${name}/health" ;;
    esac
    local code
    code=$(curl -s -o /dev/null -w "%{http_code}" --max-time 3 "$url" || echo "???")
    local label="${name}-${env}"
    [[ "$code" == "200" ]] || PROBE_FAILED+=("${label}=${code}")
    printf '  %-20s  GET %-45s  %s\n' "$label" "$url" "$code"
}

run_probes() {
    local target="$1"
    PROBE_FAILED=()
    for entry in "${PAIRED_SERVICES[@]}"; do
        IFS=: read -r name _ _ <<<"$entry"
        if [[ "$target" == "prod" || "$target" == "both" ]]; then probe_one paired "$name" prod; fi
        if [[ "$target" == "scratch" || "$target" == "both" ]]; then probe_one paired "$name" scratch; fi
    done
    if [[ "$target" == "prod" || "$target" == "both" ]]; then
        for entry in "${SOLO_SERVICES[@]}"; do
            IFS=: read -r name _ <<<"$entry"
            probe_one solo "$name" prod
        done
    fi
}

# Read-only front-door check (the apply path has a separate block that
# also STARTS a downed gateway; a probe must not mutate).
probe_front_door() {
    local code
    code=$(curl -s -o /dev/null -w "%{http_code}" --max-time 5 http://127.0.0.1:4443/ || echo "000")
    printf '  %-20s  GET %-45s  %s\n' "gateway(front-door)" "http://127.0.0.1:4443/" "$code"
    [[ "$code" =~ ^(200|302|401|403)$ ]] || PROBE_FAILED+=("gateway-front-door=${code}")
}

restart_deployed_units() {
    local unit
    for unit in "${TO_DEPLOY[@]}"; do
        systemctl enable "$unit" >/dev/null 2>&1 || true
        systemctl restart "$unit"
        echo "  restarted $unit"
    done
}

# Bounce a unit only when it is running — non-demo hosts keep e.g.
# boss-brewery-sim down on purpose.
restart_if_active() {
    local unit="$1"
    if systemctl is-active --quiet "$unit" 2>/dev/null; then
        systemctl restart "$unit" && echo "  restarted $unit"
    fi
}

restart_prod_daemons() {
    local stem
    for entry in "${DAEMONS[@]}"; do
        IFS=: read -r stem _ <<<"$entry"
        if [[ -f "/etc/systemd/system/${stem}.service" ]]; then
            systemctl enable "${stem}.service" >/dev/null 2>&1 || true
            if [[ "${stem}" == "boss-train" ]]; then
                # Never restart the conductor synchronously: this deploy
                # is routinely a CHILD of boss-train's own reconcile verb,
                # and a synchronous restart kills the deploy mid-run (the
                # PR-7 stall / suicide-deploy incident, 2026-08-13). The
                # bounce fires from its own transient unit, outside the
                # conductor's cgroup, after this script exits. An
                # already-scheduled bounce covers this run.
                if systemd-run --collect --on-active=15s \
                    --unit=boss-train-restart \
                    systemctl restart boss-train.service >/dev/null 2>&1; then
                    echo "  boss-train.service restart scheduled (+15s, detached)"
                else
                    echo "  boss-train.service restart already scheduled"
                fi
            else
                systemctl restart "${stem}.service"
                echo "  restarted ${stem}.service"
            fi
        fi
    done
}

# /usr/local/bin/boss-* as symlinks THROUGH the current symlink.
# Hand-authored units (the maintenance timers, boss-event-relay,
# boss-brewery-sim's out-of-band unit) and service shell-outs
# (jobs-api → boss-rebuild-all) all say /usr/local/bin/<name>; making
# each of those a link to current/bin/<name> means one flip re-points
# every one of them. Idempotent, and it also repairs links that a
# bootstrap-vm run overwrote with real files.
ensure_bin_links() {
    local gen_bin="$BOSS_GEN_ROOT/current/bin"
    local f name link
    [[ -d "$gen_bin" ]] || return 0
    for f in "$gen_bin"/*; do
        [[ -f "$f" ]] || continue
        name="$(basename "$f")"
        link="/usr/local/bin/$name"
        if [[ "$(readlink "$link" 2>/dev/null || true)" == "$gen_bin/$name" ]]; then
            continue
        fi
        gen_atomic_link "$gen_bin/$name" "$link"
        echo "  linked $link -> $gen_bin/$name"
    done
}

# Q3: keep the GEN_KEEP newest generations, prune the rest — and say
# what was removed AND how big it was (this box has had its disk-full
# day; a prune that frees space silently teaches nobody anything).
# The current/previous targets are never pruned, whatever their age.
prune_generations() {
    if [[ ! -d "$GEN_RELEASES" ]]; then
        echo "  no generation store at $GEN_RELEASES — nothing to prune"
        return 0
    fi
    local cur prev
    cur="$(readlink -f "$BOSS_GEN_ROOT/current" 2>/dev/null || true)"
    prev="$(readlink -f "$BOSS_GEN_ROOT/previous" 2>/dev/null || true)"
    # Newest first by directory mtime (set once, at activation swap).
    # Staging/retire temp dirs are dot-prefixed, so ls skips them.
    local -a gens=()
    local g
    while IFS= read -r g; do gens+=("$g"); done < <(ls -1t "$GEN_RELEASES" 2>/dev/null)
    local idx=0 pruned=0 path rp size
    if (( ${#gens[@]} > 0 )); then
        for g in "${gens[@]}"; do
            path="$GEN_RELEASES/$g"
            [[ -d "$path" && ! -L "$path" ]] || continue
            idx=$((idx + 1))
            (( idx <= GEN_KEEP )) && continue
            rp="$(readlink -f "$path")"
            if [[ -n "$cur" && "$rp" == "$cur" ]] || [[ -n "$prev" && "$rp" == "$prev" ]]; then
                echo "  keeping $g (current/previous points at it)"
                continue
            fi
            size="$(du -sh "$path" 2>/dev/null | cut -f1)"
            rm -rf "$path"
            echo "  pruned generation $g (${size:-?})"
            gen_log "prune sha=$g size=${size:-?}"
            pruned=$((pruned + 1))
        done
    fi
    if (( pruned == 0 )); then
        echo "  nothing to prune ($idx generation(s) on disk, keeping $GEN_KEEP)"
    fi
    return 0
}

# The make-before-break rollback: flip current <-> previous, restart.
# Seconds, no build — the previous generation never left the disk.
# Called by boss-deploy-confirm on a failed reading, and by operators.
do_revert() {
    if [[ "$(id -u)" != "0" ]]; then
        echo "error: revert needs root (it restarts the fleet)" >&2
        exit 1
    fi
    local cur_key prev_key
    cur_key="$(gen_link_key current)"
    prev_key="$(gen_link_key previous)"
    if [[ -z "$cur_key" || -z "$prev_key" ]]; then
        echo "error: revert needs both current and previous generations" >&2
        echo "       current=${cur_key:-unset} previous=${prev_key:-unset} (store: $BOSS_GEN_ROOT)" >&2
        exit 1
    fi
    if [[ "$cur_key" == "$prev_key" ]]; then
        echo "error: current and previous both point at $cur_key — nothing to revert to" >&2
        exit 1
    fi
    echo "==> revert: current $cur_key -> $prev_key (symlink flip, no build)"
    gen_atomic_link "releases/$prev_key" "$BOSS_GEN_ROOT/current"
    gen_atomic_link "releases/$cur_key" "$BOSS_GEN_ROOT/previous"
    # A standing unconfirmed marker refers to the generation being
    # rolled away — clear it so the dead-man cannot fire on (and try
    # to "revert") the revert itself.
    rm -f "$GEN_PENDING"
    gen_log "revert current=$prev_key was=$cur_key"
    ensure_bin_links
    echo "==> restart services on the reverted generation"
    add_units_for_env prod
    restart_deployed_units
    restart_if_active boss-gateway.service
    restart_if_active boss-brewery-sim.service
    restart_prod_daemons
    echo "revert done — current=$prev_key previous=$cur_key"
    echo "(scratch units, if running, pick the reverted generation up on their next restart)"
}

case "$MODE" in
    probe)
        echo "==> strict health probes (env=$TARGET)"
        run_probes "$TARGET"
        if [[ "$TARGET" == "prod" || "$TARGET" == "both" ]]; then
            probe_front_door
        fi
        if (( ${#PROBE_FAILED[@]} > 0 )); then
            echo "probe FAILED (${#PROBE_FAILED[@]}): ${PROBE_FAILED[*]}" >&2
            exit 1
        fi
        echo "probe ok — every roster probe answered 200"
        exit 0
        ;;
    revert)
        do_revert
        exit 0
        ;;
    prune)
        echo "==> prune generation store (keep $GEN_KEEP newest)"
        prune_generations
        exit 0
        ;;
esac

if [[ "$TARGET" == "both" ]]; then
    run_env prod
    run_env scratch
else
    run_env "$TARGET"
fi

if [[ "$MODE" == "check" ]]; then
    exit 0
fi

echo "==> systemctl daemon-reload"
systemctl daemon-reload

# boss-dispatcher's rule registry now lives in the `dispatcher_rules`
# table (seeded by 41-dispatcher.sql, authored from infra/dispatcher/
# rules.toml via gen-seed.py) and is loaded at startup — no rules.toml
# file deploy.

# Resolve target dir — may be a symlink (`/opt/boss/target` →
# `/var/lib/boss-build/target` per `infra/dev-bootstrap`) or a real
# directory (fresh checkout, or after a disk-cleanup that removed
# the symlink). `readlink -f` collapses both.
TARGET_DIR=$(readlink -f "$REPO_ROOT/target")
RELEASE_DIR="$TARGET_DIR/release"

# CANONICAL BUILD: infra/build-release.sh — run it before this script.
# It reads each bin's `required-features` straight from `cargo metadata`
# and builds every gated bin (postgres, plus per-service `<svc>-api`
# umbrella features like accounts-api / events-api). A plain
# `cargo build --release --workspace` instead leaves *in-memory* binaries
# for the many `default = []` service crates — they boot, then exit 1, or
# silently serve a store that loses every write. There is deliberately no
# hand-maintained feature list here: the old one drifted to 7 of the 37
# gated bins and silently shipped stale/in-memory binaries every deploy.
#
echo "==> plan deployable units (binaries from $RELEASE_DIR)"
echo "    (build first with: ./infra/build-release.sh)"
if [[ "$TARGET" == "both" ]]; then
    add_units_for_env prod
    add_units_for_env scratch
else
    add_units_for_env "$TARGET"
fi

# ---------------------------------------------------------------------
# Freshness PRE-FLIGHT — check everything before installing anything.
#
# Two defects, both found on 2026-08-08, both fixed here.
#
# 1. `install_binary` used to `exit 1` the moment it met a stale
#    binary, from INSIDE the install loop. That aborted the deploy
#    partway: some binaries already written to /usr/local/bin, none of
#    the services restarted, and the run ending on a line that reads
#    like a note rather than a failure. It installed ten binaries, hit
#    a stale one, and stopped — leaving new code on disk with
#    five-hour-old processes still serving it, while having printed
#    `installed boss-jobs-api` for each. The mismatch took an hour to
#    find because nothing said "aborted". So: validate first, act
#    second. Either every binary is good and the deploy runs end to
#    end, or nothing is touched.
#
# 2. The check itself compared MTIMES, which git rewrites. A rebase or
#    a branch switch restamps every file it rewrites, so a clean tree
#    whose binaries were built from exactly that content was reported
#    stale — demanding a 50-minute rebuild for byte-identical output.
#    Now it compares a content fingerprint recorded by the build; see
#    infra/src-fingerprint.sh.
# ---------------------------------------------------------------------
SRC_FP="$("$REPO_ROOT/infra/src-fingerprint.sh" 2>/dev/null || true)"
BUILT_FP="$(cat "$RELEASE_DIR/.boss-src-fingerprint" 2>/dev/null || true)"

declare -a MISSING_BINS=()
for unit in "${TO_DEPLOY[@]}" ; do
    bin_name="${unit%.service}"
    bin_name="${bin_name%-scratch}"
    [[ -f "$RELEASE_DIR/$bin_name" ]] || MISSING_BINS+=("$bin_name")
done

# An empty fingerprint on either side means "cannot tell" — a tarball
# deployment has no git metadata, and a build predating the stamp has
# no record. Neither is evidence of staleness, so neither blocks.
if [[ -n "$SRC_FP" && -n "$BUILT_FP" && "$SRC_FP" != "$BUILT_FP" ]]; then
    {
        echo
        echo "!! DEPLOY ABORTED — nothing installed, nothing restarted."
        echo
        echo "   The release binaries were built from different sources"
        echo "   than the tree on disk."
        echo "     built from : $BUILT_FP"
        echo "     tree is at : $SRC_FP"
        echo
        echo "   Fix: ./infra/build-release.sh   (then re-run this script)"
        echo
        echo "   The box is untouched and still serving the previous build,"
        echo "   which is the correct state to be in after a failed deploy."
    } >&2
    exit 1
fi
if [[ -z "$BUILT_FP" ]]; then
    echo "  note: no build stamp in $RELEASE_DIR — cannot verify the binaries"
    echo "        match this tree. Re-run ./infra/build-release.sh to record one."
fi
if (( ${#MISSING_BINS[@]} > 0 )); then
    echo "  note: not built, will be skipped: ${MISSING_BINS[*]}"
fi

# ---------------------------------------------------------------------
# Generation staging (deployment-as-network Q1/Q2). Nothing the fleet
# runs is touched here: binaries + carried web assets land in a
# dot-prefixed staging dir, one rename makes it releases/<sha>/, and
# the `current` symlink flip below is the whole activation. Re-running
# the sha that is already live skips the copy — the build stamp proves
# the standing generation IS this build — and just re-activates, so a
# live generation is never clobbered mid-copy.
# ---------------------------------------------------------------------
GEN_KEY="$(gen_head_key "$REPO_ROOT")"
if [[ -z "$GEN_KEY" ]]; then
    # No git metadata (tarball deploy). The store still works; the key
    # is just not a sha. Content is still pinned by the build stamp.
    GEN_KEY="unversioned"
fi
GEN_DIR="$GEN_RELEASES/$GEN_KEY"
GEN_STAGE="$GEN_RELEASES/.staging-$GEN_KEY"
GEN_REUSE=0
GEN_STAMP="$(cat "$GEN_DIR/.boss-src-fingerprint" 2>/dev/null || true)"
if [[ -d "$GEN_DIR" && -n "$BUILT_FP" && "$GEN_STAMP" == "$BUILT_FP" ]]; then
    GEN_REUSE=1
    echo "==> generation $GEN_KEY already staged from this exact build — re-activating"
else
    echo "==> stage generation $GEN_KEY"
    rm -rf "$GEN_RELEASES"/.staging-* 2>/dev/null || true
    mkdir -p "$GEN_STAGE/bin"
    # Seed the bin set from what is live today, so a partial build (or
    # a scratch-only deploy, which stages no helper binaries) can never
    # produce a generation MISSING a binary the fleet needs. Freshly
    # built binaries overwrite their seed copy right below.
    if [[ -d "$BOSS_GEN_ROOT/current/bin" ]]; then
        cp -a "$BOSS_GEN_ROOT/current/bin/." "$GEN_STAGE/bin/"
        echo "  seeded bin/ from generation $(gen_link_key current)"
        # Adopt a real-file /usr/local/bin/boss the same way the first
        # generation adopts pre-generation binaries: generations that
        # predate the CLI joining the roster carry no bin/boss to
        # seed, and the live file is then the only copy on the box. A
        # freshly built CLI overwrites this seed right below;
        # ensure_bin_links turns the real file into a symlink through
        # `current` at activation either way.
        if [[ ! -f "$GEN_STAGE/bin/boss" \
              && -f /usr/local/bin/boss && ! -L /usr/local/bin/boss ]]; then
            cp -a /usr/local/bin/boss "$GEN_STAGE/bin/"
            echo "  adopted real-file /usr/local/bin/boss into the generation"
        fi
    else
        # First-ever generation: adopt the real files the
        # pre-generation deploys left in /usr/local/bin (they become
        # symlinks through `current` at activation). The bare `boss`
        # CLI is named alongside the boss-* glob — the glob misses it,
        # and a generation without the CLI leaves boss-train.service
        # and conductor.sh on whatever stale file the box carries.
        seeded=0
        for f in /usr/local/bin/boss /usr/local/bin/boss-*; do
            [[ -f "$f" && ! -L "$f" ]] || continue
            cp -a "$f" "$GEN_STAGE/bin/"
            seeded=$((seeded + 1))
        done
        echo "  seeded bin/ with $seeded binar(ies) from /usr/local/bin (first generation)"
    fi
    # Q2: web dist + simulator dist + step-plugins live IN the
    # generation, so a revert rolls the UI back with the code. Carry
    # the currently served assets forward so the flip never serves an
    # empty SPA; deploy-web.sh then overwrites them with the freshly
    # built bundles.
    for asset in web-dist simulator-dist step-plugins; do
        case "$asset" in
            web-dist)       legacy="/var/lib/boss-web/dist" ;;
            simulator-dist) legacy="/var/lib/boss-simulator/dist" ;;
            *)              legacy="/var/lib/boss/step-plugins" ;;
        esac
        asset_src=""
        if [[ -d "$GEN_DIR/$asset" ]]; then
            asset_src="$GEN_DIR/$asset"            # restage of this sha
        elif [[ -d "$BOSS_GEN_ROOT/current/$asset" ]]; then
            asset_src="$BOSS_GEN_ROOT/current/$asset"
        elif [[ -d "$legacy" ]]; then
            asset_src="$legacy"                    # first generation
        fi
        mkdir -p "$GEN_STAGE/$asset"
        if [[ -n "$asset_src" ]]; then
            cp -a "$asset_src/." "$GEN_STAGE/$asset/"
            echo "  carried $asset from $asset_src"
        else
            echo "  note: no $asset found to carry — run ./infra/deploy-web.sh to populate it"
        fi
    done
fi

# Stage a binary into the generation being assembled. `install` is
# still the copy tool (write+rename), but the destination is the
# staging dir — /usr/local/bin only ever holds symlinks through
# `current` (see ensure_bin_links), so nothing live is overwritten
# mid-copy. Skips silently when the source binary isn't built — the
# seed copy above keeps the previous build of it in the generation,
# same net effect as the old partial-deploy behavior.
stage_binary() {
    local bin_name="$1"
    local src="$RELEASE_DIR/$bin_name"
    if (( GEN_REUSE )); then
        return 0
    fi
    if [[ ! -f "$src" ]]; then
        echo "  SKIP $bin_name (not built at $src)"
        return 0
    fi
    # No per-binary freshness check here. It used to compare this
    # binary's mtime against the newest source mtime and `exit 1` on
    # failure — from inside the install loop, which is what left the
    # box half-deployed. The question "were these binaries built from
    # this tree" is answered once, up front, by the fingerprint
    # pre-flight, and it is a property of the BUILD rather than of each
    # file's timestamp.
    install -m 0755 "$src" "$GEN_STAGE/bin/$bin_name"
    echo "  staged $bin_name"
}
if (( ! GEN_REUSE )); then
    echo "==> stage service binaries"
fi
for unit in "${TO_DEPLOY[@]}"; do
    # boss-shipping-api.service → boss-shipping-api;
    # boss-shipping-api-scratch.service → boss-shipping-api
    # (scratch units run the same binary against a different config).
    bin_name="${unit%.service}"
    bin_name="${bin_name%-scratch}"
    stage_binary "$bin_name"
done

# Daemons + timers are environment-agnostic — they always land in
# prod. Skip on `scratch`-only deploys to avoid stomping prod state
# from a scratch run.
if [[ "$TARGET" == "prod" || "$TARGET" == "both" ]]; then
    # On-demand helper binaries with no systemd unit of their own — a
    # running service shells out to them. boss-jobs-api's demo-loop Reset
    # spawns boss-rebuild-all to re-derive projections after trimming
    # audit_log; left out of every install list, it silently rotted to a
    # pre-migration build and Reset failed mid-rebuild. Freshness-guarded
    # like every other binary, so a stale one now fails the deploy loudly.
    echo "==> stage on-demand helper binaries"
    stage_binary "boss-rebuild-all"

    # The operator CLI itself. boss-train.service and
    # infra/train/conductor.sh exec /usr/local/bin/boss — and twice on
    # the cadence-cutover night a stale real-file CLI (built Aug 7,
    # missing `train`, then `cadence`) broke the conductor, because no
    # roster ever staged or linked it. The generation owns it now:
    # built by infra/build-release.sh with the rest, staged here, and
    # linked through `current` by ensure_bin_links like every other
    # managed binary.
    stage_binary "boss"

    # boss-gateway + boss-brewery-sim aren't port-table *-api services and have
    # no DAEMONS entry, so no install list refreshed them — they rotted to
    # pre-migration builds the same way boss-rebuild-all did. Refresh +
    # freshness-guard the binaries and bounce them if running (the gateway
    # always; the sim only in a demo deployment, where its ExecStartPre gate
    # keeps it up). Their unit files are managed out-of-band.
    # Gateway unit + drop-ins, from the repo. Sync BEFORE the binary
    # refresh below, so a restart picks up config and code together.
    #
    # These were box-only until 2026-08-07, when infra/gateway/ held
    # exactly one file — demo-mode.conf, for a mode removed that day —
    # while the two live, load-bearing drop-ins existed nowhere in the
    # repo. A rebuild would have deployed dead config and lost both
    # authentication and guest access, and the directory would have
    # looked authoritative while doing it.
    #
    # Copied, not symlinked: systemd reads these as root at boot, and a
    # link into a user-writable checkout is a privilege path nobody
    # should have to think about.
    if [[ -d "$REPO_ROOT/infra/gateway" ]]; then
        echo "==> sync gateway unit + drop-ins from infra/gateway"
        install -m 0644 "$REPO_ROOT/infra/gateway/boss-gateway.service" \
            /etc/systemd/system/boss-gateway.service
        mkdir -p /etc/systemd/system/boss-gateway.service.d
        for conf in "$REPO_ROOT"/infra/gateway/*.conf; do
            [[ -e "$conf" ]] || continue
            install -m 0644 "$conf" "/etc/systemd/system/boss-gateway.service.d/$(basename "$conf")"
            echo "    $(basename "$conf")"
        done
        # Deliberately NOT deleting drop-ins absent from the repo. An
        # operator may add a machine-local one (a secret EnvironmentFile,
        # a host override), and a deploy that silently removed it would
        # be a worse surprise than a stale file. Removal stays manual.
        systemctl daemon-reload
    fi

    echo "==> stage non-port-table service binaries (gateway, brewery-sim)"
    for bin in boss-gateway boss-brewery-sim; do
        stage_binary "$bin"
    done

    echo "==> install daemon units + stage their binaries"
    # The install refreshes the unit FILE only. Per-host drop-ins
    # (/etc/systemd/system/<stem>.service.d/*.conf) are deliberately
    # never written or removed here — boss-train's forge + jobs-SoR
    # env contract rides in exactly such drop-ins (see the unit's
    # header), and a deploy that clobbered them would silently point
    # the conductor at the wrong forge and the wrong jobs instance.
    # Same removal-stays-manual policy as the gateway drop-ins above.
    for entry in "${DAEMONS[@]}"; do
        IFS=: read -r stem subdir <<<"$entry"
        src_dir="$REPO_ROOT/infra"
        [[ "$subdir" != "." ]] && src_dir="$src_dir/$subdir"
        svc_src="$src_dir/${stem}.service"
        if [[ ! -f "$svc_src" ]]; then
            echo "  SKIP $stem (missing $svc_src)"
            continue
        fi
        install -m 0644 "$svc_src" "/etc/systemd/system/${stem}.service"
        stage_binary "$stem"
        echo "  installed $stem unit"
    done

    echo "==> install timer units + stage their binaries"
    for entry in "${TIMERS[@]}"; do
        IFS=: read -r stem subdir <<<"$entry"
        src_dir="$REPO_ROOT/infra"
        [[ "$subdir" != "." ]] && src_dir="$src_dir/$subdir"
        svc_src="$src_dir/${stem}.service"
        tmr_src="$src_dir/${stem}.timer"
        if [[ ! -f "$svc_src" || ! -f "$tmr_src" ]]; then
            echo "  SKIP $stem (missing $svc_src or $tmr_src)"
            continue
        fi
        install -m 0644 "$svc_src" "/etc/systemd/system/${stem}.service"
        install -m 0644 "$tmr_src" "/etc/systemd/system/${stem}.timer"
        # Stage the timer's binary if it's been built. The unit stem
        # matches the binary name (e.g. boss-ledger-recognize); pure-
        # script timers (boss-deploy-confirm) have no binary and SKIP
        # here, which is fine.
        stage_binary "$stem"
        # A CHORE REPORTS TO THE INSTANCE WHOSE DATA IT MAINTAINS.
        #
        # These units' binaries run against the LOCAL database (their
        # BOSS_POSTGRES_URL is 127.0.0.1), so their packets belong on
        # the LOCAL jobs API — hard-set here, no ambient override.
        #
        # History, because this line has now been wrong in both
        # directions. 2026-08-17: nothing set BOSS_JOBS_URL, packets
        # opened and closed on the local instance "where nobody
        # looks", and the fix pointed reporting at the cluster SoR.
        # That fixed the visibility and broke the truth: the chore
        # still worked the LOCAL database, so the SoR's packet claimed
        # `result=ok` for work the SoR never received — measured
        # 2026-08-19, the cluster's search_index was EMPTY while its
        # reindex packets were being completed ok (and when the local
        # chore crashed, the SoR packet just sat at run=ready with no
        # SoR-side runner behind it). Work the SoR needs runs ON the
        # SoR: infra/cluster/manifests/boss-search-reindex.yaml is the
        # pattern, one CronJob per chore as each migrates.
        install -d -m 0755 "/etc/systemd/system/${stem}.service.d"
        cat > "/etc/systemd/system/${stem}.service.d/jobs-url.conf" <<UNIT
[Service]
Environment=BOSS_JOBS_URL=http://127.0.0.1:$(port_of jobs prod)
UNIT
        echo "  installed $stem unit + timer"
    done
    systemctl daemon-reload
fi

# ---------------------------------------------------------------------
# Converge the schema — after staging, BEFORE anything is activated or
# restarted.
#
# Code, config and schema all converge from the tree on every deploy.
# The train's deploy verb (boss-cli, `train.rs`) already runs migrate.sh
# before calling this script, so on that path this is a no-op that says
# so; a deploy driven by hand (`sudo ./infra/deploy-services.sh prod`,
# bootstrap-vm.sh step 8) had no schema step at all, and would happily
# restart new code against an old database. The cluster had exactly that
# gap on 2026-08-13 and shipped a 500 (`relation "stations" does not
# exist`) off a deploy that reported success.
#
# Placed before the generation flip on purpose: a migration failure then
# leaves the box completely untouched — old code still serving the
# database it was built for — which is the correct state after a failed
# deploy, the same property the fingerprint pre-flight above protects.
#
# migrate.sh is idempotent by ledger (only manifest entries missing from
# schema_migrations apply, each atomically with its bookkeeping row) and
# it prints what it applied plus an `applied N, already recorded M, of K
# manifest entries` summary. Failures are NOT swallowed: a half-migrated
# database that keeps serving is worse than a visible failure.
# ---------------------------------------------------------------------
converge_schema() {
    local label="$1" db_url="$2"
    echo "==> converge schema ($label)"
    if ! "$REPO_ROOT/infra/postgres/migrate.sh" -- psql "$db_url"; then
        {
            echo
            echo "!! DEPLOY ABORTED — schema converge failed on the $label database."
            echo
            echo "   Nothing was activated and nothing was restarted: the box is"
            echo "   still serving the previous generation against the database it"
            echo "   was built for. migrate.sh named the failing entry above (its"
            echo "   transaction rolled back, nothing from it was kept)."
            echo
            echo "   A database that predates the migration runner is refused until"
            echo "   adopted once, by hand:"
            echo "     ./infra/postgres/migrate.sh --baseline -- psql \"$db_url\""
        } >&2
        exit 1
    fi
}
if [[ "$TARGET" == "prod" || "$TARGET" == "both" ]]; then
    converge_schema prod "$PROD_DB_URL"
fi
if [[ "$TARGET" == "scratch" || "$TARGET" == "both" ]]; then
    converge_schema scratch "$SCRATCH_DB_URL"
fi

# ---------------------------------------------------------------------
# Activate: one rename completes the generation, one symlink flip makes
# it live. `previous` is re-pointed FIRST — if the run dies between the
# two flips, previous==current (revert refuses, harmlessly) instead of
# previous naming a two-generations-old build (revert to the wrong
# code).
# ---------------------------------------------------------------------
echo "==> activate generation $GEN_KEY"
if (( ! GEN_REUSE )); then
    # Stamp last: a generation without a stamp reads as an unfinished
    # stage and will be restaged on the next run, never reused.
    if [[ -f "$RELEASE_DIR/.boss-src-fingerprint" ]]; then
        install -m 0644 "$RELEASE_DIR/.boss-src-fingerprint" \
            "$GEN_STAGE/.boss-src-fingerprint"
    fi
    if [[ -d "$GEN_DIR" ]]; then
        # Restage of an existing sha (stamp drift, or an interrupted
        # earlier deploy). Two renames swap the dirs; running processes
        # hold their inodes, so nothing serving traffic is disturbed.
        mv -T "$GEN_DIR" "$GEN_RELEASES/.retire-$GEN_KEY.$$"
        mv -T "$GEN_STAGE" "$GEN_DIR"
        rm -rf "$GEN_RELEASES/.retire-$GEN_KEY.$$"
    else
        mv -T "$GEN_STAGE" "$GEN_DIR"
    fi
fi
GEN_PREVIOUS_KEY="$(gen_link_key current)"
if [[ "$GEN_PREVIOUS_KEY" == "$GEN_KEY" ]]; then
    # Re-deploy of the live sha: current already points here; previous
    # stays whatever it was.
    GEN_PREVIOUS_KEY="$(gen_link_key previous)"
    echo "  current already -> releases/$GEN_KEY (previous stays ${GEN_PREVIOUS_KEY:-unset})"
else
    if [[ -n "$GEN_PREVIOUS_KEY" ]]; then
        gen_atomic_link "releases/$GEN_PREVIOUS_KEY" "$BOSS_GEN_ROOT/previous"
    fi
    gen_atomic_link "releases/$GEN_KEY" "$BOSS_GEN_ROOT/current"
    echo "  current -> releases/$GEN_KEY (previous ${GEN_PREVIOUS_KEY:-unset})"
fi
gen_log "activate sha=$GEN_KEY previous=${GEN_PREVIOUS_KEY:-none} target=$TARGET"
ensure_bin_links

echo "==> restart services"
restart_deployed_units

if [[ "$TARGET" == "prod" || "$TARGET" == "both" ]]; then
    echo "==> restart gateway + brewery-sim (if running) and daemons"
    # The gateway always runs; the sim only in a demo deployment, where
    # its ExecStartPre gate keeps it up — bounce each only if active.
    restart_if_active boss-gateway.service
    restart_if_active boss-brewery-sim.service
    restart_prod_daemons

    echo "==> enable timers"
    for entry in "${TIMERS[@]}"; do
        IFS=: read -r stem _ <<<"$entry"
        if [[ -f "/etc/systemd/system/${stem}.timer" ]]; then
            systemctl enable --now "${stem}.timer" >/dev/null 2>&1 || true
            echo "  enabled ${stem}.timer"
        fi
    done

    # Retire the train's timer pair wherever it survives: the schedule
    # lives in cadence_rules now, and an enabled timer would keep the
    # old wall-clock boarding firing beside the rules.
    #
    # Retiring is TWO acts, not one: the unit FILES go, and the LOADED
    # units are stopped. On cutover night (2026-08-13) this sweep
    # deleted the files and the loaded timers kept firing from systemd
    # memory until a manual stop + daemon-reload — a unit file is only
    # the recipe; the armed timer is the process. So: stop the loaded
    # units unconditionally (never gated on the files existing —
    # that gate is exactly what skipped the stop once the files were
    # gone), remove the files, stop again for anything that fired in
    # between, then daemon-reload. Every arm no-ops when the units
    # are already gone, so the sweep is idempotent.
    for stem in "${RETIRED_TRAIN_TIMERS[@]}"; do
        for unit in "${stem}.timer" "${stem}.service"; do
            # disable --now covers the enabled case; the fallback stop
            # covers a unit already disabled but still loaded/running.
            systemctl disable --now "$unit" >/dev/null 2>&1 \
                || systemctl stop "$unit" >/dev/null 2>&1 \
                || true
        done
        if [[ -f "/etc/systemd/system/${stem}.timer" \
              || -f "/etc/systemd/system/${stem}.service" ]]; then
            rm -f "/etc/systemd/system/${stem}.timer" \
                  "/etc/systemd/system/${stem}.service"
            echo "  retired ${stem} timer+service (schedule moved to cadence_rules)"
        fi
        for unit in "${stem}.timer" "${stem}.service"; do
            systemctl stop "$unit" >/dev/null 2>&1 || true
        done
    done
    systemctl daemon-reload

    # Q4: arm the dead-man switch — a SEPARATE unit, never in-process
    # waiting here (a dead-man that dies with the deployer reverts
    # nothing; the 45-minute TimeoutStartSec kill mid-build was the
    # proof). `restart` resets the timer's monotonic clock so its
    # readings land at +2m/+8m from THIS flip; boss-deploy-confirm
    # evaluates the probe roster and flips current back to previous on
    # a failed reading.
    echo "==> arm deploy confirm (dead-man readings at +2m/+8m)"
    mkdir -p "$GEN_STATE"
    {
        echo "sha=$GEN_KEY"
        echo "previous=${GEN_PREVIOUS_KEY:-none}"
        echo "flipped_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    } > "$GEN_PENDING"
    if systemctl restart boss-deploy-confirm.timer 2>/dev/null; then
        echo "  armed boss-deploy-confirm.timer"
    else
        echo "  !! could not arm boss-deploy-confirm.timer — this deploy stands" >&2
        echo "     UNCONFIRMED with no dead-man; run infra/deploy-confirm.sh by hand." >&2
    fi
fi

echo "==> prune old generations (keep $GEN_KEEP)"
prune_generations

echo "==> health probes"
sleep 1
run_probes "$TARGET"
if (( ${#PROBE_FAILED[@]} > 0 )); then
    # Report, don't fail: the verdict on this deploy belongs to
    # boss-deploy-confirm, which re-reads the same roster at +2m/+8m
    # and reverts if the failure stands.
    echo "  note: ${#PROBE_FAILED[@]} probe(s) not 200 (${PROBE_FAILED[*]})"
    echo "        boss-deploy-confirm re-reads at +2m/+8m and auto-reverts if this persists."
fi

# Front door. deploy-services manages the API services but NOT the
# gateway (nor the sim) — so a `systemctl stop boss-*` + deploy-services
# reset would leave the gateway down and caddy would serve the "demo
# regenerating" splash even though every API above is healthy (this bit
# us 2026-06-19). Ensure + verify the gateway here so the bring-up can't
# silently leave the public face down.
if [[ "$TARGET" == "prod" || "$TARGET" == "both" ]] \
    && systemctl list-unit-files boss-gateway.service >/dev/null 2>&1; then
    if ! systemctl is-active --quiet boss-gateway.service; then
        echo "==> gateway not running — starting it (front door; not in the managed list)"
        systemctl enable --now boss-gateway.service >/dev/null 2>&1 || true
        systemctl start boss-gateway.service || true
        sleep 2
    fi
    gw_code=$(curl -s -o /dev/null -w "%{http_code}" --max-time 5 http://127.0.0.1:4443/ || echo "000")
    if [[ "$gw_code" =~ ^(200|302|401|403)$ ]]; then
        printf '  %-20s  GET %-45s  %s\n' "gateway(front-door)" "http://127.0.0.1:4443/" "$gw_code"
    else
        echo "  !! GATEWAY NOT RESPONDING ($gw_code) — the public face will show the" >&2
        echo "     'demo regenerating' splash until :4443 is up. Investigate boss-gateway." >&2
    fi
    # The sim (also unmanaged here) runs iff its unit is enabled; flag it if a
    # demo host left it down, but don't force-start (non-demo deploys
    # intentionally leave it off).
    if systemctl is-enabled --quiet boss-brewery-sim.service 2>/dev/null \
        && ! systemctl is-active --quiet boss-brewery-sim.service; then
        echo "  note: boss-brewery-sim is enabled but not active — start it if this is a demo host."
    fi
fi

echo "done."

# A deploy is a step of a regen when one is open, and a no-op otherwise.
# `|| true` because a deploy must not fail on bookkeeping: the services
# are already running by this point, and a Job that cannot be updated
# is a worse thing to abort a deploy over than to report.
# boss-step.sh REFUSES without a system of record now, so state it
# here rather than inherit it. Same default as the drop-in above: one
# expression, written twice, because the alternative was one export
# whose correctness depended on which block it sat in — and it sat in
# do_revert(), so a normal deploy never ran it and the heredoc below
# died on `set -u`. A default at each use site cannot be defeated by
# placement.
BOSS_JOBS_URL="${BOSS_JOBS_URL:-http://127.0.0.1:$(port_of jobs prod)}" \
    "$(dirname "$0")/boss-step.sh" regenerate-deployment deploy \
    "deployed=$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    || echo "WARN: deploy step NOT recorded on the regen Job (boss-step failed above)" >&2
