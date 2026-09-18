#!/usr/bin/env bash
#
# tenant-census — the brewery's footprint in the production database,
# READ-ONLY, as one JSON document. The conformance-report shape: fixed
# queries, JSON on stdout, exit 4 = cannot answer (naming the query),
# never a smaller set that looks whole.
#
# WHY IT EXISTS (backlog d07dcc2b, 2026-09-16)
# --------------------------------------------
# The cutover (ffc83387; David 2026-09-16 "Let's do it") evicts the
# brewery's reference data from prod and trims the simulated packets,
# and the eviction verb must REFUSE if any real packet references a row
# it would delete. Nothing could measure that: the footprint spans a
# dozen seed files and the projections 7,437 closed simulated packets
# left; the only readers are the APIs, which answer per-service and
# per-actor; the dev pod cannot exec into the cluster (RBAC); and a
# port-forward to the prod database is the trap the never-test-against-
# prod rule names. So the measurement is an ops verb on the forge,
# through the same kubectl + kubeconfig resolution the deploy runner and
# delete-orphan-object use (infra/cluster/undeclared-objects.sh
# --kubectl, resolved once), exec'ing psql inside the postgres
# container the manifest declares. The eviction verb (the next car)
# takes its bounds from THESE queries — one definition (CLAUDE.md §9a).
#
# WHAT IT ANSWERS — six sections, six reads
#   row_counts   (a) exact rows per table, every table in `public`
#   seed_owned   (b) rows the brewery seed owns per table, matched by
#                    the seed's OWN ids/codes read from the checkout's
#                    examples/brewery/{seeds,data}/* — never a typed
#                    list; `--seeds` prints exactly the sets used
#   partitions   (c) jobs and every table carrying a `partition` column,
#                    by partition; every table carrying a `job_id`
#                    column, by the partition of the job it names; every
#                    other table listed under `unknown` — never guessed
#   references   (d) REAL jobs/steps whose owner_id / subject_id /
#                    assignee_id / completed_by is a brewery seed row:
#                    counts AND the first 20 ids, so a refusal can name
#                    something
#   workflows    (e) the workflows table by owning_team, with open-job
#                    counts (all partitions, and real only)
#   log          (f) what marks a simulated EVENT (backlog e2604427,
#                    2026-09-16): audit_log rows by the payload's
#                    `_simulated` bool and by the newer `_partition`
#                    stamp, by kind prefix, and — for rows that name a
#                    job — the cross-tab of `_simulated` against the
#                    partition of THAT job; rows naming no job, by
#                    prefix; the same marker read from event_outbox
#                    and event_facts; and the measured `elapsed_ms`.
#                    Why it must be measured and not assumed: the bool
#                    resolved from the CLOCK MODE at publish time (each
#                    module's http.rs: "resolved by the publisher's
#                    clock"), while jobs.partition (508cc38c) is per
#                    packet — so under the sim clock a real packet's
#                    events may read `_simulated: true`. The answer
#                    decides whether the log-filtered rebuild that
#                    trims the simulated projections (design e652c7c6
#                    option 1) filters on the flag, on the job's
#                    partition, or repairs the flag first.
#
# READ-ONLY BY CONSTRUCTION. Every psql opens with
# `SET default_transaction_read_only = on` in its own -c, so the query
# that follows runs in a read-only transaction whatever it says; and
# every query here is a SELECT/WITH. crates/core/boss-testing/tests/
# tenant_census_sh.rs greps both and runs this file against a stub
# kubectl. Nothing here is ever run from the dev pod against the
# cluster: the operator files it (`boss ops forge tenant-census --wait`)
# and the answer lands on the ops-request packet.
#
# WHY sh + jq AND NOT python: directive 26d61c97 (observe-units.sh) —
# the forge's ops-runner is sh + jq. TOML has no jq, so each TOML seed's
# keys come out with awk (`[[table]]` header, then the first `key = "…"`
# line), and the test pins every set's count to an independent read of
# the same file.
#
# USAGE
#   tenant-census.sh            the census (needs kubectl or docker+kubeconfig)
#   tenant-census.sh --seeds    print the seed key sets as JSON and exit;
#                               no kubectl — the test seam, and what the
#                               eviction verb reads for its bounds
#
# EXIT
#   0  the document is on stdout, one line. The ops-runner captures
#      stdout AND stderr onto the packet (`> raw 2>&1`), so on the
#      ops-request the six progress lines precede it: the document is
#      the LAST line of `output` — `tail -1 | jq .`
#   4  cannot answer — stderr names the query and carries psql's words;
#      NOTHING on stdout
#   2  usage
#
# ENV
#   BOSS_CENSUS_TREE   the checkout whose seeds are the key sets (default:
#                      the repository this script lives in). The
#                      ops-runner passes no packet environment, so this
#                      is a test seam only.
#   BOSS_KUBECTL       see undeclared-objects.sh; resolved there, once.
#   KUBECONFIG         a credential that can exec in namespace boss.

set -uo pipefail

ME="tenant-census"
say() { echo "$ME: $*" >&2; }
CANNOT_ANSWER=4

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$SELF_DIR/../.." && pwd)"
TREE="${BOSS_CENSUS_TREE:-$REPO}"
SEEDS="$TREE/examples/brewery/seeds"
DATA="$TREE/examples/brewery/data"
RESOLVE="$REPO/infra/cluster/undeclared-objects.sh"

