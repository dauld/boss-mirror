# tiers.sh — the shell reader of infra/platform/tiers.toml, the ONE
# definition of which tier a path in the tree belongs to (design
# 01c3cc3f, 2026-09-19; infra/platform/tiers.md says why).
#
# WHY A SECOND READER. boss_core::tiers is the loader every Rust
# consumer uses; a lint is a shell script and cannot call it, so this
# file reads the same TOML line by line — which is why tiers.toml keeps
# one key per line and `paths` on one line. This is a §9a pin, not a
# collapse: crates/core/boss-testing/tests/tiers_sh.rs runs both
# readers over one fixture of paths and names the path where they
# disagree. Before this file, tier-import-audit.sh carried the prefixes
# as its own text, a third copy.
#
# USAGE
#   . infra/lint/lib/tiers.sh || exit 3
#   tier_paths core              # the prefixes a tier owns, one per line
#   tier_rank data               # its rank
#   tier_of_path some/file.rs    # the tier name; exit 1 when no row
#                                # claims the path (say so, never guess)
#   BOSS_TIERS_TOML=<file> overrides the file read, for the pin's fixtures.

# The file this reader reads — resolved from this lib's own location so
# a caller's cwd does not matter.
tiers_file() {
    printf '%s\n' "${BOSS_TIERS_TOML:-$(dirname "${BASH_SOURCE[0]}")/../../platform/tiers.toml}"
}

# _tiers_rows — every row as one line: `<name>\t<rank>\t<p1> <p2> ...`
# in file order. The parser: a `[[tier]]` line starts a row, the three
# keys fill it, the next `[[tier]]` (or EOF) emits it. A `#` line is a
# comment. Prefixes never contain a space, so a space separates them.
_tiers_rows() {
    local file line name= rank= paths= started=0
    file=$(tiers_file)
    [ -r "$file" ] || { printf 'tiers.sh: cannot read %s\n' "$file" >&2; return 3; }
    while IFS= read -r line || [ -n "$line" ]; do
        case "$line" in
            \#*|'') continue ;;
            '[[tier]]'*)
                [ "$started" -eq 1 ] && printf '%s\t%s\t%s\n' "$name" "$rank" "$paths"
                started=1; name=; rank=; paths=
                ;;
            name*=*)
                name=${line#*=}; name=${name//\"/}; name=${name// /}
                ;;
            rank*=*)
                rank=${line#*=}; rank=${rank// /}
                ;;
            paths*=*)
                paths=${line#*=}
                paths=${paths//[\[\],]/ }; paths=${paths//\"/}
                # collapse runs of spaces, trim
                read -r paths <<< "$paths"
                ;;
        esac
    done < "$file"
    [ "$started" -eq 1 ] && printf '%s\t%s\t%s\n' "$name" "$rank" "$paths"
    return 0
}

# tier_paths <name> — the prefixes the named tier owns, one per line.
tier_paths() {
    local want="$1" name rank paths p
    while IFS=$'\t' read -r name rank paths; do
        [ "$name" = "$want" ] || continue
        for p in $paths; do printf '%s\n' "$p"; done
        return 0
    done < <(_tiers_rows)
    return 1
}

# tier_rank <name> — the named tier's rank.
tier_rank() {
    local want="$1" name rank paths
    while IFS=$'\t' read -r name rank paths; do
        [ "$name" = "$want" ] || continue
        printf '%s\n' "$rank"
        return 0
    done < <(_tiers_rows)
    return 1
}

# _tier_claimed <prefix> <path> — how many characters of the path the
# prefix claims (its own length; for a `**/x/` glob, up to and
# including the FIRST `x/` directory at any depth), or nothing when it
# does not match. Mirrors boss_core::tiers::claimed exactly.
_tier_claimed() {
    local pat="$1" path="$2" rest pre
    case "$pat" in
        '**/'*)
            rest=${pat#\*\*/}
            if [ "${path#"$rest"}" != "$path" ]; then
                printf '%s\n' "${#rest}"; return 0
            fi
            pre=${path%%/"$rest"*}
            if [ "$pre" != "$path" ]; then
                printf '%s\n' $(( ${#pre} + 1 + ${#rest} )); return 0
            fi
            return 1
            ;;
        *)
            [ "${path#"$pat"}" != "$path" ] || return 1
            printf '%s\n' "${#pat}"
            ;;
    esac
}

# tier_of_path <path> — the tier whose prefix claims the most of the
# path; the first row written wins a tie. Exit 1 and print nothing
# when no row claims it.
tier_of_path() {
    local path="${1#./}" name rank paths p n best=0 best_name=
    while IFS=$'\t' read -r name rank paths; do
        for p in $paths; do
            n=$(_tier_claimed "$p" "$path") || continue
            if [ "$n" -gt "$best" ]; then best=$n; best_name=$name; fi
        done
    done < <(_tiers_rows)
    [ -n "$best_name" ] || return 1
    printf '%s\n' "$best_name"
}
