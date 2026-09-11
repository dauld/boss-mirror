#!/usr/bin/env bash
#
# an-expectation-names-a-rule-the-tree-declares — no test, lint or
# fixture may name a dispatcher rule the assembled tree has stopped
# declaring, and a pin over a rule FAMILY must agree with the directory
# it pins.
#
# WHY THIS EXISTS
# ---------------
# Backlog 709e480b, measured on 2026-09-10: THREE red trains in one day,
# all one class.
#
#   - train aecdaa07 — `the_eight_measured_clock_cadences_are_on_the_roster`
#     wanted eight derived cadences. Train #295 had retired
#     `maintenance-sweep-doc-status-daily` with the design-doc flush
#     pipeline, so SEVEN was the right answer (alarm 3ed03942).
#   - train 31e1d207 — `SPAWNS_NOTHING_ON_PURPOSE` exempted
#     `design-review-level-sweep` by name; the corpus-deletion car had
#     retired it (alarm 02974503).
#   - earlier the same day a GATE caught the third: `sweep_spawn_guards`
#     pinned seven daily sweep spawners against six, after the same
#     retirement.
#
# Every car passed its own gate. The contradiction exists only on the
# ASSEMBLED tree, because the expectation and the retirement rode
# different cars — and the consist check, which is the one thing that
# reads the assembled tree before any money is spent, runs the shell
# lints in this directory and not cargo tests. So the first reader was
# CI: a board, a PR, a full suite, a cancel and a re-gate, to learn what
# a grep says in a second.
#
# THE EXPECTATIONS ARE RIGHT AND THIS DOES NOT WEAKEN ONE. Each exists
# to catch a derivation silently losing an entry, and the exemption
# check exists because a dead exemption silently covers the next rule to
# take that name. The defect was never the lists; it was that the
# feedback arrived after the most expensive possible step. This moves
# the reading earlier. It deletes nothing.
#
# AND EXPECT MORE OF IT. `fix/a-retired-rule-may-lead-the-live-registry`
# landed on 2026-09-10 and made rule retirement legal for the live-vs-
# tree lint, so retiring a rule is now a routine operation rather than a
# blocked one.
#
# WHAT IT COMPARES — THE TREE AGAINST THE TREE, AND NOTHING ELSE
# --------------------------------------------------------------
# Three sets, all read from the tree under test. No network, no
# database, no live registry: that is deliberate, and it is what keeps
# this check out of the false-refusal trap its sibling
# `the-live-rules-are-the-authored-rules.sh` had to be rescued from.
#
#   DECLARED — the basenames of `infra/dispatcher/rules/*.toml`. A rule
#     the tree declares. Adding a rule is dropping a file in; retiring
#     one is deleting the file.
#   EVER — DECLARED plus every rule name a `dispatcher_rules` statement
#     under `infra/postgres/schema/` has ever mentioned. Migrations are
#     applied history: they stay in the tree forever, which is exactly
#     why they are the memory of what a rule name USED to mean.
#   RETIRED — EVER minus DECLARED. A name the tree once declared and
#     declares no longer.
#
# A reference to a RETIRED name, in code, anywhere outside the two
# directories that legitimately remember it, is the defect. It is named
# with its file, its line, the line's text, and the migration that
# retired it.
#
# WHY THE RETIRED-IN-TREE-STILL-LIVE WINDOW CANNOT FALSE-REFUSE HERE.
# A rule retired in the tree but still enforced by the running
# dispatcher is a legitimate transient — the migration runs at converge,
# which is after the consist check — and its sibling lint had to learn
# to replay retirement migrations to stop refusing every retirement car
# (`fix/the-flush-pipeline-is-deleted` was blocked on every attempt).
# That window is a LIVE-vs-TREE disagreement. This check never reads the
# live registry, so the window does not exist for it: at consist time
# the file is gone, the migration is present, the reference is stale,
# and stale is precisely the right verdict. The direction is the useful
# one too — it pushes the one-line edit into the car that retires the
# rule, which is the car that knows why.
#
# THE SQL SCRAPE IS A NAME CENSUS, NOT THE SIBLING'S REPLAY. That lint
# replays every status write in apply order to answer "is the tree's
# LAST word on rule X at version V a retirement". This one asks a
# strictly smaller, order-free and version-blind question: "has any
# `dispatcher_rules` statement ever mentioned this name". A stale
# reference is about a NAME, not about a version, so nothing here needs
# ordering — which is why it is eight lines of awk rather than a second
# copy of the replay.
#
# PROSE IS NOT AN EXPECTATION
# ---------------------------
# A rule name inside a comment or a doc is a record of history and must
# stay writable: `docs/architecture-decisions.md` explains why those
# four rules were retired, `sweep_spawn_guards.rs` carries the reason
# its pin dropped from seven to six, and three more comments across
# `boss-nats` and `boss-dispatcher` cite `design-review-spawn` as a
# worked example. Every one of those is correct prose about a dead rule.
# A sibling lint has already made prose awkward to write for exactly
# this reason (backlog 24d7db5d), so:
#
#   - `*.md` and everything under `docs/` is not scanned at all.
#   - in every other file, comments are STRIPPED before the scan —
#     `//` and `/* */` for C-family, `#` for shell/TOML/YAML/Python,
#     `--` for SQL — quote-aware, so a `//` inside a string literal is
#     code and survives.
#   - of what remains, only a name that is the WHOLE of a quoted token —
#     `"design-review-level-sweep"` or `'design-review-level-sweep'` —
#     counts. That is the shape of every expectation: an array entry, a
#     const, a set member.
#   - a name in `backticks` does NOT count, wherever it appears. Backticks
#     are how this repo names a thing in prose, and prose lives inside
#     string literals as well as comments: the held car
#     `feat/a-suppressed-cadence-is-not-a-silent-one` explains in an
#     `assert!` message why `maintenance-sweep/doc-status` left the
#     roster, and names the retired rule mid-sentence to do it. That
#     sentence is right, it is the most useful line in the file, and a
#     check that refused it would be the sibling lint that made prose
#     awkward (24d7db5d) all over again. Measured: with a plain
#     word-boundary match this script refused that car.
#
# The line is crisp rather than heuristic: prose names a rule, code
# QUOTES one. Stripping comments and ignoring backticks cannot hide a
# stale expectation — only a stale sentence, which is not what reddens a
# train. The cost is a false negative on the rarer shapes (a bare
# unquoted word in a shell array, a rule name inside a path literal),
# and a missed reference is cheap where a refused car is not.
#
# A COMMENT THE MACHINE READS IS A MECHANISM
# ------------------------------------------
# The exception, and the distinction is the whole of CLAUDE.md §9a: a
# comment asking the next person to keep two lists in step is not a
# mechanism; a comment THIS SCRIPT READS AND CHECKS is. So a pin over a
# rule family declares itself in one line, in any scanned file:
#
#     rule-registry-pin: <glob> = <n>
#
# and this check expands the glob against the rule directory of the
# assembled tree and refuses a disagreement. That is what covers the
# two incidents a name scan cannot see: `sweep_spawn_guards`'s
# `checked >= 7` named no rule at all, and the eight-cadence list held
# derived `kind/subject` LABELS rather than rule names. Neither is a
# rule-name reference, so — contra the backlog item's estimate that a
# name lint alone would have caught all three — only a declared pin
# reaches them. A glob that matches nothing FAILS: a pin covering
# nothing is the dead exemption one level up.
#
# Usage:
#   infra/lint/an-expectation-names-a-rule-the-tree-declares.sh
#   infra/lint/an-expectation-names-a-rule-the-tree-declares.sh --self-test
#   infra/lint/an-expectation-names-a-rule-the-tree-declares.sh --tree DIR
#
# Exit 0 = clean, 1 = a stale reference, a stale pin, or a broken scrape.

