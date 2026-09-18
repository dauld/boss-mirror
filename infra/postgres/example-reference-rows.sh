#!/usr/bin/env bash
#
# example-reference-rows — the ONE derivation of "which reference rows
# are the example tenants', and which of those an instance may drop":
# the candidate set read from the example tenants' own seeds, and the
# SQL that judges and evicts it (backlog 718ac982; design e2580840
# car 3, folding 83a873e8).
#
# WHY IT EXISTS
# -------------
# 01-registries.sql and 40-ledger.sql seed reference rows for the two
# worked-example tenants on EVERY instance — the used-device shop's
# 26 roles and ten departments, the brewery's location kinds, account
# types, equipment categories, two production sites, the brewery-
# shaped starter chart of 33 accounts, its five tax kinds and the 27
# states it collects sales tax in (40-ledger.sql; backlog 7f163e58),
# a companies row for each. A
# real company's instance booted with a brewer's books and a refurb
# shop's org chart (measured on prod, 2026-09-17: "Brewery Taproom"
# in /api/locations). The migrations cannot be edited — applied files
# are history, migrate.sh refuses a changed checksum — so the rows
# leave by three doors that all read THIS derivation:
#
#   1. the example tenants' seeds now carry every one of those rows
#      (examples/*/seeds/classes.*, locations.toml,
#      chart_of_accounts.toml, tax.toml), so the playground publishes them
#      through its contract and the migration rows become residue;
#   2. a FRESH instance whose tenant is not an example evicts them at
#      boot, before any service starts (init.sh, first start only —
#      `boot` below decides, `delete-sql` acts);
#   3. an instance already running evicts the unreferenced residue
#      through the bounded forge verb retire-example-reference-rows
#      (`plan-sql` for the dry run, `delete-sql` for the real one),
#      with the record on the ops-request packet.
#
# THE CANDIDATE SET IS READ, NEVER TYPED. examples/*/seeds/classes.json
# and classes.toml give (subject_kind, code, member_attribute);
# locations.toml gives location ids; chart_of_accounts.toml gives
# account codes; tax.toml gives tax kinds and sales-tax states; each
# tenant.toml's [meta] tenant_id is its companies
# row. Whatever the platform needs must therefore NOT be in an
# example's seeds — the `platform-admin` / `audit-readonly` / `owner`
# / `smoke-tester` roles, the `it` department, employment types and
# statuses, the `unspecified` account type, the `remote` / `hq` /
# `field-region` kinds the platform's three default locations wear,
# those three locations (loc-hq is where the operator baseline
# hires), and the module-tier vocabularies (asset phases, shipment
# carriers, PO statuses, …). crates/core/boss-testing/tests/
# example_reference_rows_sql.rs holds that line: after an eviction on
# the bare schema, what remains is exactly what the platform's own
# baseline, workflows and schema defaults name.
#
# THE INSTANCE'S OWN TENANT IS SUBTRACTED FIRST (backlog 86835bf9).
# Measured 2026-09-18 on the first --for-real run on prod (ops-request
# 8522ad76): the candidate set is derived from the example seeds BY
# CODE, and Algedonic declares four employee departments — finance,
# marketing, sales, support — under codes the device shop also uses.
# No employee held them yet, so they were unreferenced, and they went
# with the residue. A row the instance's own tenant declares is not
# residue whatever its code, so `plan-sql` and `delete-sql` take the
# tenant directory (the one the launcher publishes — BOSS_TENANT_DIR
# on a pod, the converge's checkout on the forge), read its
# declarations with the same readers (seeds/classes.*, locations.toml,
# chart_of_accounts.toml, the manifest's tenant_id), and remove every
# matching key from the candidate set BEFORE the judgement — before,
# not as a reason, so a tenant-declared child keeps its example parent
# (the parent_code / parent_id reasons skip children that are
# themselves candidates). `seeds <dir>` names what was subtracted as
# `declared_by_tenant`. A tenant directory that cannot be read is a
# refusal (exit 4): the verb never plans without it.
#
# DELETABLE ONLY WHEN UNREFERENCED. A row is kept, and named with the
# reason, when anything points at it: an employee wearing the role /
# department, a location wearing the kind, an account of the type, an
# asset model in the category, a vendor / product / invoice line /
# document / account-team row wearing the code, a policy grant naming
# the role, a child class with it as parent; an employee or
# requisition or product inventory at the location, a location under
# it, a job about it; a job about the company; a journal line or daily
# balance on the account, a tax kind or filing naming it, a registry
# posting rule naming it, a child account; a tax filing naming the tax
# kind (a sales-tax rate is referenced by nothing: it is always
# deletable when present). A child that is itself deletable does not
# keep its parent, and a tax kind that is itself deletable does not
# keep its accounts (backlog 7f163e58: until 2026-09-18 the
# migration's five kinds kept 2150 / 2300 / 2310 / 2320 / 6500 under
# the demo's names on every instance), so the plan predicts the real run.
# A member_attribute this script has no column for is KEPT and named
# `unmapped:<attribute>` — never deleted on a guess.
#
# ONE TRANSACTION PER TABLE, in dependency order: companies, locations
# (with their `subjects` projection rows), tax_kinds (they FK the
# accounts), sales_tax_rate_by_state, gl_accounts, classes last
# (a location's kind is a class). Each transaction re-judges from the
# live state with the same CTEs the plan used, so nothing referenced
# between the plan and the run is deleted.
#
# USAGE
#   example-reference-rows.sh seeds [<dir>]     the candidate set, one JSON
#                                               line; with the tenant <dir>,
#                                               after subtracting its
#                                               declarations, named under
#                                               `declared_by_tenant`
#   example-reference-rows.sh plan-sql <dir>    the read-only judgement: one
#                                               SELECT printing one JSON line
#   example-reference-rows.sh delete-sql <dir>  the eviction: one transaction
#                                               per table, each printing one
#                                               JSON line
#   example-reference-rows.sh boot <dir>        for init.sh: exit 0 and say
#                                               `evict: tenant <id>` when <dir>
#                                               is a tenant directory whose id
#                                               is not an example's; exit 3
#                                               and say why otherwise (no dir,
#                                               no manifest, an example
#                                               tenant). Nothing else.
#   <dir> is the instance's OWN tenant directory: tenant.toml or
#   seeds/tenant.toml, and seeds/ (docs/tenant-contract.md).
#
# EXIT
#   0  answered
#   3  boot: keep the rows (the reason is on stdout)
#   4  cannot answer — a seed could not be read, a set came out empty,
#      a seed names a member_attribute this script cannot judge, or the
#      tenant directory is not one / cannot be read
#   2  usage
#
# ENV
#   BOSS_EXAMPLES_DIR   the examples directory (default: the one beside
#                       this script's checkout — /opt/boss/examples in
#                       the image, <checkout>/examples on the forge)

