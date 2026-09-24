#!/usr/bin/env bash
# a-colour-is-a-token.sh — the web app paints with the Enamel tokens, and
# only the token file names a colour.
#
# WHY THIS EXISTS (backlog 7eb59678, 2026-09-24). The Enamel token car
# (e4b58c50, train #605) turned the theme LIGHT where the tokens live —
# `--ink` became white in apps/web/src/styles.css — and David opened live
# pages whose text he could not read. The tokens were right; the
# components were not reading them. Measured that morning: 1,779 colour
# literals in 123 files under apps/web/src and libs/web-kit/src — 750
# bare, 802 as `var(--token, <dark hex>)` fallbacks, 97 as fallbacks to a
# token NOTHING defined (`--chalk`, `--text-muted`, `--danger`, …), so the
# dark theme's literal was what rendered — and 122 text colours below the
# 4.5:1 WCAG floor on the surface they land on, 24 of them below 1.5:1:
# near-white on white. Each literal was a second palette the token edit
# could not reach. The car that added this lint moved every one onto the
# tokens; this lint keeps the next component from bringing one back.
#
# THE RULE. A colour is named ONCE, in a `:root` block of
# apps/web/src/styles.css, and everything else reads it by `var(--name)`.
# Refused anywhere else in apps/web, apps/simulator and libs/web-kit:
#   * a hex colour            #fff  #0E1B2E  #16a34a22
#   * a colour function       rgb( rgba( hsl( hsla(
#   * a named colour after a colour property   color: white
#   * a `var(--x, <literal>)` fallback — a dead one is a second palette
#     waiting for the day its token is renamed, and a live one is how the
#     unreadable text of 2026-09-24 rendered.
# `transparent`, `currentColor` and `inherit` are not colours we choose,
# and `color-mix()` over tokens is a token.
#
# WHAT IS EXEMPT, and each is a judgement a reader can check:
#   * the token file's `:root` blocks (apps/web/src/styles.css) — where a
#     colour belongs. The same file's RULES are not exempt.
#   * tests — `*/tests/*`, `*.test.*`, `*.spec.*`: a test pins a value.
#   * comments — `/* */`, `//`, `<!-- -->`: prose may name what a token
#     replaced, and several do.
#   * a DECLARED paragraph: data that is a colour and is not styling (a
#     categorical palette, a browser's theme-color meta) says so where it
#     sits, and the declaration reaches to the next blank line:
#
#         // colour-literal-ok: <at least three words of reason>
#
#     No list in this file, on purpose: a marker cannot drift from the
#     lines it heads, and fewer than three words is not a reason.
#
# Files read: tracked `*.svelte *.ts *.js *.css *.html` under apps/web/,
# apps/simulator/ and libs/web-kit/. The simulator joined on 2026-09-24
# (backlog 6f471ff6, car 4): it renders the same web-kit parts, yet it
# carried its own stone-and-brew palette — 683 literals in its stylesheet,
# 91 in its components — because nothing here read it. It now takes its
# tokens from the one token file (its stylesheet @imports it), so it
# has no :root of its own and no exemption.
#
# EXIT STATUS (house style, infra/lint/lib/git-answer.sh):
#   0  the tree was read and every colour is a token
#   1  the tree was read and a literal was found — the author's to fix
#   3  the tree was never read — a fact about the MACHINE; never `clean`
#
# USAGE
#   infra/lint/a-colour-is-a-token.sh
#   infra/lint/a-colour-is-a-token.sh --self-test
set -uo pipefail

NAME="a-colour-is-a-token"
TOKEN_FILE="apps/web/src/styles.css"
LINT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=infra/lint/lib/scanned.sh
. "$LINT_DIR/lib/scanned.sh" || exit 3
cd "$LINT_DIR/../.." || exit 1
# shellcheck source=infra/lint/lib/git-answer.sh
. "$LINT_DIR/lib/git-answer.sh" || exit 3

