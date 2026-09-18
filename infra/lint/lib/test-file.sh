# test-file.sh — the one answer to "is this path a test file", for
# every lint that exempts tests from a production rule.
#
# WHY THIS EXISTS (backlog e9c77544, 2026-09-18). no-employee-id-literal
# and no-wallclock each carried their own test-region parser, and both
# knew two shapes: a path under `tests/`, and an inline
# `#[cfg(test)] mod tests { … }`. A test module declared as a whole
# FILE — `#[cfg(test)] mod tests;` in the parent, the tests in
# `tests.rs` beside it — was read as production by both, so the H1
# builder kept conductor.rs's 4,651 lines with its tests inline to stay
# green: the lint dictating file layout. The file-level shape is the
# same declaration with the body moved out, and the fact that makes the
# file a test file sits in the PARENT, so that is where this reads it.
#
# THE RULE. A path is a test file when
#   * it lies under a `tests/` directory (the shape both lints had), or
#   * it is `<dir>/<name>.rs` or `<dir>/<name>/mod.rs` and a Rust file
#     that can be its parent — `<dir>.rs`, or any `<dir>/*.rs` (mod.rs,
#     lib.rs, main.rs, a bin's crate root) — declares `mod <name>;`
#     under `#[cfg(test)]`, the attribute on its own line (other
#     attributes and doc comments may sit between) or on the
#     declaration's; or declares it bare and is itself a test file, so
#     a submodule of a whole-file test module is one end to end.
# The DECLARATION is the fact, not the name: a `tests.rs` nobody
# declares under `#[cfg(test)]` is production. The inline region
# (`mod tests { … }`) stays each lint's own business — that is a span
# inside a file, and this predicate answers for whole files.
#
# Answers are memoised per path: no-wallclock asks once per hit and a
# file can carry many.
#
# USAGE
#   . "$LINT_DIR/lib/test-file.sh"
#   if is_test_file "$file"; then …   # exit 0 = test file, 1 = not
#
# Pinned by crates/core/boss-testing/tests/a_whole_file_test_module_is_a_test_file.rs.

declare -gA TEST_FILE_MEMO=()

# _declares_test_mod <parent> <name>
#
# Does <parent> declare `mod <name>;` under `#[cfg(test)]`? The pending
# state is the one no-wallclock's item parser uses: the attribute is
# followed by stacked attributes, doc comments and blank lines, then
# the item it guards. No `\s`: mawk reads it as a literal `s`.
_declares_test_mod() {
    awk -v name="$2" '
        function is_decl(s) {
            return s ~ ("^[[:space:]]*(pub([(][a-z]+[)])?[[:space:]]+)?mod[[:space:]]+" name "[[:space:]]*;")
        }
        /^[[:space:]]*#\[cfg\(test\)\]/ {
            seg = $0
            sub(/^[[:space:]]*#\[cfg\(test\)\]/, "", seg)
            if (seg ~ /[^[:space:]]/) { if (is_decl(seg)) { found = 1; exit }; next }
            pending = 1
            next
        }
        pending && /^[[:space:]]*(#\[|\/\/|$)/ { next }
        pending { if (is_decl($0)) { found = 1; exit }; pending = 0 }
        END { exit found ? 0 : 1 }
    ' "$1"
}

# is_test_file <path>
is_test_file() {
    local path="$1" dir name parent
    if [ -n "${TEST_FILE_MEMO[$path]+x}" ]; then
        return "${TEST_FILE_MEMO[$path]}"
    fi
    TEST_FILE_MEMO[$path]=1
    case "$path" in
        */tests/*|tests/*) TEST_FILE_MEMO[$path]=0; return 0 ;;
        *.rs) ;;
        *) return 1 ;;
    esac
    dir="$(dirname -- "$path")"
    name="$(basename -- "$path" .rs)"
    if [ "$name" = "mod" ]; then
        name="$(basename -- "$dir")"
        dir="$(dirname -- "$dir")"
    fi
    # The candidate parents that mention `mod <name>;` at all — one grep
    # over the siblings that exist, then the attribute check on the few
    # that do. `grep -l` exits 1 for "none", which is an answer; >1 is
    # "could not look", and a file this cannot read is not certified a
    # test file (the lint then names it, which is loud, not silent).
    local siblings=() candidates
    for parent in "$dir.rs" "$dir"/*.rs; do
        [ -f "$parent" ] && [ "$parent" != "$path" ] && siblings+=("$parent")
    done
    [ "${#siblings[@]}" -gt 0 ] || return 1
    candidates="$(grep -lE "^[[:space:]]*(#\[cfg\(test\)\][[:space:]]*)?(pub(\([a-z]+\))?[[:space:]]+)?mod[[:space:]]+$name[[:space:]]*;" \
        -- "${siblings[@]}")" || return 1
    while IFS= read -r parent; do
        [ -n "$parent" ] || continue
        if _declares_test_mod "$parent" "$name" || is_test_file "$parent"; then
            TEST_FILE_MEMO[$path]=0
            return 0
        fi
    done <<< "$candidates"
    return 1
}
