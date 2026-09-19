#!/usr/bin/env bash
# a-dead-code-allowance-needs-a-reason — no Rust source under crates/
# (outside a tests/ directory) silences dead-code or unused-* lints with
# a bare `#[allow(...)]`, and every `#[expect(dead_code)]` /
# `#[expect(unused...)]` carries a `reason = "..."`.
#
# WHY THIS EXISTS (backlog e758f5bf). The dead-code sweep (car
# fix/dead-code-is-deleted-not-allowed, train #373, 2026-09-15) took 31
# bare `allow(dead_code)` / `allow(unused...)` markers under crates/ to
# zero, and its probe judges the converged tree ONCE. A count driven to
# zero without a floor climbs back one convenience at a time: the marker
# is the same "trust me" the sweep removed, and nothing on the gate
# refused the next one. This is the floor. Alongside, items.rs in
# boss-inventory carried three `#[expect(dead_code)]` with no reason —
# an expect is honest about the field being unread, but with no reason
# the reader still has to re-derive WHY it is kept, which is the belief
# the marker was supposed to replace (CLAUDE.md §Mostly sure vs.
# absolutely sure).
#
# THE TWO WAYS OUT, and the message names both:
#   * delete the item — the sweep's answer for all 31, and the default
#     (CLAUDE.md §What We Don't Do: no "just in case" code);
#   * `#[expect(dead_code, reason = "...")]` — the compiler then refuses
#     the marker the day the code stops being dead, and the reason says
#     what is kept and until when. rustfmt wraps a long one across lines,
#     which this lint reads as one attribute (below).
#
# WHAT IT CHECKS. Every *.rs under crates/ whose path has no `/tests/`
# segment (and is not under target/). Each outer or inner attribute
# (`#[...]` / `#![...]`) is read from its opening `#[` to its balancing
# `]`, across lines, with whole-line `//` comments and trailing `// ...`
# skipped so prose can quote the refused shape. An attribute is refused
# when it carries `allow(` whose lint list names `dead_code` or a rustc
# `unused*` lint, or `expect(` naming one of those with no `reason =`
# anywhere in the attribute. `cfg_attr(..., allow(dead_code))` is the
# same marker behind a cfg and is refused the same way. `clippy::unused_*`
# is a different lint family with a `::` in front of it and is not judged
# here.
#
# THE LIMIT, stated rather than discovered: a `#[cfg(test)] mod tests`
# under src/ is judged like the code around it — a line-based reader does
# not know where a module ends, and the packet excluded tests/
# DIRECTORIES, which it does know. An allow there gets the same two ways
# out. `#![allow(...)]` in a build.rs is under crates/ and judged too.
#
# EXIT STATUS: 0 clean, 1 at least one attribute is named by file:line.
# Reads the working tree with `find`, never git, so nothing here can
# refuse (lib/git-answer.sh's exit 3 is for lints that ask git).
#
# Usage:  infra/lint/a-dead-code-allowance-needs-a-reason.sh

set -uo pipefail

NAME="a-dead-code-allowance-needs-a-reason"
cd "$(dirname "$0")/../.." || exit 1
# shellcheck source=infra/lint/lib/scanned.sh
. infra/lint/lib/scanned.sh || exit 3

# The files under judgement: every .rs under $1 outside tests/ and
# target/. One definition, read by the scan and by the scanned count.
rust_files() { # root
    find "$1" -name '*.rs' -type f -not -path '*/tests/*' -not -path '*/target/*' | LC_ALL=C sort
}

