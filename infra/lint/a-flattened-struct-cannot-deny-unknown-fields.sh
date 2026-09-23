#!/usr/bin/env bash
# a-flattened-struct-cannot-deny-unknown-fields — no struct under crates/
# declares `#[serde(deny_unknown_fields)]` while some field anywhere under
# crates/ names it as the type of a `#[serde(flatten)]`.
#
# WHY THIS EXISTS (backlog b36a99e5). serde hands a flattened struct only
# the keys it names, so a `deny_unknown_fields` on the TARGET of a flatten
# refuses nothing: the attribute is right there on the struct, reads as
# enforcement to every future reader, and holds nothing. MEASURED LIVE,
# not reasoned (backlog a2358e7c F3, 2026-09-19): the dispatcher's draft
# POST body flattened `RawRule`, and a body carrying a misspelled trigger
# key parsed clean through it — it would have landed a draft rule with no
# trigger at all, from the API, while the same typo in a rule FILE was
# about to be refused. That instance is repaired, and the repair is the
# way out this lint names: lift the extra key off the object by hand and
# deserialize the rest as the closed struct (`split_draft_body` in
# crates/core/boss-dispatcher/src/http.rs).
#
# PREVENTION, NOT REPAIR, and stated so it is read that way: when this
# landed, no struct under crates/ was both closed and flattened — the
# first run of this lint on the tree is that measurement, per struct
# (the packet's own count was file-level: 17 flatten files, 0 sharing a
# file with the attribute). The trap is invisible at the point of use,
# so the next author to close one of those flatten targets would believe
# it took; this is where they are told otherwise.
#
# WHAT IT CHECKS. Every *.rs under crates/ (tests/ included — a closed
# fixture struct is the same belief), outside target/. Attributes are read
# from `#[` to the balancing `]` across lines, so rustfmt's wrapped
# `#[serde(\n  rename_all = "...",\n  deny_unknown_fields\n)]` is one
# attribute; quoted strings and `//` comments are removed first, so prose
# naming both words (the dispatcher's own doc comment does) is not a
# finding. A `serde(...)` attribute (bare or inside `cfg_attr`) carrying
# the whole word `deny_unknown_fields`, followed — past further
# attributes, doc comments and blank lines — by a `struct <Name>`, closes
# <Name>. One carrying the whole word `flatten`, followed the same way by
# a `field: Type` line, flattens every identifier in Type — so
# `Option<crate::rules::RawRule<T>>` flattens `RawRule`. A closed name
# that is also a flattened name is refused, naming the struct's file:line
# AND every flatten site's file:line.
#
# THE LIMITS, stated rather than discovered:
#   * names are matched tree-wide, not resolved by path: two structs of
#     one name in different crates, one closed and the other flattened,
#     read as the trap. The way out is the same as for the real thing.
#   * an ENUM carrying the attribute is not judged: a flattened enum is
#     handed the whole remaining map, so the claim is not the same one.
#   * the attribute on the OUTER struct of a flatten is serde-documented
#     as unsupported too, but it is not the measured trap; not judged.
#
# EXIT STATUS: 0 clean, 1 at least one closed struct is flattened (or the
# self-test failed, or nothing was scanned). Reads the working tree with
# `find`, never git.
#
# Usage:  infra/lint/a-flattened-struct-cannot-deny-unknown-fields.sh [--self-test]

set -uo pipefail

NAME="a-flattened-struct-cannot-deny-unknown-fields"
cd "$(dirname "$0")/../.." || exit 1
# shellcheck source=infra/lint/lib/scanned.sh
. infra/lint/lib/scanned.sh || exit 3

# The files under judgement: every .rs under $1 outside target/. One
# definition, read by the scan and by the scanned count.
rust_files() { # root
    find "$1" -name '*.rs' -type f -not -path '*/target/*' -print0
}

