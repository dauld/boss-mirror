#!/usr/bin/env bash
# a-failed-read-is-not-an-empty-one.sh — a page does not paint an
# outage as its empty state.
#
# WHY THIS EXISTS (backlog 223ebcd6, after 7a7bfc88). One line of
# punctuation manufactures the one forbidden failure mode, silence:
#
#     const pBody = pResp.ok ? await pResp.json() : [];
#
# A non-2xx becomes an empty list, the page renders "no purchase
# orders" / "$0.00 outstanding" / a hidden column, and nothing on it
# says a read failed. It was found by hand on /ux/support, then in six
# more pages; 7a7bfc88 lifted the fix into apps/web/src/data/readState.ts
# and closed with the adoption unfinished, so on 2026-09-23 an operator's
# grep of origin/main b9ffdc81 found it still live in VendorsList (two
# reads, unchanged since 2026-07-01), VendorPage (four) and ExecPage
# (one). A fix adopted page by page is a fix that stops one page short;
# this lint is the half of that fix a review cannot forget.
#
# THE RULE. A response whose `ok` is false is never parsed into an
# empty value — `[]`, `null` or `{}` — on the same line. Record the
# outcome instead (`readStateOfResponse` in data/readState.ts, or a
# `failed` flag the template renders with `.load-failed`), then parse
# only the answer that worked.
#
# WHAT IS EXEMPT, each a judgement a reader can check:
#   * apps/web/src/data/readState.ts — the helper that names the class.
#   * `*.test.ts` / `*.spec.ts` — a test stages a response.
#   * prose — a comment quoting the line to tell its story (reads.ts
#     and readState.ts both do).
#   * the ALLOW list below, file => exact count. Each entry carries its
#     reason, and the count must EQUAL the file's matches: fixing one
#     site without lowering the count leaves a hole shaped like it.
#
# NOT COVERED: the shape split across lines, and a failed `PagedResult`
# arm dropped on the floor (`x.kind === 'ready' ? x.page : null`, the
# AccountsList half of the class) — that one reads the same as a
# legitimate narrowing, so it is readState's `readStateOf` by review,
# not by grep.
#
# EXIT STATUS (house style, infra/lint/lib/git-answer.sh):
#   0  the tree was read and no failed read is painted as an empty one
#   1  a violation, or an allowance that no longer matches its count
#   3  the tree was never read — a fact about the MACHINE; never `clean`
set -euo pipefail
LINT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$LINT_DIR/../.."
# shellcheck source=infra/lint/lib/pattern-scan.sh
. "$LINT_DIR/lib/pattern-scan.sh" || exit 3
# shellcheck source=infra/lint/lib/allowlist.sh
. "$LINT_DIR/lib/allowlist.sh" || exit 3

NAME=a-failed-read-is-not-an-empty-one

# `<x>.ok ? await <y>.json() : []` — with `null` or `{}` in place of
# `[]`, and the await optionally parenthesised. ERE, no `{n}` intervals.
PATTERN='\.ok[[:space:]]*\?[[:space:]]*\(?[[:space:]]*await[[:space:]]+[A-Za-z_$][A-Za-z0-9_$.]*\.json\(\)[[:space:]]*\)?[[:space:]]*:[[:space:]]*(\[\]|null|\{\})'

# file => exact number of matches it may carry.
#   PartsList.svelte — its models, inventory and catalog-parts reads are
#     refused by `primaryDown` two lines above them, which fails the
#     list; the PO read degrades the "on order" counts deliberately
#     (filed as 61c16b17, where the decision belongs).
#   classes.svelte.ts / departments.svelte.ts — the `null` is not
#     painted: the next line refuses anything but an array (or a
#     department list) and sets the registry's `error` arm.
declare -A ALLOW=(
    ["apps/web/src/parts/PartsList.svelte"]=4
    ["libs/web-kit/src/session/classes.svelte.ts"]=1
    ["libs/web-kit/src/session/departments.svelte.ts"]=1
)
allowlist_paths_exist "$NAME" "${!ALLOW[@]}"

hits=$(pattern_scan "$PATTERN" \
    --exclude ':!apps/web/src/data/readState.ts' \
    --exclude ':!*.test.ts' \
    --exclude ':!*.spec.ts' \
    -- 'apps/' 'libs/') || exit $?

declare -A counts=() lines=()
while IFS= read -r hit; do
    [ -n "$hit" ] || continue
    file="${hit%%:*}"
    rest="${hit#*:}"
    code="${rest#*:}"
    # A comment quoting the line is prose, not a read.
    case "$(printf '%s' "$code" | sed 's/^[[:space:]]*//')" in
        //*|\**|/\**) continue ;;
    esac
    counts["$file"]=$(( ${counts["$file"]:-0} + 1 ))
    lines["$file"]="${lines[$file]:-}"$'\n'"    $hit"
done <<EOF
$hits
EOF

fail=0
used=""
for file in "${!counts[@]}"; do
    allowed="${ALLOW[$file]:-0}"
    if [ "${counts[$file]}" -gt "$allowed" ]; then
        echo "$NAME: FAIL $file — ${counts[$file]} swallowed read(s), allowance $allowed:${lines[$file]}" >&2
        fail=1
    elif [ "$allowed" -gt 0 ]; then
        used="$used"$'\n'"$file"
        if [ "${counts[$file]}" -lt "$allowed" ]; then
            echo "$NAME: FAIL $file — allowance $allowed, but only ${counts[$file]} site(s) left; lower it to match" >&2
            fail=1
        fi
    fi
done

if [ "$fail" -ne 0 ]; then
    cat >&2 <<'MSG'

A failed read must not look like an empty one. Record what the read did
and let the page say it (apps/web/src/data/readState.ts):

    xRead = readStateOfResponse('/api/x', resp);   // ok | failed
    const body = xRead.kind === 'ok' ? await resp.json() : [];

and render `.load-failed` (role="alert") when `xRead.kind === 'failed'`,
beside or instead of the rows the read feeds — the empty value is then
never the only record of what happened. An allowance is lowered in
the same change that fixes its site; it is raised only with the reason
written beside it.
MSG
    exit 1
fi
allowlist_entries_used "$NAME" "$used" "${!ALLOW[@]}"
echo "$NAME: ok — no failed read is painted as an empty one"