# The target, as infra/cluster/manifests/boss.yaml declares it: the
# StatefulSet `postgres` in namespace `boss`, container `postgres`,
# POSTGRES_USER=boss, POSTGRES_DB=boss. tenant_census_sh.rs holds the
# equality test between these five words and the manifest (§9a).
. "$(dirname "$0")/forge-defaults.sh"
CENSUS_NS="boss"
CENSUS_WORKLOAD="$PG_WORKLOAD"
CENSUS_CONTAINER="$PG_CONTAINER"
CENSUS_DB_USER="$PG_USER"
CENSUS_DB_NAME="boss"

MODE="${1:-}"
case "$MODE" in
    ""|--seeds) ;;
    *) say "usage: $ME [--seeds]"; exit 2 ;;
esac

command -v jq >/dev/null 2>&1 || { say "CANNOT ANSWER — jq is not on PATH"; exit "$CANNOT_ANSWER"; }
[ -d "$SEEDS" ] || { say "CANNOT ANSWER — no seed directory at $SEEDS"; exit "$CANNOT_ANSWER"; }

# --- the seed key sets, read from the checkout ------------------------------
#
# One extraction per seed file, each the file's OWN key. Where a table's
# key is not the obvious `id`, the column is named beside the set (and
# the schema file it was read from), because the SQL below matches on
# exactly that column.

# `[[header]]` then the first `key = "…"` — one value per table entry.
# Arrays-of-tables in these seeds put the key on the line right after
# the header, but "first key line after the header" is what is read, so
# a comment or blank line between them costs nothing.
toml_field() { # <file> <header> <key>
    awk -v h="[[$2]]" -v k="$3 = " '
        $0 == h { want = 1; next }
        want && index($0, k) == 1 {
            v = substr($0, length(k) + 1)
            sub(/^"/, "", v); sub(/"[ \t]*(#.*)?$/, "", v)
            print v; want = 0
        }' "$1"
}
# Every `"…"` inside a `key = [ … ]` array, single- or multi-line.
toml_array_strings() { # <file> <key>
    awk -v k="$2 = [" '
        index($0, k) == 1 { inside = 1 }
        inside {
            line = $0
            while (match(line, /"[^"]*"/)) {
                print substr(line, RSTART + 1, RLENGTH - 2)
                line = substr(line, RSTART + RLENGTH)
            }
            if (index($0, "]") > 0) inside = 0
        }' "$1"
}
lines_to_json() { jq -R . | jq -s .; }

seed_sets() {
    local tenant_id
    # [meta] is a table, not an array-of-tables, so not toml_field's shape.
    tenant_id=$(awk '$0 == "[meta]" {m=1; next} m && index($0, "tenant_id = ") == 1 {v=$0; sub(/^tenant_id = "/, "", v); sub(/".*$/, "", v); print v; exit}' "$SEEDS/tenant.toml")
    jq -n \
        --arg tenant_id "$tenant_id" \
        --argjson employees "$( { jq -r '.[].id' "$SEEDS/employees.json"; toml_field "$SEEDS/operator_hires.toml" hire id; } | lines_to_json)" \
        --argjson classes "$(jq -c 'map({subject_kind, code})' "$SEEDS/classes.json")" \
        --argjson accounts "$(toml_array_strings "$SEEDS/accounts.toml" names | lines_to_json)" \
        --argjson vendors "$(toml_field "$SEEDS/vendors.toml" vendor name | lines_to_json)" \
        --argjson locations "$(toml_field "$SEEDS/locations.toml" location id | lines_to_json)" \
        --argjson products "$(toml_field "$SEEDS/products.toml" products sku | lines_to_json)" \
        --argjson parts "$(toml_field "$SEEDS/parts.toml" parts part_sku | lines_to_json)" \
        --argjson bulletins "$(toml_field "$SEEDS/bulletins.toml" bulletin title | lines_to_json)" \
        --argjson messages "$(toml_field "$SEEDS/messages.toml" thread subject | lines_to_json)" \
        --argjson marketing_assets "$(jq -c 'map(.id)' "$DATA/marketing-assets.json")" \
        --argjson assets "$(jq -c 'map(.asset_id)' "$DATA/assets.json")" \
        --argjson asset_models "$(jq -c 'map(.sku)' "$DATA/catalog.json")" \
        --argjson policy_roles "$(toml_array_strings "$SEEDS/policy_rules.toml" roles | sort -u | lines_to_json)" \
        --argjson business_calendars "$(jq -c 'map(.code)' "$SEEDS/business_calendars.json")" \
        --argjson excise_jurisdictions "$(toml_field "$SEEDS/excise_rates.toml" schedule jurisdiction | lines_to_json)" \
        --argjson subject_kinds "$(toml_field "$SEEDS/subject_kinds.toml" subject_kind kind | lines_to_json)" \
        --argjson workflows "$(toml_field "$SEEDS/workflows.toml" workflow kind | lines_to_json)" \
        '{
            tenant_id: $tenant_id,
            employees: $employees, classes: $classes, accounts: $accounts,
            vendors: $vendors, locations: $locations, products: $products,
            parts: $parts, bulletins: $bulletins, messages: $messages,
            marketing_assets: $marketing_assets, assets: $assets,
            asset_models: $asset_models, policy_roles: $policy_roles,
            business_calendars: $business_calendars,
            excise_jurisdictions: $excise_jurisdictions,
            subject_kinds: $subject_kinds, workflows: $workflows,
            not_keyed: ["subject_edges", "companies", "sales_tax_rate_by_state", "tax_kinds"]
        }'
}

SEED_JSON=$(seed_sets) || { say "CANNOT ANSWER — could not read the seed key sets from $SEEDS"; exit "$CANNOT_ANSWER"; }
# Every set the SQL joins on must be non-empty, or a "0 rows owned" is a
# broken extraction reading as an answer.
empty=$(printf '%s' "$SEED_JSON" | jq -r 'to_entries[] | select(.value | type == "array" and length == 0) | .key')
if [ -n "$empty" ]; then
    say "CANNOT ANSWER — a seed key set came out empty, which is an extraction defect, not a footprint: $(printf '%s' "$empty" | tr '\n' ' ')"
    exit "$CANNOT_ANSWER"
fi
if [ "$MODE" = "--seeds" ]; then
    printf '%s\n' "$SEED_JSON"
    exit 0
fi
# The sets ride INTO the SQL as a dollar-quoted literal — one round trip
# per section, no temp table, nothing written. A name containing the
# quote tag would end the literal early, so refuse rather than guess.
case "$SEED_JSON" in
    *'$seed$'*) say "CANNOT ANSWER — a seed value contains the literal tag \$seed\$"; exit "$CANNOT_ANSWER" ;;