set -uo pipefail

ME="example-reference-rows"
say() { echo "$ME: $*" >&2; }
CANNOT_ANSWER=4

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$SELF_DIR/../.." && pwd)"
EXAMPLES="${BOSS_EXAMPLES_DIR:-$REPO/examples}"

USAGE="usage: $ME seeds [<tenant-dir>] | plan-sql <tenant-dir> | delete-sql <tenant-dir> | boot <tenant-dir>"
MODE="${1:-}"
TENANT="${2:-}"
case "$MODE" in
    seeds) [ "$#" -le 2 ] || { say "$USAGE"; exit 2; } ;;
    # The tenant directory is REQUIRED for the SQL: a plan without it is
    # the plan that deleted Algedonic's departments (86835bf9).
    plan-sql|delete-sql|boot) [ "$#" -eq 2 ] || { say "$USAGE"; exit 2; } ;;
    *) say "$USAGE"; exit 2 ;;
esac

command -v jq >/dev/null 2>&1 || { say "CANNOT ANSWER — jq is not on PATH"; exit "$CANNOT_ANSWER"; }
[ -d "$EXAMPLES" ] || { say "CANNOT ANSWER — no examples directory at $EXAMPLES"; exit "$CANNOT_ANSWER"; }

# --- reading the seeds --------------------------------------------------------

# `[meta] tenant_id` from tenant.toml at either spelling the contract
# accepts (docs/tenant-contract.md): <dir>/tenant.toml, else
# <dir>/seeds/tenant.toml. Empty when neither holds one.
tenant_id_of() { # <tenant dir>
    local f
    for f in "$1/tenant.toml" "$1/seeds/tenant.toml"; do
        [ -f "$f" ] || continue
        awk '$0 == "[meta]" {m=1; next} /^\[/ {m=0} m && index($0, "tenant_id") == 1 {
                v=$0; sub(/^tenant_id[ \t]*=[ \t]*"/, "", v); sub(/".*$/, "", v); print v; exit }' "$f"
        return
    done
}

