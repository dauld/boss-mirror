# sor-routes.sh — which SERVICE of the system of record a path belongs
# to, and how a base URL is re-pointed at that service's port. SOURCED,
# never run: by infra/forge/probe-bin/boss-sor-read (the forge's probe
# reader) and by infra/dev/boss-api (the pod's door), each from beside
# its own resolved file, so the two doors route by ONE set of rules.
#
# WHY ONE FILE (backlog de0989d2, measured 2026-09-17 14:50Z). The
# system of record is several services on one LAN IP (design 28d2bed9,
# boss-jobs-internal.yaml), each on the port boss-ports assigns it. The
# probe reader learned to pick the port from the path on 2026-09-17;
# the pod's door did not, so the first real reconcile step — an account
# created through POST /api/people/accounts, which boss-accounts serves
# on 7550 — could not be done through any door: the reader is GET-only
# by design, and boss-api sent every path to the jobs port, where an
# accounts path is a 404 about a surface that exists. Teaching boss-api
# the same rules as a second `case` would be a fact living twice
# (CLAUDE.md §9a), so the rules moved here and both doors source them.
#
# THE PIN. crates/core/boss-testing/tests/the_machine_door_carries_every_read_surface.rs
# lifts every `service=<name>` between the SOR-ROUTES markers below and
# holds the door's manifest and infra/forge/sor-ports.env equal to
# boss-ports for exactly those services; it also runs
# sor_service_for_path on the prefixes each router mounts, and checks
# that neither door carries a table of its own. To add a surface: a
# route here, a port line in the manifest, a line in sor-ports.env —
# the test refuses two of the three.
#
# Plain POSIX-ish bash, no `set` options of its own: it inherits the
# caller's, and both callers run with -u, so every variable here is
# declared before it is read.

# sor_service_for_path /api/path[?query] — prints the service name.
# The prefixes are the ones each service's router mounts; the query
# string is not part of the match. FIRST MATCH WINS, so the longer
# prefix is listed first: boss-accounts mounts UNDER /api/people/
# (accounts, support-cases, account-account-team, my-day/actions —
# crates/modules/boss-accounts/src/*.rs `.route(` lines) and is its own
# service on its own port; every other /api/people path is
# boss-people. The ledger (boss-ledger http.rs, all under /api/ledger/)
# and the dispatcher's rule registry (boss-dispatcher http.rs, all
# under /api/dispatcher/) joined on 2026-09-17 (backlog 77fd7b5a +
# 4145d2c1): the first real sponsorship sat at recognize with no door
# to post a journal entry through. Policy, the calendar and the two
# subject surfaces joined on 2026-09-22 (backlog ea3c8234): they are
# the three registries `boss tenant export` reads that no door carried,
# so an off-cluster export could read some of the instance and not the
# rest — and a partial snapshot rendered into the tenant repo deletes
# rows from the files it rewrites. `/api/subjects` and
# `/api/subject-kinds` are one service (boss-subject-kinds mounts both:
# subjects.rs and http.rs), and neither answered anywhere before this —
# they fell through to the jobs API, which serves neither. The file
# store joined on 2026-09-23 (backlog 7610dd2f): `boss attach` posts a
# file to boss-content-api's /api/files and reads it back by id, and a
# car's probe reads the same bytes — every /api/files path, including
# the `_audit` and `_upload-url` routes the same router mounts. The ML
# API joined the same day (backlog 9599babc): the nightly inference
# batch on boss-gcp POSTs /api/ml/models/<id>/infer-batch here, where
# it had been reaching the retired second stack's ML API on loopback,
# and a car's probe reads /api/ml/models the same way. Everything
# unlisted — jobs, steps, the yard, agents, sensors, workflows — is the
# jobs API.
sor_service_for_path() {
    local route="${1%%\?*}" service
    # SOR-ROUTES-BEGIN
    case "$route" in
        /api/people/accounts|/api/people/accounts/*)           service=accounts ;;
        /api/people/support-cases|/api/people/support-cases/*) service=accounts ;;
        /api/people/account-account-team/*)                    service=accounts ;;
        /api/people/my-day/actions)                            service=accounts ;;
        /api/events/*)                                         service=events ;;
        /api/people|/api/people/*)                             service=people ;;
        /api/classes|/api/classes/*)                           service=classes ;;
        /api/locations|/api/locations/*)                       service=locations ;;
        /api/ledger/*)                                         service=ledger ;;
        /api/dispatcher/*)                                     service=dispatcher ;;
        /api/policy|/api/policy/*)                             service=policy ;;
        /api/calendar|/api/calendar/*)                         service=calendar ;;
        /api/subject-kinds|/api/subject-kinds/*)               service=subject-kinds ;;
        /api/subjects|/api/subjects/*)                         service=subject-kinds ;;
        /api/files|/api/files/*)                               service=content ;;
        /api/ml|/api/ml/*)                                     service=ml ;;
        *)                                                    service=jobs ;;
    esac
    # SOR-ROUTES-END
    printf '%s\n' "$service"
}

# sor_port_for_service NAME "name=port name=port …" — prints the port
# the table gives NAME; returns 1 and prints nothing when the table
# names none (or names something that is not a number). The caller
# decides what a missing entry means; for both doors it is a refusal,
# because a 404 from the wrong port is the defect this file exists to
# remove, not a fallback.
sor_port_for_service() {
    local entry port=""
    for entry in $2; do
        [[ "$entry" == "$1="* ]] && port="${entry#*=}"
    done
    case "$port" in ''|*[!0-9]*) return 1 ;; esac
    printf '%s\n' "$port"
}

# sor_base_on_port BASE PORT — BASE with its port swapped and its host
# kept: scheme://host[:port][/…] -> scheme://host:PORT. The host is
# never the caller's to choose (a probe that names its own host can
# read a different stack and get a well-formed answer about different
# data); only the port moves.
sor_base_on_port() {
    local scheme="${1%%://*}" hostport="${1#*://}"
    hostport="${hostport%%/*}"
    printf '%s://%s:%s\n' "$scheme" "${hostport%%:*}" "$2"
}