esac
TENANT_ID=$(printf '%s' "$SEED_JSON" | jq -r .tenant_id)

# --- the SQL ------------------------------------------------------------------
#
# Each block is one statement, tagged on its first line so the failed
# one is nameable and the stub can answer by name. Every block returns
# ONE json value on one line (`-At`), which jq validates.
#
# Seed-set columns, each read from infra/postgres/schema/*.sql:
#   employees.id · employee_skills/employee_certifications/employee_changes.employee_id (10-people.sql)
#   classes (subject_kind, code) (01-registries.sql)
#   accounts.name — the prepare step mints ids `acc-bigseed-NNNN` and
#     names them from accounts.toml `names` in order, and a prospect is
#     the same name + " (Prospect)" (tenant_data.rs ensure_prospect_account),
#     so NAME is the seed's own key; account_contacts/notes/team_members
#     by account_id (22-accounts.sql). `acc-direct-shop` is minted with a
#     name that lives only in Rust, so it shows in the UNMATCHED count.
#   vendors.name — same shape, `vnd-bigseed-NNN` named from vendors.toml;
#     vendor_contacts/contracts/account_team by vendor_id (24-inventory.sql)
#   locations.id · products.sku (25-products.sql) · parts.part_sku
#     (20-catalog.sql) · bulletins.title (07-content.sql) ·
#     messages.subject (26-messages.sql) · marketing_assets.id ·
#     assets.asset_id (21-assets.sql) · asset_models.sku ·
#     policy_rules.role (04-policy.sql) · business_calendars.code and
#     business_calendar_closed_days.calendar_code ·
#     excise_rate_schedules.jurisdiction (153-…) · subject_kinds.kind ·
#     workflows.kind (03-jobs.sql) · subjects.id, any kind, across the
#     union of every id set above (01-registries.sql)

sql_seed_ctes() { # the CTEs every seed-keyed query starts from
    cat <<SQL
WITH seed AS (SELECT \$seed\$${SEED_JSON}\$seed\$::jsonb AS s),
emp AS (SELECT jsonb_array_elements_text(s->'employees') AS id FROM seed),
cls AS (SELECT e->>'subject_kind' AS subject_kind, e->>'code' AS code FROM seed, jsonb_array_elements(s->'classes') e),
acc_names AS (SELECT jsonb_array_elements_text(s->'accounts') AS name FROM seed),
acc AS (SELECT id FROM accounts WHERE name IN (SELECT name FROM acc_names) OR name IN (SELECT name || ' (Prospect)' FROM acc_names)),
ven_names AS (SELECT jsonb_array_elements_text(s->'vendors') AS name FROM seed),
ven AS (SELECT id FROM vendors WHERE name IN (SELECT name FROM ven_names)),
loc AS (SELECT jsonb_array_elements_text(s->'locations') AS id FROM seed),
prod AS (SELECT jsonb_array_elements_text(s->'products') AS sku FROM seed),
part AS (SELECT jsonb_array_elements_text(s->'parts') AS part_sku FROM seed),
bul AS (SELECT jsonb_array_elements_text(s->'bulletins') AS title FROM seed),
msg AS (SELECT jsonb_array_elements_text(s->'messages') AS subject FROM seed),
mkt AS (SELECT jsonb_array_elements_text(s->'marketing_assets') AS id FROM seed),
ast AS (SELECT jsonb_array_elements_text(s->'assets') AS asset_id FROM seed),
mdl AS (SELECT jsonb_array_elements_text(s->'asset_models') AS sku FROM seed),
roles AS (SELECT jsonb_array_elements_text(s->'policy_roles') AS role FROM seed),
cal AS (SELECT jsonb_array_elements_text(s->'business_calendars') AS code FROM seed),
exc AS (SELECT jsonb_array_elements_text(s->'excise_jurisdictions') AS jurisdiction FROM seed),
skind AS (SELECT jsonb_array_elements_text(s->'subject_kinds') AS kind FROM seed),
wf AS (SELECT jsonb_array_elements_text(s->'workflows') AS kind FROM seed),
subject_ids AS (
    SELECT id FROM emp UNION SELECT id FROM acc UNION SELECT id FROM ven UNION SELECT id FROM loc
    UNION SELECT sku FROM prod UNION SELECT part_sku FROM part UNION SELECT id FROM mkt
    UNION SELECT asset_id FROM ast UNION SELECT sku FROM mdl
),
real_jobs AS (SELECT id, owner_id, subject_kind, subject_id FROM jobs WHERE partition = 'real')
SQL
}

