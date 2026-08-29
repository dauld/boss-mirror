#!/usr/bin/env bash
# one-date-format.sh — a timestamp is never sliced for display.
#
# WHY THIS EXISTS. UI program CAR-3 (user-feedback 0ca554bf) collapsed
# six competing money formatters onto formatMoney and the date-display
# sites onto web-kit's formatDate. The money half could be pinned with a
# bare grep for `/ 100`. The date half cannot, and the reason is the
# whole point of this lint being narrow.
#
# `.slice(0, 10)` is used for THREE unrelated things in this codebase,
# and only one of them is a bug:
#
#   1. DISPLAY — `{a.updated_at.slice(0, 10)}` renders a date to a
#      human. This is the defect: it hard-codes an ISO prefix as the
#      display format, so the app shows "2026-08-29" on one page and
#      "Aug 29, 2026" on the next.
#   2. LIST TRUNCATION — `{#each closedJobs.slice(0, 10) as job}` takes
#      the first ten of a list. Nothing to do with dates.
#   3. KEYS — `d.toISOString().slice(0, 10)` produces a YYYY-MM-DD key
#      for grid bucketing, an API query param, a form field value, or a
#      payload written back to the server. formatDate returns
#      "Aug 29, 2026", which is not sortable and not what an API
#      accepts. Rewriting these would change behaviour, not appearance.
#
# A lint keyed on `.slice(0, 10)` would flag all three, and the
# mechanical fix for the false positives — rewriting a list truncation
# into a date format — is exactly the kind of edit that looks clean in
# review and breaks a page.
#
# So this pins the one shape that is unambiguous: a field whose NAME
# ends in `_at` is a timestamp, and slicing a timestamp is only ever
# done to display it. Keys are built from `toISOString()` on a Date, not
# from a `_at` field, so they do not match; list truncations slice a
# collection, so they do not match either.
#
# It deliberately UNDER-catches. `{someDate.slice(0, 10)}` on a field
# not named `_at` still passes. That is the right trade: a lint that
# has to be right about intent is a lint that will be wrong, and the
# cost of a miss here is one inconsistent date, while the cost of a
# false positive is a broken list.

set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.." || exit 1

hits=$(grep -rn --include='*.svelte' --include='*.ts' \
    -E '_at\.slice\(0, ?10\)' apps/ libs/ 2>/dev/null || true)

if [[ -n "$hits" ]]; then
    echo "one-date-format: a timestamp field is sliced for display."
    echo "  Use formatDate() from '@boss/web-kit/ui/date' — it renders the"
    echo "  calendar date the wire carries, without a timezone shift."
    echo "  If the value is a KEY (grid bucket, query param, form value,"
    echo "  API payload) it must not be a _at.slice — build it from"
    echo "  toISOString() so this lint can tell the two apart."
    echo "$hits" | sed 's/^/    /'
    exit 1
fi

count=$(grep -rn --include='*.svelte' --include='*.ts' \
    -E '\.slice\(0, ?10\)' apps/ libs/ 2>/dev/null | wc -l | tr -d ' ')
echo "one-date-format: clean — no timestamp field is sliced for display"
echo "  (${count} other .slice(0, 10) uses remain: list truncations and"
echo "  YYYY-MM-DD keys, both deliberate — see the header of this lint)"