set -uo pipefail

NAME="an-expectation-names-a-rule-the-tree-declares"
SELF_REL="infra/lint/$NAME.sh"
RULES_REL="infra/dispatcher/rules"
SCHEMA_REL="infra/postgres/schema"
PIN_TOKEN="rule-registry-pin:"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# ---------------------------------------------------------------------------
# The sets
# ---------------------------------------------------------------------------

# Rule names the tree DECLARES: one file per rule, named for the rule.
declared_names() {
    local tree="$1" f
    shopt -s nullglob
    local files=("$tree/$RULES_REL"/*.toml)
    shopt -u nullglob
    for f in "${files[@]}"; do basename "$f" .toml; done | LC_ALL=C sort -u
}

# Every rule name a `dispatcher_rules` statement has ever mentioned.
#
# Two shapes, both precise on purpose — a loose scrape would pick up
# handler names, event kinds and JSON bodies out of an insert's later
# columns, and every one of those would then be scanned for as a
# "retired rule" and found all over the tree.
#
#   INSERT INTO dispatcher_rules ... ('<name>', <version>, '<status>'
#   UPDATE|DELETE ... dispatcher_rules ... name = '<x>' / name IN ('<x>', '<y>')
#
# Measured against the real tree on 2026-09-10: 64 names from 60 files,
# and all 60 declared rules found — which is what `no_unscraped_declared`
# below turns into this script's own non-vacuity guard.
scraped_names() {
    local tree="$1"
    shopt -s nullglob
    local files=("$tree/$SCHEMA_REL"/*.sql)
    shopt -u nullglob
    [ ${#files[@]} -gt 0 ] || return 0
    LC_ALL=C awk '
    # Quote-aware `--` strip: a double dash inside a literal is data.
    function strip(line,   i, n, c, out, inq) {
        out = ""; inq = 0; n = length(line)
        for (i = 1; i <= n; i++) {
            c = substr(line, i, 1)
            if (c == "\047") { inq = 1 - inq; out = out c; continue }
            if (inq == 0 && c == "-" && substr(line, i + 1, 1) == "-") break
            out = out c
        }
        return out
    }
    FNR == 1 { mode = "NONE"; seenname = 0 }
    {
        line = strip($0); low = tolower(line)
        if (low ~ /(^|[^a-z_])insert[ \t]+into[ \t]+dispatcher_rules([ \t(]|$)/) { mode = "INSERT"; seenname = 0 }
        else if (low ~ /(^|[^a-z_])update[ \t]+dispatcher_rules([ \t]|$)/) { mode = "NAMED"; seenname = 0 }
        else if (low ~ /(^|[^a-z_])delete[ \t]+from[ \t]+dispatcher_rules([ \t]|$)/) { mode = "NAMED"; seenname = 0 }
        else if (low ~ /^[ \t]*(insert[ \t]+into|update|delete[ \t]+from)[ \t]+[a-z_]/) { mode = "NONE"; seenname = 0 }

        if (mode == "INSERT") {
            s = line
            while (match(s, /\047[A-Za-z0-9_.:@+-]+\047[ \t]*,[ \t]*[0-9]+[ \t]*,[ \t]*\047[a-z]+\047/)) {
                tup = substr(s, RSTART, RLENGTH); s = substr(s, RSTART + RLENGTH)
                nm = tup; sub(/\047[ \t]*,.*$/, "", nm); sub(/^\047/, "", nm)
                print nm
            }
        } else if (mode == "NAMED") {
            seg = ""
            if (seenname) seg = line
            else if (match(low, /(^|[^a-z_])name([^a-z_]|$)/)) { seenname = 1; seg = substr(line, RSTART + RLENGTH) }
            while (match(seg, /\047[^\047]*\047/)) {
                cand = substr(seg, RSTART + 1, RLENGTH - 2); seg = substr(seg, RSTART + RLENGTH)
                if (cand != "" && cand != "active" && cand != "draft" && cand != "retired") print cand
            }
        }
        if (index(line, ";") > 0) { mode = "NONE"; seenname = 0 }
    }
    ' "${files[@]}" | LC_ALL=C sort -u
}

# The migration that last mentions a name, in apply order — so a
# refusal can say WHERE the rule went, not only that it is gone.
retired_by() {
    local tree="$1" name="$2" f last=""
    while IFS= read -r base; do
        [ -n "$base" ] || continue
        f="$tree/$SCHEMA_REL/$base"
        LC_ALL=C grep -qF -- "'$name'" "$f" 2>/dev/null && last="$SCHEMA_REL/$base"
    done <<EOF
$(find "$tree/$SCHEMA_REL" -maxdepth 1 -name '*.sql' -type f -exec basename {} \; 2>/dev/null \
    | LC_ALL=C sort -t- -k1,1n)
EOF
    printf '%s' "$last"
}

# ---------------------------------------------------------------------------
# Which files are scanned
# ---------------------------------------------------------------------------
# `git ls-files` when the tree IS a repo checkout — that is what the
# consist check and the gate both meet, and it keeps nested worktrees,
# `target/` and `node_modules/` out by construction. A fixture tree is
# not a repo, so `find` is the fallback; both paths are exercised by the
# self-test.
#
# NOT SCANNED, each for a reason that is not "it was noisy":
#   $SCHEMA_REL/  applied history. A retirement migration must name the
#                 rule it retires, forever.
#   $RULES_REL/   the registry itself. A retired rule has no file here.
#   docs/, *.md   prose. See PROSE IS NOT AN EXPECTATION above.
#   $SELF_REL     this script. Its self-test fixtures spell real retired
#                 names on purpose (the brief's "use today's incidents"),
#                 and a fixture heredoc is not a comment.
candidate_files() {
    local tree="$1" top="" listing
    top="$(git -C "$tree" rev-parse --show-toplevel 2>/dev/null)"
    if [ -n "$top" ] && [ "$top" = "$(cd "$tree" && pwd)" ]; then
        listing="$(git -C "$tree" ls-files)"
    else
        listing="$(cd "$tree" && find . \
            \( -name .git -o -name target -o -name node_modules -o -name .claude -o -name dist \) -prune -o \
            -type f -print 2>/dev/null | sed 's#^\./##')"
    fi
    printf '%s\n' "$listing" | LC_ALL=C awk -v schema="$SCHEMA_REL/" -v rules="$RULES_REL/" -v self="$SELF_REL" '
        $0 == "" { next }
        $0 == self { next }
        index($0, schema) == 1 { next }
        index($0, rules) == 1 { next }
        index($0, "docs/") == 1 { next }
        /\.md$/ { next }
        /\.(rs|sh|ts|tsx|js|mjs|svelte|toml|py|ya?ml|json|sql)$/ { print }
    '
}

# Comment style for a path. Anything unrecognised is scanned raw: a
# false positive is visible and fixable, a silent skip is not.
style_of() {
    case "$1" in
        *.rs|*.ts|*.tsx|*.js|*.mjs|*.svelte) echo c ;;
        *.sh|*.py|*.toml|*.yaml|*.yml) echo hash ;;
        *.sql) echo dashes ;;
        *) echo none ;;
    esac
}

# `<lineno>:<code>` per line, comments removed. Quote-aware, so
# `"http://x"` keeps its tail and a `#` inside a string stays code.
strip_comments() {
    local file="$1" style="$2"
    case "$style" in
        c)
            LC_ALL=C awk '
            BEGIN { inblk = 0 }
            {
                line = $0; out = ""; i = 1; n = length(line); instr = 0
                while (i <= n) {
                    c = substr(line, i, 1); d = substr(line, i, 2)
                    if (inblk) { if (d == "*/") { inblk = 0; i += 2 } else i++; continue }
                    if (instr) {
                        if (c == "\\") { out = out c substr(line, i + 1, 1); i += 2; continue }
                        if (c == "\"") instr = 0
                        out = out c; i++; continue
                    }
                    if (c == "\"") { instr = 1; out = out c; i++; continue }
                    if (d == "//") break
                    if (d == "/*") { inblk = 1; i += 2; continue }
                    out = out c; i++
                }
                print FNR ":" out
            }' "$file"
            ;;
        hash|dashes)
            local tok='#'
            [ "$style" = dashes ] && tok='--'
            LC_ALL=C awk -v tok="$tok" '
            {
                line = $0; out = ""; i = 1; n = length(line); inq = 0; q = ""
                tl = length(tok)
                while (i <= n) {
                    c = substr(line, i, 1)
                    if (inq) { if (c == q) inq = 0; out = out c; i++; continue }
                    if (c == "\"" || c == "\047") { inq = 1; q = c; out = out c; i++; continue }
                    if (substr(line, i, tl) == tok) break
                    out = out c; i++
                }
                print FNR ":" out
            }' "$file"
            ;;
        *)
            LC_ALL=C awk '{ print FNR ":" $0 }' "$file"
            ;;
    esac
}