# --- the scanner -------------------------------------------------------
# One file's findings, as `<line>\t<literal>`. Empty output = clean.
# `kind` is css|script — whether `//` opens a comment. `root_exempt` is 1
# for the token file. No `{n}` intervals and no `\s`/`\b`: mawk answers
# the first by matching nothing and reads the others as letters (see
# lib/scanned.sh), so the hex run is walked by hand.
findings_in() { # file kind root_exempt
    awk -v kind="$2" -v root_exempt="$3" -v sq="'" '
        function is_hex(ch) { return index("0123456789abcdefABCDEF", ch) > 0 }
        function is_word(ch) { return ch != "" && index("0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_-", ch) > 0 }
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
                if (kind == "script" && two == "//" && (i == 1 || substr(s, i - 1, 1) != ":")) break
                out = out ch
                i++
            }
            return out
        }
        function has_marker(s,   p, rest, n, a, k, words) {
            p = index(s, "colour-literal-ok:")
            if (p == 0) return 0
            rest = substr(s, p + 18)
            sub(/-->.*/, "", rest); sub(/\*\/.*/, "", rest)
            n = split(rest, a, /[ \t]+/)
            words = 0
            for (k = 1; k <= n; k++) if (a[k] != "") words++
            return (words >= 3)
        }
        function report(lit) { printf "%d\t%s\n", NR, lit }
        function scan(code,   i, ch, j, n, nx, pv, rest, p, fn, lit, q, v, w, k, parts, np) {
            # hex: a # and a run of 3, 4, 6 or 8 hex digits that ends the
            # word (`{#each}` is a hex run of three followed by a letter).
            for (i = 1; i <= length(code); i++) {
                if (substr(code, i, 1) != "#") continue
                pv = (i > 1) ? substr(code, i - 1, 1) : ""
                if (pv == "&") continue
                j = i + 1
                while (j <= length(code) && is_hex(substr(code, j, 1))) j++
                n = j - i - 1
                nx = substr(code, j, 1)
                if ((n == 3 || n == 4 || n == 6 || n == 8) && !is_word(nx)) report(substr(code, i, n + 1))
            }
            # colour functions
            rest = code
            while (match(rest, /(rgba|rgb|hsla|hsl)\(/) > 0) {
                p = RSTART
                pv = (p > 1) ? substr(rest, p - 1, 1) : ""
                fn = substr(rest, p)
                q = index(fn, ")")
                lit = (q > 0) ? substr(fn, 1, q) : fn
                if (!is_word(pv)) report(lit)
                rest = substr(rest, p + RLENGTH)
            }
            # named colours after a colour property
            rest = code
            while (match(rest, /(color|background|background-color|fill|stroke|border|border-color|border-top|border-right|border-bottom|border-left|outline|outline-color)[ \t]*[:=][ \t]*/) > 0) {
                p = RSTART
                pv = (p > 1) ? substr(rest, p - 1, 1) : ""
                v = substr(rest, p + RLENGTH)
                rest = v
                if (is_word(pv)) continue
                q = match(v, /[;"`}{]/)
                if (q > 0) v = substr(v, 1, q - 1)
                gsub(sq, " ", v)
                np = split(v, parts, /[^A-Za-z]+/)
                for (k = 1; k <= np; k++) {
                    w = tolower(parts[k])
                    if (w in NAMED) report(parts[k])
                }
            }
        }
        BEGIN {
            split("white black red green blue gray grey orange yellow purple pink silver gold navy teal maroon crimson tomato", nm, " ")
            for (k in nm) NAMED[nm[k]] = 1
        }
        {
            line = $0
            if (has_marker(line)) marked = 1
            if (line ~ /^[ \t]*$/) marked = 0
            code = strip(line)
            if (root_exempt) {
                if (!in_root && code ~ /:root[ \t]*\{/) { in_root = 1; depth = 0 }
                if (in_root) {
                    t = code; opens = gsub(/\{/, "{", t); t = code; closes = gsub(/\}/, "}", t)
                    depth += opens - closes
                    if (depth <= 0) in_root = 0
                    next
                }
            }
            if (marked) next
            scan(code)
        }
    ' "$1"
}

kind_of() { # path
    case "$1" in
        *.css) echo css ;;
        *) echo script ;;
    esac
}

exempt() { # path
    case "$1" in
        */tests/*|*.test.*|*.spec.*|*_test.*|*/testdata/*) return 0 ;;
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

    # Accepted: the token file's :root, comments of all three kinds,
    # Svelte blocks, an HTML entity, a URL, tokens and color-mix over
    # them, and a declared paragraph.
    printf '%s\n' \
        '/* the dark system was #0D1014 */' \
        ':root {' \
        '  --ink: #FFFFFF; --fog: #0E1B2E;' \
        '  --wash: rgba(14, 27, 46, 0.04);' \
        '}' \
        'body { background: var(--ink); color: var(--fog); }' >"$t/tokens.css"
    hits="$(findings_in "$t/tokens.css" css 1)"
    [ -z "$hits" ] || { echo "$NAME: self-test FAILED — the token file's :root was flagged:" >&2; printf '%s\n' "$hits" >&2; return 1; }
    printf '%s\n' \
        '<script lang="ts">' \
        '  // Pre-#101 this was painted by hand; see https://example.test/#fff' \
        '  const url = "https://example.test/a";' \
        '</script>' \
        '{#each rows as r (r)}{#if r}<b>&#123;</b>{/if}{/each}' \
        '<!-- was #1c1917 -->' \
        '<style>' \
        '  /* replaced' \
        '     #7a838c */' \
        '  .a { color: var(--fog); border: 1px solid transparent; background: color-mix(in srgb, var(--err) 8%, transparent); }' \
        '  .b { white-space: nowrap; fill: currentColor; }' \
        '</style>' \
        '// colour-literal-ok: categorical identity hues are data' \
        "const HUES = ['#7FB4D8', '#C9A96B'];" >"$t/good.svelte"
    hits="$(findings_in "$t/good.svelte" script 0)"
    [ -z "$hits" ] || { echo "$NAME: self-test FAILED — an accepted shape was flagged:" >&2; printf '%s\n' "$hits" >&2; return 1; }

    # Refused: each shape the tree carried on 2026-09-24.
    printf '  .a { color: #d6d3d1; }\n'                        >"$t/bad1.svelte"
    printf '  .b { color: var(--chalk, #f4f7fa); }\n'          >"$t/bad2.svelte"
    printf "  return 'rgba(34, 197, 94, 0.15)';\n"             >"$t/bad3.ts"
    printf '<div style="background: var(--ink); color: white"></div>\n' >"$t/bad4.svelte"
    printf ':root {\n  --fog: #0E1B2E;\n}\n.shell { background: #1c1917; }\n' >"$t/bad5.css"
    printf '// colour-literal-ok\nconst BARE = "#dc2626";\n'  >"$t/bad6.ts"
    printf '// colour-literal-ok: a paragraph ends at a blank\nconst A = 1;\n\nconst B = "#3b82f6";\n' >"$t/bad7.ts"
    printf '  border: 1px solid hsl(210 20%% 50%%);\n'           >"$t/bad8.css"
    for f in bad1.svelte bad2.svelte bad3.ts bad4.svelte bad5.css bad6.ts bad7.ts bad8.css; do
        local root=0
        [ "$f" = bad5.css ] && root=1
        [ -n "$(findings_in "$t/$f" "$(kind_of "$f")" "$root")" ] || {
            echo "$NAME: self-test FAILED — the scanner passed:" >&2
            sed 's/^/    /' "$t/$f" >&2
            return 1
        }
    done
    exempt "apps/web/tests/mocked/x.mocked.spec.ts" && exempt "apps/web/src/enamel-tokens.test.ts" \
        && ! exempt "apps/web/src/it/Page.svelte" || {
        echo "$NAME: self-test FAILED — the exemption predicate answers wrongly" >&2
        return 1
    }
    echo "$NAME: self-test ok — the token file's :root, comments, Svelte blocks, an entity, a URL, color-mix over tokens and a declared paragraph pass; a bare hex, a var() fallback, rgba() in script, a named colour, a literal in the token file's rules, a reasonless marker, a literal past its paragraph and hsl() are each refused"
}

if [ "${1:-}" = "--self-test" ]; then self_test; exit $?; fi
self_test || exit 1

# --- the tree ----------------------------------------------------------
files="$(git_answer "$NAME" 0 ls-files -- \
    'apps/web/*.svelte' 'apps/web/*.ts' 'apps/web/*.js' 'apps/web/*.css' 'apps/web/*.html' \
    'apps/simulator/*.svelte' 'apps/simulator/*.ts' 'apps/simulator/*.js' 'apps/simulator/*.css' 'apps/simulator/*.html' \
    'libs/web-kit/*.svelte' 'libs/web-kit/*.ts' 'libs/web-kit/*.js' 'libs/web-kit/*.css' 'libs/web-kit/*.html')" \
    || exit "$LINT_CANNOT_ANSWER"

scanned=0
findings=0
while IFS= read -r file; do
    [ -n "$file" ] || continue
    [ -f "$file" ] || continue
    exempt "$file" && continue
    scanned=$((scanned + 1))
    root=0
    [ "$file" = "$TOKEN_FILE" ] && root=1
    while IFS=$'\t' read -r lineno lit; do
        [ -n "${lineno:-}" ] || continue
        findings=$((findings + 1))
        echo "$NAME: $file:$lineno carries $lit" >&2
    done < <(findings_in "$file" "$(kind_of "$file")" "$root")
done <<EOF
$files
EOF

if [ "$findings" -gt 0 ]; then
    cat >&2 <<'MSG'

FAIL — the line(s) above name a colour outside the tokens. Every colour
the web app paints is named once, in a :root block of
apps/web/src/styles.css (Enamel, design a4df741a), and read by name:

  * text        var(--fog) primary, var(--static) secondary,
                var(--ok) / var(--warn) / var(--err) for a state's words
  * grounds     var(--ink) a card, var(--ink-raised) a raised row or code,
                var(--ok-wash) / var(--warn-wash) / var(--err-wash) /
                var(--signal-wash) a tinted row
  * plates      var(--band) + var(--on-band), var(--clear) + var(--on-clear),
                var(--busy) + var(--on-busy), var(--troubled) + var(--on-troubled)
  * rules       var(--hairline), var(--border-strong)
  * action      var(--signal)

No fallback — var(--x, #fff) is a second palette. If the token you need
does not exist, add it to :root, where the next re-skin will reach it.
Data that is a colour and not styling (a categorical palette) says so on
the paragraph that holds it:  // colour-literal-ok: <three words why>
MSG
    exit 1
fi

lint_scanned "$NAME" "$scanned" "web file(s) read for a colour literal"
echo "$NAME: ok — every colour is a token"
exit 0
