#!/usr/bin/env bash
#
# no-tenant-vocabulary-above-its-tier — the sensor, with a ratchet, on
# tenant words leaking into the tiers above the tenant (backlog
# be39298f).
#
# THE PRINCIPLE
# -------------
# BOSS core is a generic state-machine toolkit; the brewery and the
# used-device shop are tenants instantiated on it, and CLAUDE.md §10
# says no tenant-specific assumption lives in core. Until 2026-09-16
# that rule was enforced only as a Cargo dependency audit
# (tier-import-audit.sh), which sees crate edges and nothing else. A
# grep for the brewery's own words outside the brewery found them in
# every tier above it — 362 files: kegs in core registries, excise in
# module ledgers, the brewery simulator in an orchestrator crate.
# David, the same day: development moves on without the brewery
# tenant, "because I am guessing we will need to clear out a lot of
# hardcoded brewery code still."
#
# Clearing it is one car per concentration. This is the instrument
# those cars are measured with, and the ratchet that stops the number
# rising between them.
#
# THE CHECKED PROPERTY
# --------------------
# For every tenant that declares `examples/<tenant>/VOCABULARY` — one
# term per line, the tenant's own words, `*` for a word-start prefix —
# count the case-insensitive occurrences of every term in each TIER
# (the roots below), over .rs .ts .svelte .toml .sql .sh .yaml, and
# compare each tier's count to infra/lint/tenant-vocabulary.baseline
# (one `<tier> <count>` line per tier). Then, per tier:
#
#   count > baseline  -> REFUSED, naming the files: the leak grew.
#   count = baseline  -> clean.
#   count < baseline  -> REFUSED, telling the author to lower that
#                        tier's line in the same car. The ratchet only
#                        goes down, and refusing a baseline ABOVE the
#                        count is what stops anyone raising it.
#
# The count is derived from the tree on every run; the baseline is the
# one fact that lives twice, and the equality above is its test
# (CLAUDE.md §9a). It was written from this lint's own measurement
# when it landed, not from the packet's approximate grep — this file
# is the definition.
#
# WHAT IS NOT COUNTED, and why
#   - the tenant's own homes: examples/<tenant>/ and
#     crates/tenants/<its engine>/ are not tiers above the tenant, and
#     neither is a root below;
#   - test FILES: any path under a /tests/ directory, *_test.rs,
#     *.test.*, *.spec.*. A `#[cfg(test)]` block inside a source file
#     IS counted — telling one apart needs a parser, and a lint that
#     guessed would count differently from the number an author sees.
#     Move a tenant-flavoured test into tests/ or into the tenant;
#   - docs, worktrees, node_modules, target/: none is under a root;
#   - a tenant's NAME used as a PATH or as an ID: `examples/<tenant>`
#     (so `examples/brewery/seeds/tenant.toml`, `/opt/boss/examples/
#     brewery/data`) and `tenant_id = "<tenant>"`. The product must be
#     able to say which tenant an instance runs, and that is not the
#     tenant's vocabulary leaking. Measured the day this lint was
#     written: the car rendering every instance from one manifest
#     (infra/cluster/instances.toml, `tenant = "examples/brewery/seeds/
#     tenant.toml"`) was clean on its own gate and red beside this one
#     on the assembled tree, for naming the playground's tenant path.
#     ONLY those two forms, for the names of tenants that declare a
#     VOCABULARY; the same name as a bare word, or as any other key's
#     value, still counts.
#
# A wrong path answers 0 instead of erroring (CLAUDE.md §Doors), so a
# missing tier root, a missing VOCABULARY, a malformed term, and a
# baseline that names a tier this lint does not (or misses one it does)
# are all refusals, never a smaller count.
#
# Usage:  infra/lint/no-tenant-vocabulary-above-its-tier.sh
set -uo pipefail

LINT=no-tenant-vocabulary-above-its-tier
cd "$(dirname "${BASH_SOURCE[0]}")/../.." || exit 1

BASELINE="infra/lint/tenant-vocabulary.baseline"

# The tiers above a tenant. Order is the report's order; the baseline
# file may list them in any order but must list each exactly once.
TIERS=(
    crates/core
    crates/modules
    crates/orchestrators
    apps/web/src
    apps/simulator
    infra/postgres/schema
    infra/platform
    infra/cluster
)

refuse() { printf '%s: %s\n' "$LINT" "$*" >&2; exit 1; }

for tier in "${TIERS[@]}"; do
    [ -d "$tier" ] || refuse "tier root $tier is not a directory here — a missing root would count 0, which is a wrong path, not a clean tier"
