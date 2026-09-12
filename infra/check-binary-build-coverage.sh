#!/usr/bin/env bash
# Build-coverage check for boss-* binaries.
#
# Background: `cargo build --release --workspace` SKIPS any [[bin]]
# entry whose `required-features` aren't activated either by the
# crate's own `default` feature set OR by some other workspace member
# that depends on the crate with the feature enabled. Today most
# *-api binaries are saved by the second path — orchestrators
# (boss-rebuild, boss-rebuild-all) depend on each domain
# crate with `features = ["postgres"]`, and feature unification
# propagates that activation to the [[bin]] entry. But crates with
# zero workspace consumers (boss-docs at one point, anything new
# under crates/core/ before an orchestrator picks it up) silently
# produce an in-memory-only binary because nothing turns the feature
# on.
#
# Two recent regressions tracked back to this:
#
#   2026-04-28 — boss-docs-api ran in-memory because no workspace
#                member activated boss-docs/postgres. POSTs returned
#                200; rows never landed. Fixed 2026-05-05 by moving
#                postgres into boss-docs's default features.
#   2026-05-05 — boss-audit-integrity-check went missing during the
#                regen because its `bin` feature wasn't activated by
#                anyone. Fixed by moving bin into boss-events default.
#
# This script is the build-time defense. For every workspace [[bin]]
# with required-features:
#   1. If the features are part of the crate's `default`, OK.
#   2. Otherwise, check whether any other workspace Cargo.toml
#      depends on the crate with those features. If yes, OK.
#   3. Otherwise, the bin is silently skipped by the workspace build
#      → flag it.
#
# Usage: ./infra/check-binary-build-coverage.sh
# Exit:  0 — every [[bin]] has its required-features activated
#        1 — at least one bin will be silently skipped (script prints)

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# Pass 1: collect every workspace crate's name + path + default features
# Output: crate_name<TAB>cargo_toml_path<TAB>defaults_csv
while IFS= read -r cargo_file; do
    [[ -z "$cargo_file" ]] && continue
    pkg_name=$(awk '
        /^\[package\]/ { inp=1; next }
        /^\[/ { inp=0 }
        inp && /^name[[:space:]]*=/ {
            line=$0; sub(/^[^"]*"/, "", line); sub(/".*$/, "", line); print line; exit
        }
    ' "$cargo_file")
    [[ -z "$pkg_name" ]] && continue
    defaults=$(awk '
        /^\[features\]/ { inf=1; next }
        /^\[/ { inf=0 }
        inf && /^default[[:space:]]*=/ {
            line=$0; sub(/^[^=]*=[[:space:]]*\[/, "", line); sub(/\].*$/, "", line)
            gsub(/[[:space:]"]/, "", line); print line; exit
        }
    ' "$cargo_file")
    echo -e "$pkg_name\t$cargo_file\t$defaults"
done < <(find "$REPO_ROOT/crates" -name 'Cargo.toml' -not -path '*/target/*' 2>/dev/null) > "$TMP/crates.tsv"

# Pass 2: collect every cross-crate dep + its activated features
# Output: depending_crate<TAB>dep_crate<TAB>features_csv
while IFS=$'\t' read -r pkg_name cargo_file _defaults; do
    awk -v me="$pkg_name" '
        /^\[(dependencies|dev-dependencies|build-dependencies)\]/ { ind=1; next }
        /^\[/ { ind=0 }
        ind && /^[a-zA-Z0-9_-]+[[:space:]]*=/ {
            line=$0
            split(line, a, "=")
            depname=a[1]; gsub(/[[:space:]]/, "", depname)
            # Skip non-boss deps to keep this fast.
            if (depname !~ /^boss-/) next
            features=""
            if (match(line, /features[[:space:]]*=[[:space:]]*\[[^\]]*\]/)) {
                f=substr(line, RSTART, RLENGTH); sub(/^[^=]*=[[:space:]]*\[/, "", f); sub(/\].*$/, "", f)
                gsub(/[[:space:]"]/, "", f); features=f
            }
            print me "\t" depname "\t" features
        }
    ' "$cargo_file"
done < "$TMP/crates.tsv" > "$TMP/deps.tsv"

# Pass 3: collect every [[bin]] with required-features
# Output: crate_name<TAB>bin_name<TAB>required_features_csv
while IFS=$'\t' read -r pkg_name cargo_file _defaults; do
    awk -v pkg="$pkg_name" '
        /^\[\[bin\]\]/ {
            if (name != "" && req != "") print pkg "\t" name "\t" req
            name=""; req=""; in_bin=1; next
        }
        /^\[/ && !/^\[\[bin\]\]/ {
            if (in_bin && name != "" && req != "") print pkg "\t" name "\t" req
            name=""; req=""; in_bin=0
        }
        in_bin && /^name[[:space:]]*=/ {
            line=$0; sub(/^[^"]*"/, "", line); sub(/".*$/, "", line); name=line
        }
        in_bin && /^required-features[[:space:]]*=/ {
            line=$0; sub(/^[^=]*=[[:space:]]*\[/, "", line); sub(/\].*$/, "", line)
            gsub(/[[:space:]"]/, "", line); req=line
        }
        END { if (name != "" && req != "") print pkg "\t" name "\t" req }
    ' "$cargo_file"
done < "$TMP/crates.tsv" > "$TMP/bins.tsv"

# Allowlist: bins intentionally NOT built by the workspace default
# build. Each entry is `<crate>::<bin>::<feature>`. Add to this list
# when a [[bin]] is genuinely opt-in (developer convenience binary,
# not a service deployed in production).
ALLOWLIST=(
    # (empty) — every [[bin]] is built by the default workspace build.
    # boss-brewery-sim was previously gated behind the sim-daemon feature
    # (sqlx); that feature was removed when the daemon became a pure
    # public-API client, so it now ships in the default workspace build
    # like the other boss-brewery-* binaries.
)

# Pass 4: for each bin, check feature satisfaction.
ok=true
declare -a UNSATISFIED=()
while IFS=$'\t' read -r pkg_name bin_name req_features; do
    IFS=',' read -ra needed <<< "$req_features"
    crate_defaults=$(awk -F'\t' -v p="$pkg_name" '$1 == p { print $3; exit }' "$TMP/crates.tsv")

    for f in "${needed[@]}"; do
        # In crate's own default?
        IFS=',' read -ra have <<< "$crate_defaults"
        in_default=false
        for h in "${have[@]}"; do
            if [[ "$h" == "$f" ]]; then in_default=true; break; fi
        done
        if $in_default; then continue; fi

        # Activated by some other workspace member?
        activated=false
        while IFS=$'\t' read -r _depender dep_crate dep_features; do
            [[ "$dep_crate" != "$pkg_name" ]] && continue
            IFS=',' read -ra dep_feats <<< "$dep_features"
            for df in "${dep_feats[@]}"; do
                if [[ "$df" == "$f" ]]; then activated=true; break 2; fi
            done
        done < "$TMP/deps.tsv"
        if $activated; then continue; fi

        # Neither default nor activated. Allowlisted?
        key="$pkg_name::$bin_name::$f"
        allowed=false
        for a in "${ALLOWLIST[@]}"; do
            if [[ "$key" == "$a" ]]; then allowed=true; break; fi
        done
        if $allowed; then continue; fi

        UNSATISFIED+=("$pkg_name::$bin_name needs '$f' but no workspace member activates it")
        ok=false
    done
done < "$TMP/bins.tsv"

# Pass 5: detect bins that DON'T declare required-features but WILL
# silently misbehave at runtime if the heavy feature isn't activated.
#
# Concrete bug shape (boss-classes / boss-locations / boss-subject-kinds
# 2026-05-05): the *-api binary has no `required-features`, so the
# workspace build always produces it. But the crate has a `postgres`
# feature, optional `dep:sqlx`, and main.rs calls
# the Postgres repositories under `#[cfg(feature = "postgres")]`. When
# the feature isn't active the binary does not COMPILE — the in-memory
# serving arms were deleted 2026-09-12 (be793304) — and before that it
# booted, hit a startup guard, and exit-1'd, so systemd restart-looped
# the unit. Either way a mis-declared feature is a broken deploy. Pass 4
# couldn't see this because the bin never appears in $TMP/bins.tsv
# (no required-features → no row).
#
# Detection rule: every `*-api` / `*-rebuild` / `*-files-gc` /
# `*-events-purge` binary in a crate that has a `postgres` feature
# must have postgres activated by ONE of:
#   1. The crate's own `default` features.
#   2. The bin's `required-features`.
#   3. A workspace member that depends on the crate with
#      features = ["postgres"].

# Collect every bin (regardless of required-features) for the
# service-binary-name check — re-walk the Cargo.tomls.
while IFS=$'\t' read -r pkg_name cargo_file _defaults; do
    awk -v pkg="$pkg_name" '
        /^\[\[bin\]\]/ {
            if (name != "") print pkg "\t" name "\t" req
            name=""; req=""; in_bin=1; next
        }
        /^\[/ && !/^\[\[bin\]\]/ {
            if (in_bin && name != "") print pkg "\t" name "\t" req
            name=""; req=""; in_bin=0
        }
        in_bin && /^name[[:space:]]*=/ {
            line=$0; sub(/^[^"]*"/, "", line); sub(/".*$/, "", line); name=line
        }
        in_bin && /^required-features[[:space:]]*=/ {
            line=$0; sub(/^[^=]*=[[:space:]]*\[/, "", line); sub(/\].*$/, "", line)
            gsub(/[[:space:]"]/, "", line); req=line
        }
        END { if (name != "") print pkg "\t" name "\t" req }
    ' "$cargo_file"
done < "$TMP/crates.tsv" > "$TMP/all_bins.tsv"

# Service-bin name patterns. Adjust if a new naming convention lands.
declare -a HEAVY_FEATURES=("postgres")
declare -a SERVICE_BIN_PATTERNS=("-api$" "-rebuild$" "-files-gc$" "-events-purge$" "-recognize$" "-replay-check$" "-facts-rebuild$" "-baseline-seed$")

is_service_bin() {
    local bn="$1"
    for p in "${SERVICE_BIN_PATTERNS[@]}"; do
        if [[ "$bn" =~ $p ]]; then return 0; fi
    done
    return 1
}

declare -a STARTUP_TRAPS=()
while IFS=$'\t' read -r pkg_name bin_name req_features; do
    is_service_bin "$bin_name" || continue
    crate_defaults=$(awk -F'\t' -v p="$pkg_name" '$1 == p { print $3; exit }' "$TMP/crates.tsv")

    for hf in "${HEAVY_FEATURES[@]}"; do
        # Does the crate declare this feature at all? Bail if not.
        cargo_file=$(awk -F'\t' -v p="$pkg_name" '$1 == p { print $2; exit }' "$TMP/crates.tsv")
        crate_has_feature=$(awk -v f="$hf" '
            /^\[features\]/ { inf=1; next }
            /^\[/ { inf=0 }
            inf && /^[a-zA-Z0-9_-]+[[:space:]]*=/ {
                k=$1; gsub(/[[:space:]]/, "", k)
                if (k == f) { print "1"; exit }
            }
        ' "$cargo_file")
        [[ -z "$crate_has_feature" ]] && continue

        # 1. In crate's default features?
        IFS=',' read -ra have <<< "$crate_defaults"
        in_default=false
        for h in "${have[@]}"; do
            if [[ "$h" == "$hf" ]]; then in_default=true; break; fi
        done
        if $in_default; then continue; fi

        # 2. In bin's required-features?
        IFS=',' read -ra reqs <<< "$req_features"
        in_req=false
        for r in "${reqs[@]}"; do
            if [[ "$r" == "$hf" ]]; then in_req=true; break; fi
        done
        if $in_req; then continue; fi

        # 3. Activated by some other workspace member?
        activated=false
        while IFS=$'\t' read -r _depender dep_crate dep_features; do
            [[ "$dep_crate" != "$pkg_name" ]] && continue
            IFS=',' read -ra dep_feats <<< "$dep_features"
            for df in "${dep_feats[@]}"; do
                if [[ "$df" == "$hf" ]]; then activated=true; break 2; fi
            done
        done < "$TMP/deps.tsv"
        if $activated; then continue; fi

        STARTUP_TRAPS+=("$pkg_name::$bin_name has '$hf' feature but neither default, required-features, nor a workspace dep activates it; the bin has no in-memory arm to fall back to and will not compile or deploy")
        ok=false
    done
done < "$TMP/all_bins.tsv"

if $ok; then
    audited=$(wc -l < "$TMP/bins.tsv")
    service_audited=$(awk -F'\t' '{print $2}' "$TMP/all_bins.tsv" | while read -r b; do
        if is_service_bin "$b"; then echo 1; fi
    done | wc -l)
    echo "ok: every [[bin]] with required-features is activated by either its"
    echo "    crate's default features or by a workspace dep declaration."
    echo "    $audited binaries with required-features audited (Pass 4); 0 silent skips."
    echo "    $service_audited service-shaped binaries audited for unactivated"
    echo "    heavy features (Pass 5); 0 startup-guard traps."
    exit 0
fi

if [[ ${#UNSATISFIED[@]} -gt 0 ]]; then
    echo "DRIFT (Pass 4): workspace build silently skips the following binaries —" >&2
    echo "       deploys will install a stale copy if any has a previous" >&2
    echo "       binary on disk; otherwise install_binary() echoes SKIP" >&2
    echo "       and the unit comes up missing." >&2
    for u in "${UNSATISFIED[@]}"; do
        echo "  + $u" >&2
    done
    echo >&2
fi

if [[ ${#STARTUP_TRAPS[@]} -gt 0 ]]; then
    echo "DRIFT (Pass 5): the following service-shaped binaries declare a heavy" >&2
    echo "       feature that nothing activates. With no in-memory arm left to" >&2
    echo "       fall back to (be793304) such a bin does not compile without" >&2
    echo "       the feature, and a build that skips it deploys nothing." >&2
    echo "       Same shape as the boss-classes/" >&2
    echo "       locations/subject-kinds bug 2026-05-05." >&2
    for u in "${STARTUP_TRAPS[@]}"; do
        echo "  + $u" >&2
    done
    echo >&2
fi

echo "Fix options:" >&2
echo "  (a) Add the feature to the crate's default features (cleanest;" >&2
echo "      precedent: boss-events bin → default 2026-05-05;" >&2
echo "      boss-docs postgres → default 2026-05-05)." >&2
echo '  (b) Add `required-features = ["postgres"]` (etc.) to the bin so' >&2
echo "      this lint flags any future regression." >&2
echo "  (c) Add a workspace member that depends on the crate with the" >&2
echo "      feature enabled (e.g. boss-rebuild gates feature-unification" >&2
echo "      for most domain crates today)." >&2
exit 1
