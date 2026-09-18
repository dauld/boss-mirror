# allowlist.sh — the two questions every allowlisted lint owes its own
# allowlist, asked in one place: does each entry still name something
# in the tree, and did each entry still excuse anything this run.
#
# WHY THIS EXISTS (backlog cdf2d959, tech-debt audit §4, 2026-09-18).
# The audit measured five allowlists and found stale entries in four:
#
#   no-wallclock            4 entries naming files that no longer exist
#                           (boss_brewery_bootstrap.rs, system_parts.rs,
#                           shape_driven/faker.rs, boss-policy in_memory.rs)
#                           and 8 more whose file has no wall-clock call
#                           left to excuse
#   no-step-kind-match      DesignReviewPage.svelte, allowed 1 match,
#                           carrying 0
#   api-path-bypass-smell   check-service-write-roundtrip.sh, allowed as
#                           a diagnostic write, now writing nothing the
#                           classifier reports
#   sim-boundary-audit      six entries that were never compared to
#                           anything (its scan was empty — lib/scanned.sh)
#
# Stale-entry detection existed in exactly ONE of the five (sim-boundary-
# audit's, which was itself dead). Each lint that grows an allowlist
# re-derives the same two checks or, more often, does not. The rule is
# the ratchet's own rule applied to a set instead of a count: an
# allowance that excuses nothing is not neutral. It is a hole shaped
# like a file that no longer exists, and a future file of that name
# walks through it without anyone deciding so — which is exactly how
# gate.sh treats an exclusion naming a lint that is gone ("left
# standing, it would keep a future lint of that name out of the gate").
#
# USAGE
#   . "$LINT_DIR/lib/allowlist.sh"
#   allowlist_paths_exist "$LINT" "${ALLOWED[@]}"
#   ... the scan, appending each entry that excused a hit to $used ...
#   allowlist_entries_used "$LINT" "$used" "${ALLOWED[@]}"
#
# Both print every offending entry and EXIT 1, the same way
# lib/scanned.sh does: a stale allowance is the lint author's to fix
# (delete the entry in the same change that retired the site), and
# returning would let a caller without `set -e` print `clean` on the
# next line. Neither is exit 3 — the tree was read; the list is wrong.

# allowlist_paths_exist <lint> <path>...
#
# Every path must exist as a file or a directory (a prefix entry ends
# in `/` and names a directory; anything else names a file). Prints
# each missing one, then exits 1.
allowlist_paths_exist() {
    local lint="$1"
    shift
    local p missing=0
    for p in "$@"; do
        [ -n "$p" ] || continue
        if [ ! -e "$p" ]; then
            if [ "$missing" -eq 0 ]; then
                printf '%s: stale allowlist — an entry names a path that does not exist:\n' "$lint" >&2
            fi
            printf '    %s\n' "$p" >&2
            missing=$((missing + 1))
        fi
    done
    if [ "$missing" -gt 0 ]; then
        {
            printf '  %s entry/entries excuse a file that is gone. Remove each; an allowance\n' "$missing"
            printf '  naming nothing keeps a future file of that name out of the check without\n'
            printf '  anyone deciding so. (lib/allowlist.sh; backlog cdf2d959)\n'
        } >&2
        exit 1
    fi
}

# allowlist_entries_used <lint> <used> <entry>...
#
# <used> is the newline-separated list of entries that excused at least
# one hit this run (the lint appends to it as it classifies). Every
# entry must appear there; the ones that do not are printed and the
# function exits 1. Matched whole-line (`grep -qxF`) through a
# here-string, never a pipe: a `printf | grep -q` under pipefail SIGPIPEs
# the writer for a match past the first block and reports 141 for an
# entry that IS in the list (backlog 28af807c).
allowlist_entries_used() {
    local lint="$1" used="$2"
    shift 2
    local e unused=0
    for e in "$@"; do
        [ -n "$e" ] || continue
        if ! grep -qxF -- "$e" <<< "$used"; then
            if [ "$unused" -eq 0 ]; then
                printf '%s: stale allowlist — an entry excused nothing this run:\n' "$lint" >&2
            fi
            printf '    %s\n' "$e" >&2
            unused=$((unused + 1))
        fi
    done
    if [ "$unused" -gt 0 ]; then
        {
            printf '  %s entry/entries name a site that no longer trips this lint. Either the site\n' "$unused"
            printf '  was fixed and the entry outlived it, or the file was renamed and the entry\n'
            printf '  is misspelled; in both cases delete it. (lib/allowlist.sh; backlog cdf2d959)\n'
        } >&2
        exit 1
    fi
}