# Every `[[<header>]]` block of a TOML file as one JSON object of its
# top-level `key = "string"` / `key = number` lines (arrays, inline
# tables and nested tables are skipped — only the keys the candidate
# set needs are read: subject_kind, code, member_attribute, id).
toml_blocks() { # <file> <header>
    awk -v h="[[$2]]" '
        function flush() { if (open) { print "{" body "}"; body = ""; open = 0 } }
        $0 == h { flush(); open = 1; next }
        /^\[/ { flush(); next }
        open && match($0, /^[A-Za-z_][A-Za-z0-9_-]*[ \t]*=[ \t]*/) {
            k = substr($0, 1, RLENGTH); sub(/[ \t]*=[ \t]*$/, "", k)
            v = substr($0, RLENGTH + 1)
            if (v ~ /^"/) { sub(/^"/, "", v); sub(/"[ \t]*(#.*)?$/, "", v); gsub(/\\/, "\\\\", v); gsub(/"/, "\\\"", v); v = "\"" v "\"" }
            else if (v ~ /^-?[0-9]+([ \t]*(#.*)?)?$/) { sub(/[ \t].*$/, "", v) }
            else next
            body = body (body == "" ? "" : ",") "\"" k "\":" v
        }
        END { flush() }' "$1" | jq -s .
}

# The example tenants: every examples/<name>/ holding a manifest.
example_dirs() {
    local d
    for d in "$EXAMPLES"/*/; do
        d="${d%/}"
        [ -n "$(tenant_id_of "$d")" ] && printf '%s\n' "$d"
    done
}

# The key sets ONE tenant directory declares, as one JSON object
# {classes, locations, gl_accounts, companies, sources} — the same
# reader for an example (whose rows are the candidates) and for the
# instance's own tenant (whose rows are subtracted). `sources` names
# each file read, relative to <base>. Returns 1 when a seed cannot be
# parsed — a refusal upstream, never a smaller set.
dir_sets() { # <tenant dir> <base for source names>
    local d="$1" base="$2" id classes='[]' locations='[]' accounts='[]' companies='[]' tax_kinds='[]' tax_rates='[]' sources='[]' n
    id=$(tenant_id_of "$d")
    companies=$(jq -c --arg id "$id" '. + [$id]' <<<"$companies")
    if [ -f "$d/seeds/classes.json" ]; then
        n=$(jq -c 'map({subject_kind, code, member_attribute: (.member_attribute // null)})' "$d/seeds/classes.json") || return 1
        classes=$(jq -c --argjson n "$n" '. + $n' <<<"$classes")
        sources=$(jq -c --arg s "${d#"$base"/}/seeds/classes.json" '. + [$s]' <<<"$sources")
    fi
    if [ -f "$d/seeds/classes.toml" ]; then
        n=$(toml_blocks "$d/seeds/classes.toml" class | jq -c 'map({subject_kind, code, member_attribute: (.member_attribute // null)})') || return 1
        classes=$(jq -c --argjson n "$n" '. + $n' <<<"$classes")
        sources=$(jq -c --arg s "${d#"$base"/}/seeds/classes.toml" '. + [$s]' <<<"$sources")
    fi
    if [ -f "$d/seeds/locations.toml" ]; then
        n=$(toml_blocks "$d/seeds/locations.toml" location | jq -c 'map(.id)') || return 1
        locations=$(jq -c --argjson n "$n" '. + $n' <<<"$locations")
        sources=$(jq -c --arg s "${d#"$base"/}/seeds/locations.toml" '. + [$s]' <<<"$sources")
    fi
    if [ -f "$d/seeds/chart_of_accounts.toml" ]; then
        n=$(toml_blocks "$d/seeds/chart_of_accounts.toml" account | jq -c 'map(.code)') || return 1
        accounts=$(jq -c --argjson n "$n" '. + $n' <<<"$accounts")
        sources=$(jq -c --arg s "${d#"$base"/}/seeds/chart_of_accounts.toml" '. + [$s]' <<<"$sources")
    fi
    if [ -f "$d/seeds/tax.toml" ]; then
        n=$(toml_blocks "$d/seeds/tax.toml" tax_kind | jq -c 'map(.kind)') || return 1
        tax_kinds=$(jq -c --argjson n "$n" '. + $n' <<<"$tax_kinds")
        n=$(toml_blocks "$d/seeds/tax.toml" sales_tax_rate | jq -c 'map(.state)') || return 1
        tax_rates=$(jq -c --argjson n "$n" '. + $n' <<<"$tax_rates")
        sources=$(jq -c --arg s "${d#"$base"/}/seeds/tax.toml" '. + [$s]' <<<"$sources")
    fi
    # Every row must carry its key, or the extraction is broken, not
    # the seed.
    jq -n -c \
        --argjson classes "$classes" --argjson locations "$locations" \
        --argjson accounts "$accounts" --argjson companies "$companies" \
        --argjson tax_kinds "$tax_kinds" --argjson tax_rates "$tax_rates" --argjson sources "$sources" '
        if ($classes | map(select(.subject_kind == null or .code == null)) | length) > 0
        then error("a classes row without subject_kind or code") else . end
        | if ($locations | map(select(. == null)) | length) > 0 then error("a location row without id") else . end
        | if ($accounts | map(select(. == null)) | length) > 0 then error("an account row without code") else . end
        | if ($tax_kinds | map(select(. == null)) | length) > 0 then error("a tax_kind row without kind") else . end
        | if ($tax_rates | map(select(. == null)) | length) > 0 then error("a sales_tax_rate row without state") else . end
        | {classes: $classes, locations: $locations, gl_accounts: $accounts, companies: $companies,
           tax_kinds: $tax_kinds, sales_tax_rates: $tax_rates, sources: $sources}'
}

seed_sets() {
    local d all='[]' one
    while IFS= read -r d; do
        [ -n "$d" ] || continue
        one=$(dir_sets "$d" "$EXAMPLES") || return 1
        all=$(jq -c --argjson one "$one" '. + [$one]' <<<"$all")
    done < <(example_dirs)
    # Duplicates across tenants (both carry `ceo`) collapse.
    jq -c '{
        classes: (map(.classes) | add | unique_by([.subject_kind, .code]) | sort_by([.subject_kind, .code])),
        locations: (map(.locations) | add | unique | sort),
        gl_accounts: (map(.gl_accounts) | add | unique | sort),
        companies: (map(.companies) | add | unique | sort),
        tax_kinds: (map(.tax_kinds) | add | unique | sort),
        sales_tax_rates: (map(.sales_tax_rates) | add | unique | sort),
        sources: (map(.sources) | add)
    }' <<<"$all"
}

# The candidate set with the instance's own tenant subtracted
# (86835bf9): every (subject_kind, code), location id, account code
# and company id the tenant declares leaves the set, and what left is
# named under `declared_by_tenant` so the record can say `kept …
# declared by tenant:<id>`. Exit 4 when the directory is not a tenant
# directory or a seed of its cannot be read — never a set without it.
subtract_tenant() { # <candidate JSON> <tenant dir> -> JSON
    local seed="$1" dir="$2" id declared
    [ -d "$dir" ] || { say "CANNOT ANSWER — the instance's tenant directory $dir is not a directory, so its own declarations cannot be subtracted; nothing is planned without them"; return "$CANNOT_ANSWER"; }
    id=$(tenant_id_of "$dir")
    [ -n "$id" ] || { say "CANNOT ANSWER — $dir holds no tenant.toml or seeds/tenant.toml with [meta] tenant_id, so it is not a tenant directory; its declarations cannot be subtracted"; return "$CANNOT_ANSWER"; }
    declared=$(dir_sets "$dir" "$dir") || { say "CANNOT ANSWER — a seed under $dir could not be read (above), so the tenant's declarations cannot be subtracted"; return "$CANNOT_ANSWER"; }
    jq -c --argjson t "$declared" --arg id "$id" --arg dir "$dir" '
        ($t.classes | map("\(.subject_kind):\(.code)")) as $tc
        | ($t.locations) as $tl | ($t.gl_accounts) as $ta | ($t.companies) as $tco
        | ($t.tax_kinds) as $tk | ($t.sales_tax_rates) as $ts
        | {
            classes: (.classes | map(select(("\(.subject_kind):\(.code)") as $k | $tc | index($k) | not))),
            locations: (.locations | map(select(. as $k | $tl | index($k) | not))),
            gl_accounts: (.gl_accounts | map(select(. as $k | $ta | index($k) | not))),
            companies: (.companies | map(select(. as $k | $tco | index($k) | not))),
            tax_kinds: (.tax_kinds | map(select(. as $k | $tk | index($k) | not))),
            sales_tax_rates: (.sales_tax_rates | map(select(. as $k | $ts | index($k) | not))),
            sources,
            declared_by_tenant: {
                tenant: $id,
                directory: $dir,
                classes: (.classes | map("\(.subject_kind):\(.code)") | map(select(. as $k | $tc | index($k))) | unique),
                locations: (.locations | map(select(. as $k | $tl | index($k))) | unique),
                gl_accounts: (.gl_accounts | map(select(. as $k | $ta | index($k))) | unique),
                companies: (.companies | map(select(. as $k | $tco | index($k))) | unique),
                tax_kinds: (.tax_kinds | map(select(. as $k | $tk | index($k))) | unique),
                sales_tax_rates: (.sales_tax_rates | map(select(. as $k | $ts | index($k))) | unique),
                sources: $t.sources
            }
          }' <<<"$seed"
}

# --- boot: the decision init.sh takes ---------------------------------------
if [ "$MODE" = boot ]; then
    DIR="$2"
    if [ -z "$DIR" ]; then
        echo "keep: no tenant directory named (BOSS_TENANT_DIR unset) — the example rows stay"
        exit 3
    fi
    if [ ! -d "$DIR" ]; then
        echo "keep: $DIR is not a directory — the example rows stay"
        exit 3
    fi
    ID=$(tenant_id_of "$DIR")
    if [ -z "$ID" ]; then
        echo "keep: $DIR holds no tenant.toml or seeds/tenant.toml with [meta] tenant_id — the example rows stay"
        exit 3
    fi
    while IFS= read -r d; do
        [ -n "$d" ] || continue
        if [ "$(tenant_id_of "$d")" = "$ID" ]; then
            echo "keep: tenant $ID is an example (${d#"$EXAMPLES"/}) — its own rows"
            exit 3
        fi
    done < <(example_dirs)
    echo "evict: tenant $ID is not an example — the example rows are residue"
    exit 0
fi

SEED_JSON=$(seed_sets) || { say "CANNOT ANSWER — could not read the seed key sets under $EXAMPLES"; exit "$CANNOT_ANSWER"; }
empty=$(printf '%s' "$SEED_JSON" | jq -r 'to_entries[] | select(.key != "sources" and (.value | length) == 0) | .key')
if [ -n "$empty" ]; then
    say "CANNOT ANSWER — a seed key set came out empty, which is an extraction defect, not an answer: $(printf '%s' "$empty" | tr '\n' ' ')"
    exit "$CANNOT_ANSWER"
fi
# The instance's own tenant, subtracted before anything is judged
# (86835bf9). `seeds` without a directory is the raw example set — the
# tests' independent count; the SQL modes never run without one.
if [ -n "$TENANT" ] || [ "$MODE" != seeds ]; then
    SEED_JSON=$(subtract_tenant "$SEED_JSON" "$TENANT") || exit "$CANNOT_ANSWER"
fi
if [ "$MODE" = seeds ]; then
    printf '%s\n' "$SEED_JSON"
    exit 0
fi
# What the SQL embeds is the candidate set alone: the subtracted keys
# must not appear in the document the judgement reads, on any path.
SEED_JSON=$(jq -c 'del(.declared_by_tenant)' <<<"$SEED_JSON")
case "$SEED_JSON" in
    *'$seed$'*) say "CANNOT ANSWER — a seed value contains the literal tag \$seed\$"; exit "$CANNOT_ANSWER" ;;
esac

# --- the reference map ----------------------------------------------------
#
# (subject_kind, member_attribute) -> the column whose value is the
# code. Read from infra/postgres/schema/*.sql: employees.role /
# department / employment_type / status (10-people.sql),
# account_team_members.role and accounts.account_type / tier
# (22-accounts.sql), locations.kind (01-registries.sql),
# asset_models.category and asset_documents.kind (20-catalog.sql),
# vendors.category / payment_terms (24-inventory.sql),
# products.product_kind / package_unit (25-products.sql),
# invoice_line_items.revenue_category (23-commerce.sql), and the
# policy grant that names an employee role (04-policy.sql). A seed
# attribute outside this map is a refusal to judge, above.
CLASS_REFS=(
    "employee|role|employees|role"
    "employee|role|policy_rules|role"
    "employee|department|employees|department"
    "employee|employment_type|employees|employment_type"
    "employee|status|employees|status"
    "employee|account_team_role|account_team_members|role"
    "location|kind|locations|kind"
    "account|type|accounts|account_type"
    "account|tier|accounts|tier"
    "asset|category|asset_models|category"
    "asset|document-kind|asset_documents|kind"
    "vendor|category|vendors|category"
    "vendor|payment_terms|vendors|payment_terms"
    "product|product_kind|products|product_kind"
    "product|package_unit|products|package_unit"
    "invoice|revenue_category|invoice_line_items|revenue_category"
)
unmapped=$(printf '%s' "$SEED_JSON" | jq -r --arg map "$(printf '%s\n' "${CLASS_REFS[@]}")" '
    ($map | split("\n") | map(split("|") | "\(.[0])|\(.[1])")) as $known
    | .classes | map("\(.subject_kind)|\(.member_attribute // "")") | unique | map(select(. as $k | $known | index($k) | not)) | .[]')
if [ -n "$unmapped" ]; then
    say "CANNOT ANSWER — an example seed names a class attribute this script has no column for, so it cannot judge whether the row is referenced: $(printf '%s' "$unmapped" | tr '\n' ' ')"
    exit "$CANNOT_ANSWER"
fi

# One `WHEN` per map entry: the reason names table.column. Applied to
# a candidate row `c` (subject_kind, code, attr). A location wearing
# the kind does not keep the class when that location is itself being
# evicted (loc_judged.deletable) — the plan then matches the run.
class_reason_cases() {
    local e sk attr t col
    for e in "${CLASS_REFS[@]}"; do
        IFS='|' read -r sk attr t col <<<"$e"
        if [ "$t" = locations ]; then
            printf "        CASE WHEN c.subject_kind = '%s' AND c.attr = '%s' AND EXISTS (SELECT 1 FROM %s r WHERE r.%s = c.code AND r.id NOT IN (SELECT id FROM loc_judged WHERE deletable)) THEN '%s.%s' END,\n" "$sk" "$attr" "$t" "$col" "$t" "$col"
        else
            printf "        CASE WHEN c.subject_kind = '%s' AND c.attr = '%s' AND EXISTS (SELECT 1 FROM %s r WHERE r.%s = c.code) THEN '%s.%s' END,\n" "$sk" "$attr" "$t" "$col" "$t" "$col"
        fi
    done
}

# The judgement, as CTEs every statement below starts from. `present`
# rows are the candidates the database holds; `reasons` is the list of
# what points at each; `deletable` is present with no reason.
judgement_ctes() {
    cat <<SQL
WITH seed AS (SELECT \$seed\$${SEED_JSON}\$seed\$::jsonb AS s),
co_cand AS (SELECT jsonb_array_elements_text(s->'companies') AS id FROM seed),
co_judged AS (
    SELECT c.id,
           array_remove(ARRAY[
               CASE WHEN EXISTS (SELECT 1 FROM jobs j WHERE j.subject_kind = 'company' AND j.subject_id = c.id) THEN 'jobs.subject_id' END
           ], NULL) AS reasons
    FROM companies c WHERE c.id IN (SELECT id FROM co_cand)
),
co AS (SELECT id, reasons, cardinality(reasons) = 0 AS deletable FROM co_judged),
loc_cand AS (SELECT jsonb_array_elements_text(s->'locations') AS id FROM seed),
loc_judged0 AS (
    SELECT l.id,
           array_remove(ARRAY[
               CASE WHEN EXISTS (SELECT 1 FROM employees r WHERE r.location = l.id) THEN 'employees.location' END,
               CASE WHEN EXISTS (SELECT 1 FROM requisitions r WHERE r.location = l.id) THEN 'requisitions.location' END,
               CASE WHEN EXISTS (SELECT 1 FROM finished_product_inventory r WHERE r.location_id = l.id) THEN 'finished_product_inventory.location_id' END,
               CASE WHEN EXISTS (SELECT 1 FROM locations r WHERE r.parent_id = l.id AND r.id NOT IN (SELECT id FROM loc_cand)) THEN 'locations.parent_id' END,
               CASE WHEN EXISTS (SELECT 1 FROM jobs j WHERE j.subject_kind = 'location' AND j.subject_id = l.id) THEN 'jobs.subject_id' END
           ], NULL) AS reasons
    FROM locations l WHERE l.id IN (SELECT id FROM loc_cand)
),
loc_judged AS (SELECT id, reasons, cardinality(reasons) = 0 AS deletable FROM loc_judged0),
tk_cand AS (SELECT jsonb_array_elements_text(s->'tax_kinds') AS kind FROM seed),
tk_judged0 AS (
    SELECT k.kind,
           array_remove(ARRAY[
               CASE WHEN EXISTS (SELECT 1 FROM tax_filings r WHERE r.kind = k.kind) THEN 'tax_filings.kind' END
           ], NULL) AS reasons
    FROM tax_kinds k WHERE k.kind IN (SELECT kind FROM tk_cand)
),
tk_judged AS (SELECT kind, reasons, cardinality(reasons) = 0 AS deletable FROM tk_judged0),
str_cand AS (SELECT jsonb_array_elements_text(s->'sales_tax_rates') AS state FROM seed),
str_judged AS (
    SELECT r.state, ARRAY[]::text[] AS reasons, true AS deletable
    FROM sales_tax_rate_by_state r WHERE r.state IN (SELECT state FROM str_cand)
),
gl_cand AS (SELECT jsonb_array_elements_text(s->'gl_accounts') AS code FROM seed),
gl_judged0 AS (
    SELECT a.id, a.code,
           array_remove(ARRAY[
               CASE WHEN EXISTS (SELECT 1 FROM gl_journal_lines r WHERE r.account_id = a.id) THEN 'gl_journal_lines.account_id' END,
               CASE WHEN EXISTS (SELECT 1 FROM gl_account_daily r WHERE r.account_id = a.id) THEN 'gl_account_daily.account_id' END,
               CASE WHEN EXISTS (SELECT 1 FROM gl_accounts r WHERE r.parent_id = a.id AND r.code NOT IN (SELECT code FROM gl_cand)) THEN 'gl_accounts.parent_id' END,
               CASE WHEN EXISTS (SELECT 1 FROM tax_kinds r WHERE (r.liability_account = a.code OR r.expense_account = a.code) AND r.kind NOT IN (SELECT kind FROM tk_judged WHERE deletable)) THEN 'tax_kinds' END,
               CASE WHEN EXISTS (SELECT 1 FROM tax_filings r WHERE r.liability_account = a.code) THEN 'tax_filings.liability_account' END,
               CASE WHEN EXISTS (SELECT 1 FROM gl_posting_rules r, jsonb_array_elements(r.lines) l WHERE l->>'account_code' = a.code) THEN 'gl_posting_rules.lines' END
           ], NULL) AS reasons
    FROM gl_accounts a WHERE a.code IN (SELECT code FROM gl_cand)
),
gl_judged AS (SELECT id, code, reasons, cardinality(reasons) = 0 AS deletable FROM gl_judged0),
cls_cand AS (SELECT e->>'subject_kind' AS subject_kind, e->>'code' AS code, e->>'member_attribute' AS attr FROM seed, jsonb_array_elements(s->'classes') e),
cls_rows AS (
    SELECT k.subject_kind, k.code, coalesce(k.member_attribute, s.attr) AS attr
    FROM classes k JOIN cls_cand s ON s.subject_kind = k.subject_kind AND s.code = k.code
),
cls_judged0 AS (
    SELECT c.subject_kind, c.code, c.attr,
           array_remove(ARRAY[
$(class_reason_cases)
               CASE WHEN c.attr IS NULL OR c.attr NOT IN ($(printf '%s\n' "${CLASS_REFS[@]}" | cut -d'|' -f2 | sort -u | sed "s/.*/'&'/" | paste -sd,)) THEN 'unmapped:' || coalesce(c.attr, '') END,
               CASE WHEN EXISTS (SELECT 1 FROM classes r WHERE r.subject_kind = c.subject_kind AND r.parent_code = c.code AND (r.subject_kind, r.code) NOT IN (SELECT subject_kind, code FROM cls_cand)) THEN 'classes.parent_code' END
           ], NULL) AS reasons
    FROM cls_rows c
),
cls_judged AS (SELECT subject_kind, code, attr, reasons, cardinality(reasons) = 0 AS deletable FROM cls_judged0)
SQL
}