# (a) Exact counts for every table in `public`, one statement:
# query_to_xml runs `SELECT count(*)` per table inside this read-only
# transaction. pg_class.reltuples is an estimate and the eviction's
# refusal will be compared against these numbers, so estimates are out.
sql_row_counts() {
    cat <<'SQL'
-- census:row_counts
SELECT coalesce(json_object_agg(t, c ORDER BY t), '{}'::json)
FROM (
    SELECT table_name AS t,
           (xpath('/row/c/text()', query_to_xml(
               format('SELECT count(*) AS c FROM %I.%I', table_schema, table_name),
               false, true, '')))[1]::text::bigint AS c
    FROM information_schema.tables
    WHERE table_schema = 'public' AND table_type = 'BASE TABLE'
) s
SQL
}

# (b) Per table: the key column, how many keys the seed carries, how
# many rows match, the first 20 matching keys, and `unmatched` — rows
# in the table the seed does NOT own, which is what survives an
# eviction bounded to these sets.
sql_seed_owned() {
    echo "-- census:seed_owned"
    sql_seed_ctes
    cat <<'SQL'
SELECT json_build_object(
    'employees', (SELECT json_build_object('key', 'id', 'seeded', (SELECT count(*) FROM emp), 'present', count(*), 'unmatched', (SELECT count(*) FROM employees) - count(*), 'sample', coalesce((array_agg(id ORDER BY id))[1:20], '{}')) FROM employees WHERE id IN (SELECT id FROM emp)),
    'employee_skills', (SELECT json_build_object('key', 'employee_id', 'present', count(*), 'unmatched', (SELECT count(*) FROM employee_skills) - count(*)) FROM employee_skills WHERE employee_id IN (SELECT id FROM emp)),
    'employee_certifications', (SELECT json_build_object('key', 'employee_id', 'present', count(*), 'unmatched', (SELECT count(*) FROM employee_certifications) - count(*)) FROM employee_certifications WHERE employee_id IN (SELECT id FROM emp)),
    'employee_changes', (SELECT json_build_object('key', 'employee_id', 'present', count(*), 'unmatched', (SELECT count(*) FROM employee_changes) - count(*)) FROM employee_changes WHERE employee_id IN (SELECT id FROM emp)),
    'classes', (SELECT json_build_object('key', '(subject_kind, code)', 'seeded', (SELECT count(*) FROM cls), 'present', count(*), 'unmatched', (SELECT count(*) FROM classes) - count(*), 'sample', coalesce((array_agg(c.subject_kind || ':' || c.code ORDER BY c.subject_kind, c.code))[1:20], '{}')) FROM classes c WHERE (c.subject_kind, c.code) IN (SELECT subject_kind, code FROM cls)),
    'accounts', (SELECT json_build_object('key', 'name', 'seeded', (SELECT count(*) FROM acc_names), 'present', count(*), 'unmatched', (SELECT count(*) FROM accounts) - count(*), 'sample', coalesce((array_agg(id ORDER BY id))[1:20], '{}')) FROM accounts WHERE id IN (SELECT id FROM acc)),
    'account_contacts', (SELECT json_build_object('key', 'account_id', 'present', count(*), 'unmatched', (SELECT count(*) FROM account_contacts) - count(*)) FROM account_contacts WHERE account_id IN (SELECT id FROM acc)),
    'account_notes', (SELECT json_build_object('key', 'account_id', 'present', count(*), 'unmatched', (SELECT count(*) FROM account_notes) - count(*)) FROM account_notes WHERE account_id IN (SELECT id FROM acc)),
    'account_team_members', (SELECT json_build_object('key', 'account_id', 'present', count(*), 'unmatched', (SELECT count(*) FROM account_team_members) - count(*)) FROM account_team_members WHERE account_id IN (SELECT id FROM acc)),
    'vendors', (SELECT json_build_object('key', 'name', 'seeded', (SELECT count(*) FROM ven_names), 'present', count(*), 'unmatched', (SELECT count(*) FROM vendors) - count(*), 'sample', coalesce((array_agg(id ORDER BY id))[1:20], '{}')) FROM vendors WHERE id IN (SELECT id FROM ven)),
    'vendor_contacts', (SELECT json_build_object('key', 'vendor_id', 'present', count(*), 'unmatched', (SELECT count(*) FROM vendor_contacts) - count(*)) FROM vendor_contacts WHERE vendor_id IN (SELECT id FROM ven)),
    'vendor_contracts', (SELECT json_build_object('key', 'vendor_id', 'present', count(*), 'unmatched', (SELECT count(*) FROM vendor_contracts) - count(*)) FROM vendor_contracts WHERE vendor_id IN (SELECT id FROM ven)),
    'vendor_account_team', (SELECT json_build_object('key', 'vendor_id', 'present', count(*), 'unmatched', (SELECT count(*) FROM vendor_account_team) - count(*)) FROM vendor_account_team WHERE vendor_id IN (SELECT id FROM ven)),
    'locations', (SELECT json_build_object('key', 'id', 'seeded', (SELECT count(*) FROM loc), 'present', count(*), 'unmatched', (SELECT count(*) FROM locations) - count(*), 'sample', coalesce((array_agg(id ORDER BY id))[1:20], '{}')) FROM locations WHERE id IN (SELECT id FROM loc)),
    'products', (SELECT json_build_object('key', 'sku', 'seeded', (SELECT count(*) FROM prod), 'present', count(*), 'unmatched', (SELECT count(*) FROM products) - count(*), 'sample', coalesce((array_agg(sku ORDER BY sku))[1:20], '{}')) FROM products WHERE sku IN (SELECT sku FROM prod)),
    'parts', (SELECT json_build_object('key', 'part_sku', 'seeded', (SELECT count(*) FROM part), 'present', count(*), 'unmatched', (SELECT count(*) FROM parts) - count(*), 'sample', coalesce((array_agg(part_sku ORDER BY part_sku))[1:20], '{}')) FROM parts WHERE part_sku IN (SELECT part_sku FROM part)),
    'bulletins', (SELECT json_build_object('key', 'title', 'seeded', (SELECT count(*) FROM bul), 'present', count(*), 'unmatched', (SELECT count(*) FROM bulletins) - count(*), 'sample', coalesce((array_agg(id::text ORDER BY id))[1:20], '{}')) FROM bulletins WHERE title IN (SELECT title FROM bul)),
    'messages', (SELECT json_build_object('key', 'subject', 'seeded', (SELECT count(*) FROM msg), 'present', count(*), 'unmatched', (SELECT count(*) FROM messages) - count(*), 'sample', coalesce((array_agg(id ORDER BY id))[1:20], '{}')) FROM messages WHERE subject IN (SELECT subject FROM msg)),
    'marketing_assets', (SELECT json_build_object('key', 'id', 'seeded', (SELECT count(*) FROM mkt), 'present', count(*), 'unmatched', (SELECT count(*) FROM marketing_assets) - count(*), 'sample', coalesce((array_agg(id ORDER BY id))[1:20], '{}')) FROM marketing_assets WHERE id IN (SELECT id FROM mkt)),
    'assets', (SELECT json_build_object('key', 'asset_id', 'seeded', (SELECT count(*) FROM ast), 'present', count(*), 'unmatched', (SELECT count(*) FROM assets) - count(*), 'sample', coalesce((array_agg(asset_id ORDER BY asset_id))[1:20], '{}')) FROM assets WHERE asset_id IN (SELECT asset_id FROM ast)),
    'asset_models', (SELECT json_build_object('key', 'sku', 'seeded', (SELECT count(*) FROM mdl), 'present', count(*), 'unmatched', (SELECT count(*) FROM asset_models) - count(*), 'sample', coalesce((array_agg(sku ORDER BY sku))[1:20], '{}')) FROM asset_models WHERE sku IN (SELECT sku FROM mdl)),
    'policy_rules', (SELECT json_build_object('key', 'role', 'seeded', (SELECT count(*) FROM roles), 'present', count(*), 'unmatched', (SELECT count(*) FROM policy_rules) - count(*), 'sample', coalesce((array_agg(id ORDER BY id))[1:20], '{}')) FROM policy_rules WHERE role IN (SELECT role FROM roles)),
    'business_calendars', (SELECT json_build_object('key', 'code', 'seeded', (SELECT count(*) FROM cal), 'present', count(*), 'unmatched', (SELECT count(*) FROM business_calendars) - count(*), 'sample', coalesce((array_agg(code ORDER BY code))[1:20], '{}')) FROM business_calendars WHERE code IN (SELECT code FROM cal)),
    'business_calendar_closed_days', (SELECT json_build_object('key', 'calendar_code', 'present', count(*), 'unmatched', (SELECT count(*) FROM business_calendar_closed_days) - count(*)) FROM business_calendar_closed_days WHERE calendar_code IN (SELECT code FROM cal)),
    'excise_rate_schedules', (SELECT json_build_object('key', 'jurisdiction', 'seeded', (SELECT count(*) FROM exc), 'present', count(*), 'unmatched', (SELECT count(*) FROM excise_rate_schedules) - count(*), 'sample', coalesce((array_agg(jurisdiction || '@' || effective_from::text ORDER BY jurisdiction, effective_from))[1:20], '{}')) FROM excise_rate_schedules WHERE jurisdiction IN (SELECT jurisdiction FROM exc)),
    'subject_kinds', (SELECT json_build_object('key', 'kind', 'seeded', (SELECT count(*) FROM skind), 'present', count(*), 'unmatched', (SELECT count(*) FROM subject_kinds) - count(*), 'sample', coalesce((array_agg(kind ORDER BY kind))[1:20], '{}')) FROM subject_kinds WHERE kind IN (SELECT kind FROM skind)),
    'workflows', (SELECT json_build_object('key', 'kind', 'seeded', (SELECT count(*) FROM wf), 'present', count(*), 'unmatched', (SELECT count(*) FROM workflows) - count(*), 'sample', coalesce((array_agg(kind || '@v' || version::text ORDER BY kind, version))[1:20], '{}')) FROM workflows WHERE kind IN (SELECT kind FROM wf)),
    'subjects', (SELECT json_build_object('key', 'id (any kind, across every seed id set)', 'present', count(*), 'unmatched', (SELECT count(*) FROM subjects) - count(*), 'sample', coalesce((array_agg(kind || ':' || id ORDER BY kind, id))[1:20], '{}')) FROM subjects WHERE id IN (SELECT id FROM subject_ids)),
    'not_keyed', (SELECT s->'not_keyed' FROM seed)
)
SQL
}