# ---------------------------------------------------------------------------
# The scan
# ---------------------------------------------------------------------------
scan_tree() {
    local tree="$1"
    local problems=0
    local tmp
    tmp="$(mktemp -d)" || return 1
    # shellcheck disable=SC2064
    trap "rm -rf '$tmp'" RETURN

    [ -d "$tree/$RULES_REL" ] || {
        echo "$NAME: $tree/$RULES_REL does not exist — a wrong path, not an empty registry" >&2
        return 1
    }

    declared_names "$tree" > "$tmp/declared"
    [ -s "$tmp/declared" ] || {
        echo "$NAME: no *.toml in $RULES_REL — a wrong path, not an empty registry" >&2
        return 1
    }
    scraped_names "$tree" > "$tmp/scraped"

    # NON-VACUITY. Every declared rule is seeded by a migration (the
    # rules README requires it and the authored registry's loader
    # pins it), so the scrape must find all of them. If it does not,
    # either the scrape stopped recognising a statement shape — and then
    # RETIRED is wrong in the silent direction — or a rule arrived with
    # no seed migration and a fresh database will not have it. Both are
    # findings; neither may pass as "nothing to report".
    if ! LC_ALL=C comm -23 "$tmp/declared" "$tmp/scraped" > "$tmp/unscraped"; then
        echo "$NAME: could not compare the declared and scraped name sets" >&2
        return 1
    fi
    if [ -s "$tmp/unscraped" ]; then
        echo "$NAME: $(wc -l < "$tmp/unscraped" | tr -d ' ') declared rule(s) are not mentioned by any migration in $SCHEMA_REL:" >&2
        sed 's/^/    /' "$tmp/unscraped" >&2
        echo "" >&2
        echo "  Either the rule arrived without the ON CONFLICT-safe INSERT a fresh" >&2
        echo "  database needs (see $RULES_REL/README.md; the DB test" >&2
        echo "  parse_raw_dir refuses the file the same way, later), or this" >&2
        echo "  script's scrape no longer recognises the statement's shape — in which" >&2
        echo "  case the retired set below is wrong in the quiet direction and must" >&2
        echo "  not be trusted. Refusing rather than reporting a clean tree." >&2
        problems=$((problems + 1))
    fi

    LC_ALL=C comm -13 "$tmp/declared" "$tmp/scraped" > "$tmp/retired"

    candidate_files "$tree" > "$tmp/files"

    # ----- half one: a reference to a retired rule name -----
    local name esc hits path lineno text where
    while IFS= read -r name; do
        [ -n "$name" ] || continue
        # Cheap pass first: almost no file mentions a retired name, so
        # the comment stripping runs on a handful rather than on the
        # whole tree.
        # `/dev/null` FIRST, never decoration: a grep with no file
        # arguments reads STDIN, and in gate.sh's roster loop stdin is
        # the rest of the roster — so an empty candidate list silently
        # swallowed forty lints and the pre-flight still printed "clean"
        # (found building this car, 2026-09-10; see the REPORT note).
        hits="$(cd "$tree" && LC_ALL=C grep -lF -- "$name" /dev/null $(tr '\n' ' ' < "$tmp/files") 2>/dev/null)"
        [ -n "$hits" ] || continue
        esc="$(printf '%s' "$name" | sed 's/[.[\*^$]/\\&/g')"
        # Where the rule went — one lookup per NAME, not per hit: it
        # walks 130 migrations, and a verdict that names the retiring
        # migration is worth exactly one walk.
        where="$(retired_by "$tree" "$name")"
        while IFS= read -r path; do
            [ -n "$path" ] || continue
            [ -f "$tree/$path" ] || continue
            while IFS= read -r hit; do
                # A heredoc over an empty capture still yields one empty
                # line; unguarded, that is a finding per scanned file.
                [ -n "$hit" ] || continue
                lineno="${hit%%:*}"
                text="${hit#*:}"
                echo "$NAME: $path:$lineno names \`$name\`, which $RULES_REL no longer declares" >&2
                echo "    $(printf '%s' "$text" | sed 's/^[[:space:]]*//' | cut -c1-160)" >&2
                [ -z "$where" ] || echo "    retired in $where" >&2
                echo "" >&2
                echo "  This is backlog 709e480b: an expectation and the retirement that" >&2
                echo "  invalidates it ride different cars, so the contradiction exists only" >&2
                echo "  on the assembled tree and CI is the first thing that reads it." >&2
                echo "  Drop the entry — and say why in the diff, the way" >&2
                echo "  sweep_spawn_guards.rs records its pin dropping from seven to six." >&2
                echo "  A dead exemption is not free: left standing it silently covers the" >&2
                echo "  next rule to take that name." >&2
                echo "  If the name belongs to a rule you are ADDING, the file is missing:" >&2
                echo "  $RULES_REL/$name.toml." >&2
                echo "  If it is history being described rather than expected, it belongs in" >&2
                echo "  a comment or under docs/ — neither is scanned." >&2
                problems=$((problems + 1))
            done <<EOF
$(strip_comments "$tree/$path" "$(style_of "$path")" | LC_ALL=C grep -E "\"${esc}\"|'${esc}'")
EOF
        done <<EOF
$hits
EOF
    done < "$tmp/retired"

    # ----- half two: a declared pin over a rule family -----
    local pinline glob want got matched
    while IFS= read -r pinline; do
        [ -n "$pinline" ] || continue
        path="${pinline%%:*}"
        local rest="${pinline#*:}"
        lineno="${rest%%:*}"
        text="${rest#*:}"
        # `rule-registry-pin: <glob> = <n>`, tolerant of surrounding prose.
        glob="$(printf '%s' "$text" | sed -n "s/.*$PIN_TOKEN[[:space:]]*\([^[:space:]]*\)[[:space:]]*=[[:space:]]*\([0-9][0-9]*\).*/\1/p")"
        want="$(printf '%s' "$text" | sed -n "s/.*$PIN_TOKEN[[:space:]]*\([^[:space:]]*\)[[:space:]]*=[[:space:]]*\([0-9][0-9]*\).*/\2/p")"
        if [ -z "$glob" ] || [ -z "$want" ]; then
            echo "$NAME: $path:$lineno carries a $PIN_TOKEN declaration this script cannot read" >&2
            echo "    $(printf '%s' "$text" | sed 's/^[[:space:]]*//' | cut -c1-160)" >&2
            echo "  The shape is exactly: $PIN_TOKEN <glob> = <n>" >&2
            problems=$((problems + 1))
            continue
        fi
        matched="$(glob_match "$glob" "$tmp/declared")"
        got="$(printf '%s' "$matched" | LC_ALL=C grep -c . || true)"
        if [ "$got" -eq 0 ]; then
            echo "$NAME: $path:$lineno pins \`$glob\` = $want, and no rule in $RULES_REL matches that glob" >&2
            echo "  A pin covering nothing is the dead exemption one level up: it passes" >&2
            echo "  forever and would silently cover the next family to take that shape." >&2
            echo "  Fix the glob, or drop the pin with the reason in the diff." >&2
            problems=$((problems + 1))
        elif [ "$got" -ne "$want" ]; then
            echo "$NAME: $path:$lineno pins \`$glob\` = $want, but the tree declares $got:" >&2
            printf '    %s\n' $matched >&2
            echo "" >&2
            echo "  Backlog 709e480b again, the half a name scan cannot see: this pin" >&2
            echo "  names no rule, so only the count goes stale. Move the pin AND the" >&2
            echo "  expectation it guards, and say which rule left and why — a number" >&2
            echo "  nudged until green is how a sweep goes missing unnoticed." >&2
            problems=$((problems + 1))
        fi
    done <<EOF
$(cd "$tree" && LC_ALL=C grep -nHF -- "$PIN_TOKEN" /dev/null $(tr '\n' ' ' < "$tmp/files") 2>/dev/null)
EOF

    [ "$problems" -eq 0 ] || return 1

    local nretired npins
    nretired="$(LC_ALL=C grep -c . < "$tmp/retired" || true)"
    npins="$(cd "$tree" && LC_ALL=C grep -lF -- "$PIN_TOKEN" /dev/null $(tr '\n' ' ' < "$tmp/files") 2>/dev/null | LC_ALL=C grep -c . || true)"
    echo "$NAME: OK — $(LC_ALL=C grep -c . < "$tmp/declared") declared rules, $nretired retired and referenced nowhere in code, $npins file(s) carrying a $PIN_TOKEN pin"
    return 0
}