table_json() { # <table> <judged cte> <key expr> <cand cte>
    cat <<SQL
json_build_object(
    'candidates', (SELECT count(*) FROM $4),
    'present', (SELECT count(*) FROM $2),
    'deletable', (SELECT coalesce(json_agg($3 ORDER BY $3), '[]'::json) FROM $2 WHERE deletable),
    'kept', (SELECT coalesce(json_agg(json_build_object('key', $3, 'reasons', to_json(reasons)) ORDER BY $3), '[]'::json) FROM $2 WHERE NOT deletable)
)
SQL
}

if [ "$MODE" = plan-sql ]; then
    echo "-- retire-example-reference-rows:plan"
    judgement_ctes
    cat <<SQL
SELECT json_build_object(
    'companies',   $(table_json companies co id co_cand),
    'locations',   $(table_json locations loc_judged id loc_cand),
    'tax_kinds',   $(table_json tax_kinds tk_judged kind tk_cand),
    'sales_tax_rates', $(table_json sales_tax_rate_by_state str_judged state str_cand),
    'gl_accounts', $(table_json gl_accounts gl_judged code gl_cand),
    'classes',     $(table_json classes cls_judged "subject_kind || ':' || code" cls_cand)
)
SQL
    exit 0
fi

# delete-sql: one transaction per table, dependency order, each
# re-judging from the live state and printing what it deleted.
echo "-- retire-example-reference-rows:delete companies"
echo "BEGIN;"
judgement_ctes
cat <<'SQL'
, del AS (DELETE FROM companies c USING co WHERE c.id = co.id AND co.deletable RETURNING c.id),
del_subjects AS (DELETE FROM subjects s USING del WHERE s.kind = 'company' AND s.id = del.id RETURNING s.id)
SELECT json_build_object('table', 'companies', 'deleted', (SELECT coalesce(json_agg(id ORDER BY id), '[]'::json) FROM del), 'subjects_deleted', (SELECT count(*) FROM del_subjects));
COMMIT;
SQL
echo "-- retire-example-reference-rows:delete locations"
echo "BEGIN;"
judgement_ctes
cat <<'SQL'
, del AS (DELETE FROM locations l USING loc_judged j WHERE l.id = j.id AND j.deletable RETURNING l.id),
del_subjects AS (DELETE FROM subjects s USING del WHERE s.kind = 'location' AND s.id = del.id RETURNING s.id)
SELECT json_build_object('table', 'locations', 'deleted', (SELECT coalesce(json_agg(id ORDER BY id), '[]'::json) FROM del), 'subjects_deleted', (SELECT count(*) FROM del_subjects));
COMMIT;
SQL
echo "-- retire-example-reference-rows:delete tax_kinds"
echo "BEGIN;"
judgement_ctes
cat <<'SQL'
, del AS (DELETE FROM tax_kinds k USING tk_judged j WHERE k.kind = j.kind AND j.deletable RETURNING k.kind)
SELECT json_build_object('table', 'tax_kinds', 'deleted', (SELECT coalesce(json_agg(kind ORDER BY kind), '[]'::json) FROM del));
COMMIT;
SQL
echo "-- retire-example-reference-rows:delete sales_tax_rates"
echo "BEGIN;"
judgement_ctes
cat <<'SQL'
, del AS (DELETE FROM sales_tax_rate_by_state r USING str_judged j WHERE r.state = j.state AND j.deletable RETURNING r.state)
SELECT json_build_object('table', 'sales_tax_rates', 'deleted', (SELECT coalesce(json_agg(state ORDER BY state), '[]'::json) FROM del));
COMMIT;
SQL
echo "-- retire-example-reference-rows:delete gl_accounts"
echo "BEGIN;"
judgement_ctes
cat <<'SQL'
, del AS (DELETE FROM gl_accounts a USING gl_judged j WHERE a.id = j.id AND j.deletable RETURNING a.code)
SELECT json_build_object('table', 'gl_accounts', 'deleted', (SELECT coalesce(json_agg(code ORDER BY code), '[]'::json) FROM del));
COMMIT;
SQL
echo "-- retire-example-reference-rows:delete classes"
echo "BEGIN;"
judgement_ctes
cat <<'SQL'
, del AS (DELETE FROM classes k USING cls_judged j WHERE k.subject_kind = j.subject_kind AND k.code = j.code AND j.deletable RETURNING k.subject_kind || ':' || k.code AS key)
SELECT json_build_object('table', 'classes', 'deleted', (SELECT coalesce(json_agg(key ORDER BY key), '[]'::json) FROM del));
COMMIT;
SQL