# (c) Partition is a fact only where a column carries it. Three lists,
# derived from the catalog at run time, never typed: tables with their
# own `partition` column, grouped by it; tables with a `job_id` column,
# grouped by the partition of the job they name (`unlinked` = no such
# job, or NULL); and every other table under `unknown`.
sql_partitions() {
    cat <<'SQL'
-- census:partitions
WITH cols AS (
    SELECT table_name, column_name FROM information_schema.columns WHERE table_schema = 'public'
),
tables AS (
    SELECT table_name FROM information_schema.tables WHERE table_schema = 'public' AND table_type = 'BASE TABLE'
),
by_partition AS (
    SELECT c.table_name AS t,
           (SELECT coalesce(json_object_agg(
                       (xpath('/row/p/text()', r))[1]::text,
                       (xpath('/row/n/text()', r))[1]::text::bigint), '{}'::json)
            FROM unnest(xpath('/table/row', query_to_xml(
                format('SELECT partition AS p, count(*) AS n FROM public.%I GROUP BY 1', c.table_name),
                false, false, ''))) r) AS counts
    FROM cols c WHERE c.column_name = 'partition'
),
by_job AS (
    SELECT c.table_name AS t,
           (SELECT coalesce(json_object_agg(
                       (xpath('/row/p/text()', r))[1]::text,
                       (xpath('/row/n/text()', r))[1]::text::bigint), '{}'::json)
            FROM unnest(xpath('/table/row', query_to_xml(
                format('SELECT coalesce(j.partition, ''unlinked'') AS p, count(*) AS n FROM public.%I t LEFT JOIN public.jobs j ON j.id::text = t.job_id::text GROUP BY 1', c.table_name),
                false, false, ''))) r) AS counts
    FROM cols c WHERE c.column_name = 'job_id'
)
SELECT json_build_object(
    'by_partition_column', (SELECT coalesce(json_object_agg(t, counts ORDER BY t), '{}'::json) FROM by_partition),
    'by_job_id', (SELECT coalesce(json_object_agg(t, counts ORDER BY t), '{}'::json) FROM by_job),
    'unknown', (SELECT coalesce(json_agg(table_name ORDER BY table_name), '[]'::json) FROM tables
                WHERE table_name NOT IN (SELECT t FROM by_partition) AND table_name NOT IN (SELECT t FROM by_job))
)
SQL
}