# Every finding under $1, one `<struct file:line>\t<Name>\t<flatten
# file:line>` per line, sorted. Empty output = clean.
scan() { # root
    rust_files "$1" | xargs -0 -r env LC_ALL=C awk '
        # A whole word: not glued to an identifier character on either
        # side. Built as a string because mawk has no \b.
        function has_word(text, w) {
            return text ~ ("(^|[^A-Za-z0-9_])" w "([^A-Za-z0-9_]|$)")
        }
        function judge_attr(text, line) {
            if (text !~ /serde[ \t]*\(/) return
            if (has_word(text, "deny_unknown_fields")) pend_deny = 1
            if (has_word(text, "flatten")) { pend_flat = 1; flat_line = line }
        }
        FNR == 1 { depth = 0; pend_deny = 0; pend_flat = 0 }
        {
            line = $0
            # Strings first (a "//" inside one is not a comment), then
            # comments. Doc comments keep whatever is pending: they sit
            # between an attribute and its item.
            gsub(/"([^"\\]|\\.)*"/, "\"\"", line)
            if (depth == 0 && line ~ /^[ \t]*\/\//) next
            sub(/\/\/.*$/, "", line)
            if (depth == 0 && line ~ /^[ \t]*#!?\[/) { start = FNR; text = "" }
            if (depth > 0 || line ~ /^[ \t]*#!?\[/) {
                text = text " " line
                n = length(line)
                for (i = 1; i <= n; i++) {
                    c = substr(line, i, 1)
                    if (c == "[") depth++
                    else if (c == "]") { depth--; if (depth == 0) break }
                }
                if (depth == 0) judge_attr(text, start)
                next
            }
            if (line ~ /^[ \t]*$/) next
            item = line
            sub(/^[ \t]+/, "", item)
            sub(/^pub[ \t]*\([^)]*\)[ \t]*/, "", item)
            sub(/^pub[ \t]+/, "", item)
            if (pend_deny && item ~ /^struct[ \t]/) {
                name = item
                sub(/^struct[ \t]+/, "", name)
                if (match(name, /^[A-Za-z_][A-Za-z0-9_]*/))
                    print "D\t" substr(name, 1, RLENGTH) "\t" FILENAME ":" FNR
            }
            if (pend_flat && item ~ /^[A-Za-z_][A-Za-z0-9_]*[ \t]*:[^:]/) {
                ty = item
                sub(/^[A-Za-z_][A-Za-z0-9_]*[ \t]*:/, "", ty)
                gsub(/[^A-Za-z0-9_]+/, " ", ty)
                k = split(ty, toks, " ")
                for (j = 1; j <= k; j++)
                    if (toks[j] != "") print "F\t" toks[j] "\t" FILENAME ":" flat_line
            }
            pend_deny = 0; pend_flat = 0
        }
    ' | LC_ALL=C awk -F'\t' '
        # Test membership BEFORE the assignment names the element: awk
        # creates closed[$2] as the lvalue is evaluated, so a one-line
        # `closed[$2] = ($2 in closed) ? ...` always takes the true arm.
        $1 == "D" { had = ($2 in closed); closed[$2] = had ? closed[$2] "\n" $3 : $3; next }
        $1 == "F" { nf++; fname[nf] = $2; fsite[nf] = $3 }
        END {
            for (i = 1; i <= nf; i++) {
                if (!(fname[i] in closed)) continue
                m = split(closed[fname[i]], locs, "\n")
                for (j = 1; j <= m; j++) print locs[j] "\t" fname[i] "\t" fsite[i]
            }
        }
    ' | LC_ALL=C sort -u
}