done

# ---- the word list: every tenant's VOCABULARY, merged --------------
shopt -s nullglob
vocab_files=(examples/*/VOCABULARY)
shopt -u nullglob
[ "${#vocab_files[@]}" -gt 0 ] \
    || refuse "no examples/*/VOCABULARY found — zero words would count zero hits everywhere and certify nothing"

# One ERE alternation. A bare term is a whole word (`wort`, not
# `worth`); a trailing `*` is a word-start prefix (`brew*` reaches
# brewery, brewhouse, brewing). The term alphabet is restricted so no
# escaping is ever needed and a stray metacharacter cannot widen the
# match silently.
pattern=""
terms=0
tenants=""
for vf in "${vocab_files[@]}"; do
    tenant=${vf#examples/}; tenant=${tenant%/VOCABULARY}
    tenants="${tenants:+$tenants, }$tenant"
    while IFS= read -r line || [ -n "$line" ]; do
        line=${line%%#*}
        line=${line#"${line%%[![:space:]]*}"}
        line=${line%"${line##*[![:space:]]}"}
        [ -n "$line" ] || continue
        case "$line" in
            *'*') term=${line%\*}; tail='' ;;
            *)    term=$line;      tail='\b' ;;
        esac
        case "$term" in
            ''|*[!a-z0-9\ -]*)
                refuse "$vf: term '$line' — terms are lowercase letters, digits, spaces and hyphens, with an optional trailing *" ;;
        esac
        pattern="${pattern:+$pattern|}\\b${term}${tail}"
        terms=$((terms + 1))
    done < "$vf"
done
[ "$terms" -gt 0 ] || refuse "every VOCABULARY is empty — nothing to count"

# ---- the count: one grep per tier, occurrences per file ------------
# `-o` so a line carrying two terms counts two; `-H` so every hit
# carries its file. Test files are dropped by path AFTER the grep so
# the same file set is scanned whatever grep's --exclude semantics are.
# Paths here never contain ':' (the separator); a tree that adds one
# would miscount and this is where to fix it.
hits=""
for tier in "${TIERS[@]}"; do
    tier_hits=$(grep -rIioHE \
        --include='*.rs' --include='*.ts' --include='*.svelte' \
        --include='*.toml' --include='*.sql' --include='*.sh' \
        --include='*.yaml' \
        -e "$pattern" -- "$tier" 2>/dev/null \
        | grep -vE '^[^:]*(/tests/|_test\.rs:|\.test\.[^/:]*:|\.spec\.[^/:]*:)' \
        || true)
    [ -n "$tier_hits" ] && hits="${hits:+$hits
}$tier_hits"
done

# ---- the exemption: a tenant's name as a path or as an id ----------
# The same grep, for the two exempt forms, over the names of the
# tenants that declared a VOCABULARY. Each exempt occurrence is
# SUBTRACTED from its file's count, weighted by how many vocabulary
# hits that exact literal carries (`examples/brewery` is one `brew*`
# hit; a tenant whose name were two terms would be two), so the
# subtraction is exact and never reaches a word outside the form.
names=""
for vf in "${vocab_files[@]}"; do
    n=${vf#examples/}; n=${n%/VOCABULARY}
    names="${names:+$names|}$n"
done
exempt_pattern="examples/($names)\\b|\\btenant_id *= *\"($names)\""
exempt=""
for tier in "${TIERS[@]}"; do
    tier_exempt=$(grep -rIioHE \
        --include='*.rs' --include='*.ts' --include='*.svelte' \
        --include='*.toml' --include='*.sql' --include='*.sh' \
        --include='*.yaml' \
        -e "$exempt_pattern" -- "$tier" 2>/dev/null \
        | grep -vE '^[^:]*(/tests/|_test\.rs:|\.test\.[^/:]*:|\.spec\.[^/:]*:)' \
        || true)
    [ -n "$tier_exempt" ] && exempt="${exempt:+$exempt
}$tier_exempt"
done
# `<file> <weight>` per exempt occurrence: the literal's own hit count,
# looked up once per distinct literal (there are a handful).
exempt_weighted=$(
    declare -A weight
    while IFS= read -r line; do
        [ -n "$line" ] || continue
        file=${line%%:*}; lit=${line#*:}
        if [ -z "${weight[$lit]+x}" ]; then
            weight[$lit]=$(printf '%s' "$lit" | grep -oiE -e "$pattern" | wc -l)
        fi
        printf '%s %s\n' "$file" "${weight[$lit]}"
    done <<< "$exempt"
)

# `<count> <file>` per file, descending — raw hits minus the exempt
# weight, files at zero dropped — and `<count> <tier>` per tier.
per_file=$(
    {
        printf '%s\n' "$hits" | sed '/^$/d' | cut -d: -f1 | awk '{ print $1, 1 }'
        printf '%s\n' "$exempt_weighted" | sed '/^$/d' | awk '{ print $1, -$2 }'
    } | awk '{ n[$1] += $2 } END { for (f in n) if (n[f] > 0) print n[f], f }' \
      | LC_ALL=C sort -k1,1nr -k2,2
)

count_of() {
    # Hits under one tier root, summed from the per-file list.
    local root="$1"
    printf '%s\n' "$per_file" | awk -v r="$root/" '
        index($2, r) == 1 { n += $1 }
        END { print n + 0 }'
}

# ---- the baseline ---------------------------------------------------
[ -f "$BASELINE" ] || refuse "$BASELINE is missing — write one line per tier, '<tier> <count>', from this lint's own table"

baseline_of() {
    local tier="$1" n
    n=$(sed -e 's/#.*//' "$BASELINE" | awk -v t="$tier" '$1 == t { print $2 }')
    case "${n:-empty}" in
        empty)        refuse "$BASELINE has no line for tier $tier — add '$tier <count>' from the table this lint prints" ;;
        *[!0-9]*)     refuse "$BASELINE: tier $tier has a non-numeric or repeated count ('$n')" ;;
    esac
    printf '%s\n' "$n"
}