# (d) The eviction's refusal set: REAL packets that point at a seed row.
# Counts and the first 20 ids, so a refusal names something.
sql_references() {
    echo "-- census:references"
    sql_seed_ctes
    cat <<'SQL'
SELECT json_build_object(
    'jobs.owner_id', (SELECT json_build_object('count', count(*), 'ids', coalesce((array_agg(id::text ORDER BY id))[1:20], '{}')) FROM real_jobs WHERE owner_id IN (SELECT id FROM emp)),
    'jobs.subject_id', (SELECT json_build_object('count', count(*), 'ids', coalesce((array_agg(id::text ORDER BY id))[1:20], '{}'), 'by_subject_kind', coalesce((SELECT json_object_agg(k, n) FROM (SELECT subject_kind k, count(*) n FROM real_jobs WHERE subject_id IN (SELECT id FROM subject_ids) GROUP BY 1) x), '{}'::json)) FROM real_jobs WHERE subject_id IN (SELECT id FROM subject_ids)),
    'steps.assignee_id', (SELECT json_build_object('count', count(*), 'ids', coalesce((array_agg(s.id::text ORDER BY s.id))[1:20], '{}')) FROM steps s JOIN real_jobs j ON j.id = s.job_id WHERE s.assignee_id IN (SELECT id FROM emp)),
    'steps.completed_by', (SELECT json_build_object('count', count(*), 'ids', coalesce((array_agg(s.id::text ORDER BY s.id))[1:20], '{}')) FROM steps s JOIN real_jobs j ON j.id = s.job_id WHERE s.completed_by IN (SELECT id FROM emp))
)
SQL
}

# (e) Who owns which workflows, and what is still open under them.
sql_workflows() {
    cat <<'SQL'
-- census:workflows
SELECT coalesce(json_agg(row_to_json(x) ORDER BY x.owning_team), '[]'::json)
FROM (
    SELECT w.owning_team,
           count(DISTINCT w.kind) AS kinds,
           count(*) AS versions,
           count(*) FILTER (WHERE w.status = 'active') AS active_versions,
           (SELECT count(*) FROM jobs j WHERE j.status NOT IN ('closed', 'cancelled')
              AND j.kind IN (SELECT kind FROM workflows w2 WHERE w2.owning_team = w.owning_team)) AS open_jobs,
           (SELECT count(*) FROM jobs j WHERE j.status NOT IN ('closed', 'cancelled') AND j.partition = 'real'
              AND j.kind IN (SELECT kind FROM workflows w2 WHERE w2.owning_team = w.owning_team)) AS open_real_jobs
    FROM workflows w GROUP BY w.owning_team
) x
SQL
}