# Every refused attribute under $1, one `file:line:kind` per line where
# kind is `allow` or `expect-without-reason`; line is the attribute's
# FIRST line (the `#[`). Empty output = clean. Judged-file count on
# stderr so a run that read nothing reads as one.
scan() { # root
    local root="$1" f judged=0
    while IFS= read -r f; do
        [ -n "$f" ] || continue
        judged=$((judged + 1))
        LC_ALL=C awk '
            # A lint list names dead_code or a rustc unused* lint when the
            # token stands on its own: preceded by `(`, `,` or space, not
            # by `::` (clippy::unused_async is a different family).
            function names_dead(list) {
                return list ~ /(^|[(, \t])(dead_code|unused[a-z_]*)([),= \t]|$)/
            }
            function judge(text, line) {
                # Trailing `// ...` off each joined line already stripped.
                if (text ~ /allow[ \t]*\(/) {
                    rest = text; sub(/^.*allow[ \t]*\(/, "", rest)
                    if (names_dead("(" rest)) print FILENAME ":" line ":allow"
                }
                if (text ~ /expect[ \t]*\(/) {
                    rest = text; sub(/^.*expect[ \t]*\(/, "", rest)
                    if (names_dead("(" rest) && text !~ /reason[ \t]*=/)
                        print FILENAME ":" line ":expect-without-reason"
                }
            }
            {
                line = $0
                if (line ~ /^[ \t]*\/\//) next
                sub(/[ \t]*\/\/.*$/, "", line)
                if (depth == 0) {
                    if (line !~ /^[ \t]*#!?\[/) next
                    start = FNR; text = ""
                }
                text = text " " line
                # Balance the square brackets of the attribute itself.
                n = length(line)
                for (i = 1; i <= n; i++) {
                    c = substr(line, i, 1)
                    if (c == "[") depth++
                    else if (c == "]") { depth--; if (depth == 0) break }
                }
                if (depth == 0) judge(text, start)
                # An attribute that never balances (a stray `#[` in a
                # string) is abandoned at end of file; nothing is judged.
            }
        ' "$f"
    done <<EOF
$(rust_files "$root")
EOF
    echo "$NAME: $judged Rust file(s) judged" >&2
}

# ---------------------------------------------------------------------------
# Self-test — fixtures in a temp directory this run owns, never under
# crates/ or infra/lint/, where a file is discovered as real.
# ---------------------------------------------------------------------------
tmp="$(mktemp -d)" || exit 1
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/crate/src" "$tmp/crate/tests"

# Every accepted shape in one file: an expect with its reason on one
# line, the rustfmt-wrapped multi-line expect, a clippy allow, a
# clippy::unused_* allow (a different family), prose quoting the refused
# shape as a whole-line comment and as a trailing comment, and an
# allow of an unrelated rustc lint.
cat > "$tmp/crate/src/accepted.rs" <<'RS'
// A comment may say `#[allow(dead_code)]` to tell the story.
#[expect(dead_code, reason = "read by the ledger rebuilder once 4f1c lands")]
struct Kept;
#[expect(
    unused_variables,
    reason = "bound so the destructure names every column; the rebuild reads it"
)]
fn wrapped(x: u32) {}
#[allow(clippy::too_many_arguments)] // not #[allow(dead_code)]
fn wide() {}
#[allow(clippy::unused_async)]
async fn quiet() {}
#[allow(non_snake_case)]
fn Legacy() {}
#[derive(Debug)]
struct Plain;
RS
# A tests/ directory is out of scope by the packet's exclusion.
printf '#[allow(dead_code)]\nstruct Fixture;\n' > "$tmp/crate/tests/fixture.rs"
hits="$(scan "$tmp" 2>/dev/null)"
if [ -n "$hits" ]; then
    echo "$NAME: SELF-TEST FAILED — every accepted shape must pass; got: [$hits]" >&2
    exit 1
fi

# The refused shapes, each on a known line: a bare allow, a bare inner
# allow of an unused lint in a list, a cfg_attr-wrapped allow, an expect
# with no reason on one line, and a wrapped expect with no reason.
cat > "$tmp/crate/src/refused.rs" <<'RS'
#[allow(dead_code)]
struct Bare;
#![allow(clippy::pedantic, unused_imports)]
#[cfg_attr(test, allow(dead_code))]
struct Behind;
#[expect(dead_code)]
struct Trust;
#[expect(
    unused_mut
)]
fn wrapped() {}
RS
hits="$(scan "$tmp" 2>/dev/null)"
want="$tmp/crate/src/refused.rs:1:allow
$tmp/crate/src/refused.rs:3:allow
$tmp/crate/src/refused.rs:4:allow
$tmp/crate/src/refused.rs:6:expect-without-reason
$tmp/crate/src/refused.rs:8:expect-without-reason"
if [ "$hits" != "$want" ]; then
    echo "$NAME: SELF-TEST FAILED — five refused shapes must be named by file:line; got:" >&2
    printf '%s\n' "$hits" >&2
    exit 1
fi
rm -rf "$tmp/crate"

# ---------------------------------------------------------------------------
# The tree.
# ---------------------------------------------------------------------------
[ -d crates ] || { echo "$NAME: crates/ does not exist" >&2; exit 1; }
hits="$(scan crates)"
lint_scanned "$NAME" "$(rust_files crates | wc -l | tr -d ' ')" "Rust file(s) under crates/ outside tests/"
if [ -n "$hits" ]; then
    while IFS=: read -r f line kind; do
        [ -n "$f" ] || continue
        case "$kind" in
            allow) echo "$NAME: $f:$line silences a dead-code/unused lint with a bare allow" >&2 ;;
            *) echo "$NAME: $f:$line expects a dead-code/unused lint with no reason" >&2 ;;
        esac
    done <<EOF
$hits
EOF
    cat >&2 <<'MSG'

  The dead-code sweep took every bare allow(dead_code)/allow(unused...)
  under crates/ to zero (backlog e758f5bf; car
  fix/dead-code-is-deleted-not-allowed), and this is the floor under
  that count. A bare allow is a belief that the code is worth keeping;
  nothing checks it again. Two ways out:

    delete the item — the default; the tree is the record, not a shelf

    #[expect(dead_code, reason = "<what is kept, read by whom, until when>")]
      — the compiler refuses the marker the day the code is used, and
        the reason spares the next reader re-deriving why it is here

  A tests/ directory is out of scope; a #[cfg(test)] module under src/
  is not (a line reader does not know where a module ends).
MSG
    exit 1
fi

echo "$NAME: self-test ok — a reasoned expect (one line and wrapped), clippy allows, prose and a tests/ fixture pass; a bare allow, an inner allow, a cfg_attr allow and two reason-less expects are refused by file:line"
echo "$NAME: ok — no bare allow(dead_code)/allow(unused...) under crates/ outside tests/, and every expect of one carries a reason"
exit 0
