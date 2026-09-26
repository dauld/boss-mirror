#!/usr/bin/env bash
# a-map-edge-is-served-not-drawn.sh — the IT map draws the routes the
# server serves, and no page source names a station pair of its own.
#
# WHY THIS EXISTS (design e765b3fc, car R3 on feedback 84cba7e2,
# 2026-09-26). Every edge on both /it maps came from one hand-written
# list in three copies — boss_jobs::borders::BORDERS, world.ts BORDERS
# and transit.ts PATHS — pinned equal to each other and to nothing else.
# Measured against the record on 2026-09-25: of ten drawn edges five
# matched a source, three were partly wrong, two carried no packet, and
# ten real routes were drawn nowhere — among them the one David named,
# "like how the dock routes a train over to the gates before it departs
# onto the tracks." The map drew the train from the dock straight to the
# track. A pin between copies cannot catch an edge no packet takes.
# Car R2 derived the routes from the protocols and serves them at
# GET /api/yard/routes; car R3 deleted the two web copies and draws only
# what that read serves. This lint keeps a hand-drawn edge from coming
# back: the next person to want "just one more line" on the map adds a
# protocol step, a hand-off declaration, or nothing.
#
# THE RULE. Under apps/web/src/it/yard/, no code line may carry
#   * a station-pair object literal, `from: 'gates', to: 'dock'` (either
#     end may be `null` — an exit or an entry is an edge too — but not
#     both, and either quote);
#   * a station-pair key literal, `'dock→track'` or `'dock->track'`.
# A pair built from data (`${from}→${to}`, `sectionKey(r.from, r.to)`)
# is not a literal and passes.
#
# WHAT IS EXEMPT, and each is a judgement a reader can check:
#   * tests — `*/tests/*`, `*.test.*`, `*.spec.*`: a test's fixture is
#     the server's answer standing in, not a drawing
#     (apps/web/tests/fixtures/yard.ts holds the shared ones);
#   * comments — `//`, `/* */`, `<!-- -->`: prose may name a route.
# No marker and no allowlist: a map that needs a pair the server does
# not serve needs the server to serve it.
#
# EXIT STATUS (house style, infra/lint/lib/git-answer.sh):
#   0  the tree was read and no page source names a pair
#   1  the tree was read and a pair literal was found — the author's to fix
#   3  the tree was never read — a fact about the MACHINE; never `clean`
#
# USAGE
#   infra/lint/a-map-edge-is-served-not-drawn.sh
#   infra/lint/a-map-edge-is-served-not-drawn.sh --self-test
set -uo pipefail

NAME="a-map-edge-is-served-not-drawn"
SCOPE="apps/web/src/it/yard"
LINT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=infra/lint/lib/scanned.sh
. "$LINT_DIR/lib/scanned.sh" || exit 3
cd "$LINT_DIR/../.." || exit 1
# shellcheck source=infra/lint/lib/git-answer.sh
. "$LINT_DIR/lib/git-answer.sh" || exit 3

# --- the scanner -------------------------------------------------------
# One file's findings, as `<line>\t<code>`. Empty output = clean. No
# `{n}` intervals and no `\s`/`\b`: mawk answers the first by matching
# nothing and reads the others as letters (see lib/scanned.sh).
findings_in() { # file
    awk -v sq="'" '
        # The code half of a line, with comment state carried across lines.
        function strip(s,   out, i, ch, two, four) {
            out = ""
            i = 1
            while (i <= length(s)) {
                ch = substr(s, i, 1); two = substr(s, i, 2); four = substr(s, i, 4)
                if (in_block) {
                    if (two == "*/") { in_block = 0; i += 2 } else i++
                    continue
                }
                if (in_html) {
                    if (substr(s, i, 3) == "-->") { in_html = 0; i += 3 } else i++
                    continue
                }
                if (two == "/*") { in_block = 1; i += 2; continue }
                if (four == "<!--") { in_html = 1; i += 4; continue }
                if (two == "//" && (i == 1 || substr(s, i - 1, 1) != ":")) break
                out = out ch
                i++
            }
            return out
        }
        BEGIN {
            q = "[\"" sq "`]"
            name = q "[a-z][a-z-]*" q
            end = "(" name "|null)"
            gap = "[ \t]*"
            pair = "from" gap ":" gap end gap "," gap "to" gap ":" gap end
            both_null = "from" gap ":" gap "null" gap "," gap "to" gap ":" gap "null"
            arrow = q "[a-z][a-z-]*(→|->)[a-z][a-z-]*" q
        }
        {
            code = strip($0)
            if ((code ~ pair && code !~ both_null) || code ~ arrow) {
                sub(/^[ \t]+/, "", code)
                printf "%d\t%s\n", NR, code
            }
        }
    ' "$1"
}