# (f) What marks a simulated EVENT (backlog e2604427). ONE pass over
# audit_log (1.4M rows on 2026-09-16): every row is reduced to its cell
# — kind prefix, the `_simulated` bool, the `_partition` stamp, and the
# partition of the job it names — and counted; every cross-tab below
# is then an aggregate over those few hundred cells, never a second
# scan. The job reference is what the packet named: payload.job_id, or
# payload.id when the kind is jobs.job.* (a job's own events carry
# themselves as `id`, boss-jobs rebuild.rs). `_partition` is read
# beside `_simulated` because the rebuilder prefers it when present
# (boss-core partition.rs from_event_payload) — so the cross-tab shows
# both facts the replay would use. event_outbox and event_facts carry
# the same envelope: one grouped pass each, and the note beside each
# says how it relates to the log, so a count that differs is read as a
# lag, not a second opinion. Nothing here is a per-row subquery.
#
# The marker expression lives once, expanded into the (unquoted)
# heredoc: true / false / absent, and `other` for a value that is
# present but not a boolean (a JSON null, a string) — which the
# rebuilder reads as REAL, so it must not hide under `absent`.
LOG_MARKER_SQL="CASE WHEN NOT (payload ? '_simulated') THEN 'absent' WHEN jsonb_typeof(payload->'_simulated') = 'boolean' THEN payload->>'_simulated' ELSE 'other' END"
sql_log() {
    cat <<SQL
-- census:log
WITH cells AS (
    SELECT split_part(a.kind, '.', 1) AS prefix,
           (${LOG_MARKER_SQL}) AS sim,
           coalesce(a.payload->>'_partition', 'absent') AS pstamp,
           CASE WHEN ref.job_ref IS NULL THEN 'none' ELSE coalesce(j.partition, 'missing') END AS jpart,
           count(*) AS n
    FROM audit_log a
    CROSS JOIN LATERAL (SELECT coalesce(a.payload->>'job_id', CASE WHEN a.kind LIKE 'jobs.job.%' THEN a.payload->>'id' END) AS job_ref) ref
    LEFT JOIN jobs j ON j.id::text = ref.job_ref
    GROUP BY 1, 2, 3, 4
),
ob AS (
    SELECT (${LOG_MARKER_SQL}) AS sim, (delivered_at IS NULL) AS pending, count(*) AS n
    FROM event_outbox GROUP BY 1, 2
),
ef AS (
    SELECT (${LOG_MARKER_SQL}) AS sim, count(*) AS n
    FROM event_facts GROUP BY 1
)
SELECT json_build_object(
    'audit_log', json_build_object(
        'rows', (SELECT coalesce(sum(n), 0)::bigint FROM cells),
        'by_simulated', (SELECT coalesce(json_object_agg(sim, n ORDER BY sim), '{}'::json) FROM (SELECT sim, sum(n)::bigint AS n FROM cells GROUP BY 1) x),
        'by_partition_stamp', (SELECT coalesce(json_object_agg(pstamp, n ORDER BY pstamp), '{}'::json) FROM (SELECT pstamp, sum(n)::bigint AS n FROM cells GROUP BY 1) x),
        'by_simulated_and_partition_stamp', (SELECT coalesce(json_object_agg(sim, o ORDER BY sim), '{}'::json) FROM (
            SELECT sim, json_object_agg(pstamp, n ORDER BY pstamp) AS o FROM (SELECT sim, pstamp, sum(n)::bigint AS n FROM cells GROUP BY 1, 2) y GROUP BY sim) x),
        'by_prefix', (SELECT coalesce(json_object_agg(prefix, o ORDER BY prefix), '{}'::json) FROM (
            SELECT prefix, json_object_agg(sim, n ORDER BY sim) AS o FROM (SELECT prefix, sim, sum(n)::bigint AS n FROM cells GROUP BY 1, 2) y GROUP BY prefix) x),
        'job_linked', json_build_object(
            'rows', (SELECT coalesce(sum(n), 0)::bigint FROM cells WHERE jpart <> 'none'),
            'cross_tab', (SELECT coalesce(json_object_agg(sim, o ORDER BY sim), '{}'::json) FROM (
                SELECT sim, json_object_agg(jpart, n ORDER BY jpart) AS o FROM (SELECT sim, jpart, sum(n)::bigint AS n FROM cells WHERE jpart <> 'none' GROUP BY 1, 2) y GROUP BY sim) x),
            'missing_job', (SELECT coalesce(sum(n), 0)::bigint FROM cells WHERE jpart = 'missing')
        ),
        'unlinked', json_build_object(
            'rows', (SELECT coalesce(sum(n), 0)::bigint FROM cells WHERE jpart = 'none'),
            'by_prefix', (SELECT coalesce(json_object_agg(prefix, o ORDER BY prefix), '{}'::json) FROM (
                SELECT prefix, json_object_agg(sim, n ORDER BY sim) AS o FROM (SELECT prefix, sim, sum(n)::bigint AS n FROM cells WHERE jpart = 'none' GROUP BY 1, 2) y GROUP BY prefix) x)
        )
    ),
    'event_outbox', json_build_object(
        'rows', (SELECT coalesce(sum(n), 0)::bigint FROM ob),
        'pending', (SELECT coalesce(sum(n), 0)::bigint FROM ob WHERE pending),
        'by_simulated', (SELECT coalesce(json_object_agg(sim, n ORDER BY sim), '{}'::json) FROM (SELECT sim, sum(n)::bigint AS n FROM ob GROUP BY 1) x),
        'note', 'the same envelope as audit_log: the relay copies each row into audit_log and stamps delivered_at, and nothing prunes delivered rows (02-events.sql), so rows here are the log since the outbox landed, pending is the relay lag, and a marker split that differs from audit_log over the same era is a relay gap'
    ),
    'event_facts', json_build_object(
        'rows', (SELECT coalesce(sum(n), 0)::bigint FROM ef),
        'by_simulated', (SELECT coalesce(json_object_agg(sim, n ORDER BY sim), '{}'::json) FROM (SELECT sim, sum(n)::bigint AS n FROM ef GROUP BY 1) x),
        'note', 'a projection of audit_log with payload kept whole (43-event-facts.sql): fewer rows than audit_log is projection lag, and the marker split must match audit_log for the rows it holds'
    )
)
SQL
}