# A tier named in the baseline that this lint does not scan is a line
# nobody is ratcheting — refused, so a renamed tier cannot leave a stale
# number behind that reads as covered.
while read -r name _; do
    [ -n "$name" ] || continue
    case " ${TIERS[*]} " in
        *" $name "*) ;;
        *) refuse "$BASELINE names tier '$name', which this lint does not scan — remove the line or add the tier to TIERS" ;;
    esac
done < <(sed -e 's/#.*//' "$BASELINE")

# ---- the verdict, one line per tier ---------------------------------
echo "$LINT: tenant words above their tier ($tenants; $terms terms)"
echo
printf '  %-24s %7s %9s\n' tier count baseline
grew=0; fell=0
for tier in "${TIERS[@]}"; do
    count=$(count_of "$tier")
    base=$(baseline_of "$tier") || exit 1
    if [ "$count" -gt "$base" ]; then
        note="GREW by $((count - base))"; grew=$((grew + 1))
    elif [ "$count" -lt "$base" ]; then
        note="fell by $((base - count)) — lower the baseline"; fell=$((fell + 1))
    else
        note="at baseline"
    fi
    printf '  %-24s %7s %9s  %s\n' "$tier" "$count" "$base" "$note"
done
echo
echo "  top 10 files:"
# The limit lives in awk, never in a `| head` after a multi-line
# writer: under pipefail a reader that exits early SIGPIPEs the writer
# and the script reports 141 for a list that IS there (backlog 28af807c).
awk 'NR <= 10 { printf "  %6s  %s\n", $1, $2 }' <<< "$per_file"

if [ "$grew" -gt 0 ]; then
    echo >&2
    echo "$LINT: REFUSED — $grew tier(s) carry more tenant vocabulary than their baseline." >&2
    echo "  A tenant's words in a tier above it are a tenant assumption in core" >&2
    echo "  (CLAUDE.md §10). Move the code to crates/tenants/<engine> or to" >&2
    echo "  tenant data under examples/<tenant>/, or express it as a registry" >&2
    echo "  row the tenant seeds. The files that carry the growth, per tier:" >&2
    for tier in "${TIERS[@]}"; do
        count=$(count_of "$tier"); base=$(baseline_of "$tier")
        [ "$count" -gt "$base" ] || continue
        awk -v r="$tier/" 'index($2, r) == 1 && n++ < 10 { printf "    %6s  %s\n", $1, $2 }' <<< "$per_file" >&2
    done
    echo "  The baseline in $BASELINE is never raised." >&2
    exit 1
fi

if [ "$fell" -gt 0 ]; then
    echo >&2
    echo "$LINT: REFUSED — $fell tier(s) sit BELOW their baseline. Good: the leak" >&2
    echo "  shrank. Now lower each tier's line in $BASELINE to the count in the" >&2
    echo "  table above, in this same car, so the ratchet holds the new number." >&2
    echo "  (A baseline above the count is refused so that nobody can raise one.)" >&2
    exit 1
fi

echo
echo "$LINT: clean (every tier at its baseline)"