# ---------------------------------------------------------------------------
# Self-test — fixtures in a temp directory this run owns, never under
# crates/, where a file is discovered as real. Runs on every invocation:
# mawk (the gate image's awk) matches NOTHING for `\s` or an interval, and
# a scanner that matches nothing passes every tree.
# ---------------------------------------------------------------------------
self_test() {
    local tmp hits want
    tmp="$(mktemp -d)" || { echo "$NAME: cannot make a temp dir for the self-test" >&2; return 1; }
    mkdir -p "$tmp/a/src" "$tmp/b/tests"

    # Accepted: a closed struct nobody flattens; a flatten of an open
    # struct; prose naming both words, as a doc comment and a trailing
    # comment; the words inside a string; a closed ENUM that is flattened
    # (not this claim — see THE LIMITS); a closed struct whose name is
    # only a PREFIX of a flattened one.
    cat > "$tmp/a/src/accepted.rs" <<'RS'
/// NOT `#[serde(flatten)]`: `#[serde(deny_unknown_fields)]` is inert there.
#[derive(Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Closed {
    pub name: String,
}
#[derive(Deserialize)]
pub struct Open {
    pub id: String,
}
#[derive(Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum Shape { A { x: u32 } }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rule { x: u32 }
#[derive(Deserialize)]
struct Outer {
    #[serde(flatten)]
    open: Open, // not #[serde(flatten)] of Closed
    #[serde(flatten)]
    shape: Shape,
    #[serde(flatten)]
    rules: RuleSet,
    #[serde(rename = "flatten deny_unknown_fields")]
    label: Closed,
}
RS
    hits="$(scan "$tmp")"
    if [ -n "$hits" ]; then
        echo "$NAME: self-test FAILED — every accepted shape must pass; got:" >&2
        printf '%s\n' "$hits" >&2
        rm -rf "$tmp"; return 1
    fi

    # Refused, each on a known line: a one-line closed struct flattened
    # through Option and a path; rustfmt's wrapped attribute with a doc
    # comment before a generic pub(crate) struct, flattened with
    # `default` in the same attribute from another file; a cfg_attr form.
    cat > "$tmp/a/src/refused.rs" <<'RS'
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct One {
    x: u32,
}
#[derive(Deserialize)]
#[serde(
    rename_all = "snake_case",
    deny_unknown_fields
)]
/// wrapped
pub(crate) struct Two<T> {
    x: T,
}
#[cfg_attr(feature = "strict", serde(deny_unknown_fields))]
pub struct Three;
RS
    cat > "$tmp/b/tests/sites.rs" <<'RS'
struct Holder {
    #[serde(flatten)]
    one: Option<crate::One>,
    /// doc
    #[serde(default, flatten)]
    pub two: Two<String>,
    #[serde(flatten)]
    pub(crate) three: Three,
}
RS
    hits="$(scan "$tmp")"
    want="$(printf '%s\t%s\t%s\n' \
        "$tmp/a/src/refused.rs:12" Two "$tmp/b/tests/sites.rs:5" \
        "$tmp/a/src/refused.rs:16" Three "$tmp/b/tests/sites.rs:7" \
        "$tmp/a/src/refused.rs:3" One "$tmp/b/tests/sites.rs:2" | LC_ALL=C sort -u)"
    if [ "$hits" != "$want" ]; then
        echo "$NAME: self-test FAILED — three refused shapes must be named by struct and flatten site; got:" >&2
        printf '%s\n' "$hits" >&2
        rm -rf "$tmp"; return 1
    fi
    rm -rf "$tmp"
    echo "$NAME: self-test ok — an unflattened closed struct, a flattened open struct, a flattened closed enum, a name-prefix, prose and a string pass; a one-line, a wrapped generic and a cfg_attr closed struct, each flattened, are named by struct and flatten site"
}

self_test || exit 1
if [ "${1:-}" = "--self-test" ]; then exit 0; fi

# ---------------------------------------------------------------------------
# The tree.
# ---------------------------------------------------------------------------
[ -d crates ] || { echo "$NAME: crates/ does not exist" >&2; exit 1; }
hits="$(scan crates)"
lint_scanned "$NAME" "$(rust_files crates | tr -cd '\0' | wc -c | tr -d ' ')" "Rust file(s) under crates/"
if [ -n "$hits" ]; then
    while IFS="$(printf '\t')" read -r struct name site; do
        [ -n "$struct" ] || continue
        echo "$NAME: $struct: struct $name denies unknown fields, and $site flattens it — the attribute refuses nothing there" >&2
    done <<EOF
$hits
EOF
    cat >&2 <<'MSG'

  serde hands a #[serde(flatten)]ed struct only the keys it names, so
  its deny_unknown_fields never sees an unknown key: a typo parses
  clean, and the attribute on the struct says it cannot (backlog
  b36a99e5; measured live on the dispatcher's draft body, a2358e7c F3).
  Two ways out:

    drop the flatten — lift the extra key(s) off the JSON object by
      hand and deserialize the rest as the closed struct; the worked
      example is split_draft_body in crates/core/boss-dispatcher/src/http.rs

    drop the attribute — if the struct is meant to be open, say so by
      not claiming otherwise
MSG
    exit 1
fi

echo "$NAME: ok — no struct that denies unknown fields is the target of a serde flatten"
exit 0