# --- the kubectl, resolved once, by the derivation the delete verb uses --
KUBECTL_LINE=$("$RESOLVE" --kubectl) || {
    say "CANNOT ANSWER — no kubectl to read with (see above). Nothing measured."
    exit "$CANNOT_ANSWER"
}
read -r -a KUBECTL <<<"$KUBECTL_LINE"

TMP=$(mktemp -d) || exit "$CANNOT_ANSWER"
trap 'rm -rf "$TMP"' EXIT

# One read: exec psql in the postgres container with the session set
# read-only in its own -c BEFORE the query's -c. `-At` prints the single
# json value bare, on one line; jq -c proves it parsed. On any failure
# the answer is exit 4 naming the section, with psql's own words — and
# no document, because a census with a section missing is not a census.
# Wall-clock milliseconds, for the `log` section's measured cost (three
# full scans of the largest tables; backlog e2604427 asked for the
# number, not a guess). GNU date — the forge and the gate are Linux;
# anything else reads 0 rather than failing the census.
now_ms() {
    local ns
    ns=$(date +%s%N)
    case "$ns" in *[!0-9]*) echo 0 ;; *) echo $((ns / 1000000)) ;; esac
}
read_section() { # <name> <sql>
    local name="$1" sql="$2" t0
    say "reading $name"
    t0=$(now_ms)
    if ! "${KUBECTL[@]}" -n "$CENSUS_NS" exec "$CENSUS_WORKLOAD" -c "$CENSUS_CONTAINER" -- \
            psql -X -q -At -v ON_ERROR_STOP=1 -U "$CENSUS_DB_USER" -d "$CENSUS_DB_NAME" \
            -c 'SET default_transaction_read_only = on' \
            -c "$sql" > "$TMP/$name.out" 2> "$TMP/$name.err"; then
        say "CANNOT ANSWER — the $name query failed; psql said:"
        sed 's/^/    /' "$TMP/$name.err" >&2
        exit "$CANNOT_ANSWER"
    fi
    if ! jq -c . < "$TMP/$name.out" > "$TMP/$name.json" 2> "$TMP/$name.jq"; then
        say "CANNOT ANSWER — the $name query answered, but not with one JSON value:"
        sed 's/^/    /' "$TMP/$name.jq" >&2
        head -c 400 "$TMP/$name.out" | sed 's/^/    /' >&2
        exit "$CANNOT_ANSWER"
    fi
    echo $(( $(now_ms) - t0 )) > "$TMP/$name.ms"
}

read_section row_counts "$(sql_row_counts)"
read_section seed_owned "$(sql_seed_owned)"
read_section partitions "$(sql_partitions)"
read_section references "$(sql_references)"
read_section workflows "$(sql_workflows)"
read_section log "$(sql_log)"

# All six answered: assemble, and only now print. Only `log` carries
# its elapsed_ms — the five earlier sections keep the shape the
# eviction verb (built beside this car) reads.
jq -n -c \
    --arg verb "$ME" \
    --arg tenant_id "$TENANT_ID" \
    --arg measured_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg seeds "$SEEDS" \
    --argjson seed_keys "$SEED_JSON" \
    --slurpfile row_counts "$TMP/row_counts.json" \
    --slurpfile seed_owned "$TMP/seed_owned.json" \
    --slurpfile partitions "$TMP/partitions.json" \
    --slurpfile references "$TMP/references.json" \
    --slurpfile workflows "$TMP/workflows.json" \
    --slurpfile log "$TMP/log.json" \
    --arg log_ms "$(cat "$TMP/log.ms")" \
    '{
        verb: $verb, tenant_id: $tenant_id, measured_at: $measured_at, seeds: $seeds,
        row_counts: $row_counts[0], seed_owned: $seed_owned[0], partitions: $partitions[0],
        references: $references[0], workflows: $workflows[0],
        log: ($log[0] + {elapsed_ms: ($log_ms | tonumber)}),
        seed_keys: $seed_keys
    }'