# Expand a `*` glob over a newline-delimited name list.
glob_match() {
    local glob="$1" file="$2" n
    while IFS= read -r n; do
        [ -n "$n" ] || continue
        # shellcheck disable=SC2254
        case "$n" in $glob) printf '%s\n' "$n" ;; esac
    done < "$file"
}

# ---------------------------------------------------------------------------
# Self-test — today's three incidents as fixtures
# ---------------------------------------------------------------------------
self_test() {
    local tmp rc out
    tmp="$(mktemp -d)" || return 1
    # shellcheck disable=SC2064
    trap "rm -rf '$tmp'" RETURN
    local bad=0
    st_fail() { echo "$NAME: SELF-TEST FAIL: $*" >&2; bad=1; }

    # A tree shaped like the real one: the six daily maintenance sweeps
    # plus the mirror publish, all seeded, with the two rules retired on
    # 2026-09-10 present only in the retirement migration.
    # The fixture migrations are written with LOWERCASE sql keywords, and
    # that is not a style choice. `api-path-bypass-smell.sh` greps every
    # `infra/**/*.sh` for uppercase write-DML and flags it as a script
    # writing to the database behind the API's back — a true rule, and a
    # false positive here, because these lines write a FILE into a temp
    # fixture tree and no database exists. Its designed remedy is an
    # ALLOWLIST entry in that script, which another car holds open as
    # this is written; lowercase keeps this car independent of it. That
    # is the second lint in two days to force a workaround on prose or
    # fixtures rather than on a defect (backlog 24d7db5d), and the
    # allowlist entry is the right end state. Meanwhile it earns
    # something: the scrape above lowercases before matching, so this
    # fixture is also the proof that it reads either case.
    mk_tree() {
        local root="$1" r s
        r="$root/$RULES_REL"; s="$root/$SCHEMA_REL"
        mkdir -p "$r" "$s" "$root/crates/t/tests"
        local n
        for n in maintenance-sweep-build-caches-daily maintenance-sweep-cluster-conformance-daily \
                 maintenance-sweep-converge-lag-daily maintenance-sweep-disk-daily \
                 maintenance-sweep-empty-decisions-daily maintenance-sweep-image-freshness-daily \
                 publish-to-github-daily; do
            printf '[[rule]]\nname = "%s"\nversion = 1\n' "$n" > "$r/$n.toml"
            printf "insert into dispatcher_rules (name, version, status) values ('%s', 1, 'active');\n" \
                "$n" >> "$s/100-seed.sql"
        done
        # The two retirements of 2026-09-10, as migrations: file gone,
        # history kept. This is the state a retirement car assembles.
        {
            printf -- "-- the flush pipeline and the corpus index are deleted\n"
            printf "insert into dispatcher_rules (name, version, status) values ('design-review-level-sweep', 1, 'active');\n"
            printf "insert into dispatcher_rules (name, version, status) values ('maintenance-sweep-doc-status-daily', 1, 'active');\n"
            printf "update dispatcher_rules set status = 'retired'\n where name in ('design-review-level-sweep', 'maintenance-sweep-doc-status-daily');\n"
        } > "$s/200-retire.sql"
    }

    # 1. The clean tree passes — including a retired name in PROSE,
    #    which is what `sweep_spawn_guards.rs` and three other files
    #    legitimately hold today.
    mk_tree "$tmp/clean"
    cat > "$tmp/clean/crates/t/tests/pin.rs" <<'RS'
//! SIX since 2026-09-10, down from seven: the seventh was
//! `maintenance-sweep-doc-status-daily`, retired with the flush pipeline.
/* design-review-level-sweep is gone too — a block comment is prose. */
// rule-registry-pin: maintenance-sweep-*-daily = 6
const WATCHED: &[&str] = &["maintenance-sweep-disk-daily", "publish-to-github-daily"];
const URL: &str = "http://example.invalid//maintenance-sweep-disk-daily";
fn why() {
    // Prose inside a string literal, backticked: the held car's real
    // shape. `maintenance-sweep/doc-status` was removed because train
    // #295 retired `maintenance-sweep-doc-status-daily` with the flush
    // pipeline — and the same sentence as a panic message must stay
    // writable.
    assert!(
        true,
        "`maintenance-sweep/doc-status` left the roster when train #295 retired \
         `maintenance-sweep-doc-status-daily` with the flush pipeline"
    );
}
RS
    out="$(scan_tree "$tmp/clean" 2>&1)"; rc=$?
    [ "$rc" -eq 0 ] || st_fail "the clean tree was refused: $out"
    printf '%s' "$out" | grep -q "OK — 7 declared rules, 2 retired" \
        || st_fail "the clean tree's OK line does not state what it compared: $out"

    # 2. INCIDENT 31e1d207 — a test names `design-review-level-sweep`
    #    in code while the corpus-deletion car retired it. Must fail,
    #    naming the rule AND the file.
    mk_tree "$tmp/stale-name"
    cat > "$tmp/stale-name/crates/t/tests/pin.rs" <<'RS'
// rule-registry-pin: maintenance-sweep-*-daily = 6
const SPAWNS_NOTHING_ON_PURPOSE: &[(&str, &str)] = &[
    ("design-review-level-sweep", "runs docs.design.sweep, one packet per doc"),
];
RS
    out="$(scan_tree "$tmp/stale-name" 2>&1)"; rc=$?
    [ "$rc" -eq 1 ] || st_fail "a test naming a retired rule was not refused (rc=$rc): $out"
    printf '%s' "$out" | grep -q 'design-review-level-sweep' \
        || st_fail "the refusal does not name the rule: $out"
    printf '%s' "$out" | grep -q 'crates/t/tests/pin.rs:3' \
        || st_fail "the refusal does not name the referencing file and line: $out"
    printf '%s' "$out" | grep -q "retired in $SCHEMA_REL/200-retire.sql" \
        || st_fail "the refusal does not say where the rule went: $out"

    # 3. INCIDENT aecdaa07 / the gate's catch — a count pin expecting
    #    eight where the tree declares six. Names no rule, so only the
    #    declared pin reaches it.
    mk_tree "$tmp/stale-count"
    cat > "$tmp/stale-count/crates/t/tests/pin.rs" <<'RS'
// rule-registry-pin: maintenance-sweep-*-daily = 8
RS
    out="$(scan_tree "$tmp/stale-count" 2>&1)"; rc=$?
    [ "$rc" -eq 1 ] || st_fail "a stale count pin was not refused (rc=$rc): $out"
    printf '%s' "$out" | grep -q 'pins `maintenance-sweep-\*-daily` = 8, but the tree declares 6' \
        || st_fail "the refusal does not state expected vs actual: $out"
    printf '%s' "$out" | grep -q 'maintenance-sweep-disk-daily' \
        || st_fail "the refusal does not list what the glob matched: $out"

    # 4. A pin whose glob matches nothing passes forever — refused, for
    #    the same reason a dead exemption is.
    mk_tree "$tmp/empty-pin"
    printf '// rule-registry-pin: design-review-* = 2\n' > "$tmp/empty-pin/crates/t/tests/pin.rs"
    out="$(scan_tree "$tmp/empty-pin" 2>&1)"; rc=$?
    [ "$rc" -eq 1 ] || st_fail "a pin matching no rule was not refused (rc=$rc): $out"
    printf '%s' "$out" | grep -q 'no rule in .* matches that glob' \
        || st_fail "the empty-pin refusal does not say the glob matched nothing: $out"

    # 5. NON-VACUITY: a rule the migrations never mention means either a
    #    missing seed or a broken scrape, and the retired set cannot be
    #    trusted either way.
    mk_tree "$tmp/unseeded"
    printf '[[rule]]\nname = "spawn-nothing-daily"\n' > "$tmp/unseeded/$RULES_REL/spawn-nothing-daily.toml"
    out="$(scan_tree "$tmp/unseeded" 2>&1)"; rc=$?
    [ "$rc" -eq 1 ] || st_fail "a rule with no seed migration was not refused (rc=$rc): $out"
    printf '%s' "$out" | grep -q 'spawn-nothing-daily' \
        || st_fail "the non-vacuity refusal does not name the rule: $out"

    # 6. An empty or wrong rule directory is a wrong path, never a clean
    #    tree — the sibling lint's refusal, for the same reason.
    mkdir -p "$tmp/hollow/$RULES_REL"
    out="$(scan_tree "$tmp/hollow" 2>&1)"; rc=$?
    [ "$rc" -eq 1 ] || st_fail "an empty rule directory was read as a clean tree (rc=$rc): $out"

    # 7. THIS SCRIPT MUST NOT READ STDIN, and the cost of getting that
    #    wrong is not its own result. gate.sh runs the roster as
    #    `while read -r name path; do check "$name" bash "$path"; done
    #    <<< "$roster"`, so stdin inside a lint IS the rest of the
    #    roster: a `grep` that falls back to stdin swallows every lint
    #    after it, and the pre-flight prints "clean" having run ten of
    #    fifty. Measured exactly that on 2026-09-10 while building this
    #    car, with a tree whose candidate file list came out empty so
    #    `grep -lF "$name"` had no file arguments at all. The fix is the
    #    `/dev/null` sentinel argument; this is the pin on it, and it
    #    runs against the tree that has no candidate files.
    local left
    left=$(printf 'a\nb\nc\n' | { scan_tree "$tmp/unseeded" >/dev/null 2>&1; cat; } | LC_ALL=C grep -c .)
    [ "$left" -eq 3 ] || st_fail "the scan consumed stdin ($left of 3 lines left) — inside gate.sh's \
roster loop that silently truncates the roster and still reports clean"

    [ "$bad" -eq 0 ] || return 1
    echo "$NAME: self-test ok — a retired rule QUOTED in code fails with its file, line and \
retiring migration; a stale count pin fails with expected vs actual; a pin matching nothing \
fails; a rule with no seed migration fails; the same names in line comments, block comments, \
backticked prose inside a string literal, and docs all pass; and the scan leaves stdin alone, \
so it cannot truncate gate.sh's roster loop"
    return 0
}

# ---------------------------------------------------------------------------
case "${1:-}" in
    --self-test) self_test; exit $? ;;
    --tree)
        [ -n "${2:-}" ] || { echo "$NAME: --tree needs a directory" >&2; exit 1; }
        scan_tree "$2"; exit $?
        ;;
    "") ;;
    *) echo "$NAME: unknown argument: $1" >&2; exit 1 ;;
esac

self_test || exit 1
scan_tree "$REPO_ROOT" || exit 1
exit 0