exempt() { # path
    case "$1" in
        */tests/*|*.test.*|*.spec.*|*_test.*) return 0 ;;
    esac
    return 1
}

# --- self-test ---------------------------------------------------------
# Runs on every invocation: a scanner whose pattern stopped matching
# passes every file, and only a fixture it must refuse tells that from a
# clean tree. Fixtures live in a mktemp dir this run owns.
self_test() {
    local t hits f
    t="$(mktemp -d)" || { echo "$NAME: cannot make a temp dir for the self-test" >&2; return 1; }
    # shellcheck disable=SC2064
    trap "rm -rf '$t'" RETURN

    # Accepted: pairs built from data, prose in all three comment kinds,
    # a lone `from:` that names one station, and an edge with no ends.
    printf '%s\n' \
        "// The train departs from: 'gates', to: 'track' — prose may say so." \
        '/* once PATHS held { from: "dock", to: "track" } */' \
        'export const sectionKey = (from: string | null, to: string | null) => `${from ?? ""}→${to ?? ""}`;' \
        "const at = { from: r.from, to: r.to };" \
        "const one = { from: 'gates' };" \
        "const none = { from: null, to: null };" \
        "const href = sectionHref(s.from, s.to);" >"$t/good.ts"
    printf '%s\n' \
        '<!-- the dock -> track section, as it was -->' \
        '<path data-section={s.key} />' >"$t/good.svelte"
    for f in good.ts good.svelte; do
        hits="$(findings_in "$t/$f")"
        [ -z "$hits" ] || { echo "$NAME: self-test FAILED — an accepted shape was flagged in $f:" >&2; printf '%s\n' "$hits" >&2; return 1; }
    done

    # Refused: each shape the deleted copies were written in, and the
    # arrow keys the specs used before car R3.
    printf "  { from: 'gates', to: 'garage' },\n"                                   >"$t/bad1.ts"
    printf '  { from: "dock", to: "track", line: "delivery", d: "M540 200 H660" },\n' >"$t/bad2.ts"
    printf "  { from: 'dock', to: null },\n"                                        >"$t/bad3.ts"
    printf "  { from: null, to: 'receiving' },\n"                                   >"$t/bad4.ts"
    printf "  const held = 'dock→track';\n"                                         >"$t/bad5.ts"
    printf '  <a data-key="gates->track">x</a>\n'                                   >"$t/bad6.svelte"
    printf '  {from:`arrivals`,to:`publish`}\n'                                     >"$t/bad7.ts"
    for f in bad1.ts bad2.ts bad3.ts bad4.ts bad5.ts bad6.svelte bad7.ts; do
        [ -n "$(findings_in "$t/$f")" ] || {
            echo "$NAME: self-test FAILED — the scanner passed:" >&2
            sed 's/^/    /' "$t/$f" >&2
            return 1
        }
    done
    exempt "apps/web/tests/fixtures/yard.ts" && exempt "apps/web/src/it/yard/transit.test.ts" \
        && ! exempt "apps/web/src/it/yard/transit.ts" || {
        echo "$NAME: self-test FAILED — the exemption predicate answers wrongly" >&2
        return 1
    }
    echo "$NAME: self-test ok — pairs built from data, comments of all three kinds, a lone end and an edge with no ends pass; an object pair in either quote, an exit, an entry, an arrow key in either spelling and a backtick pair are each refused"
}

if [ "${1:-}" = "--self-test" ]; then self_test; exit $?; fi
self_test || exit 1

# --- the tree ----------------------------------------------------------
files="$(git_answer "$NAME" 0 ls-files -- "$SCOPE/*.ts" "$SCOPE/*.svelte")" \
    || exit "$LINT_CANNOT_ANSWER"

scanned=0
findings=0
while IFS= read -r file; do
    [ -n "$file" ] || continue
    [ -f "$file" ] || continue
    exempt "$file" && continue
    scanned=$((scanned + 1))
    while IFS=$'\t' read -r lineno code; do
        [ -n "${lineno:-}" ] || continue
        findings=$((findings + 1))
        echo "$NAME: $file:$lineno names a station pair: $code" >&2
    done < <(findings_in "$file")
done <<EOF
$files
EOF

if [ "$findings" -gt 0 ]; then
    cat >&2 <<'MSG'

FAIL — the line(s) above write a map edge by hand. The IT map draws
exactly the routes GET /api/yard/routes serves (design e765b3fc §2b),
each derived from one of three sources, and a route appears on the map
by appearing there:

  * a protocol step — a Workflow row whose ready_when moves a packet
    from one region to another (walked by boss_jobs::routes::walk);
  * a declared hand-off — infra/platform/yard/handoffs.toml, one row
    per rule, cadence or verb that turns one packet into another;
  * an observed move — the moves record; one no protocol or hand-off
    declares is drawn dashed red, which is the finding, not a fix.

Build a pair from the data (`sectionKey(route.from, route.to)`), never
from a literal. A test's fixture lives under apps/web/tests/fixtures/.
MSG
    exit 1
fi

lint_scanned "$NAME" "$scanned" "map source file(s) read for a station-pair literal"
echo "$NAME: ok — every edge on the map is one the server serves"
exit 0
