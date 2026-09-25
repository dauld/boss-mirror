#!/usr/bin/env bash
# a-write-to-a-childs-stdin-is-fed.sh — a test feeds a child's piped
# stdin through `boss_testing::feed_stdin`; it never unwraps the write.
#
# WHY THIS EXISTS (backlog d93cc7d5). A test that writes to a child's
# piped stdin with `write!` / `writeln!` / `.write_all(..)` and then
# `.unwrap()`s or `.expect()`s the result races the child: when the child
# exits before it reads — which is exactly what a refusal under test does
# — the write meets a closed pipe, BrokenPipe panics the TEST, and a
# correct verdict reads as a red. It came back three times, each fixed
# where it was found:
#
#   28f29f0b  talos check-declared twin
#   d0eafe94  its dns twin, same unwrap a week later (gate 8a1c65e7, a car
#             that touched no DNS file); wrote feed_stdin as the ONE door
#   fec29a02  tiers_sh.rs red train 11:23 (gate-run 10cdfa86), and five
#             sibling helpers carrying the same shape
#
# and the sweep for this lint found two more that fec29a02 missed
# (the_tunnel_connector_runs_in_the_cluster.rs, example_reference_rows_sql.rs).
# A door nobody is sent to is a comment (CLAUDE.md §9a); this is the
# mechanism that sends them.
#
# THE RULE. Under crates/<tier>/<crate>/tests/, a write whose receiver is a
# child's stdin — the `.stdin` field itself (`child.stdin.take()...`,
# `child.stdin.as_mut()...`), or a name bound from it (`let mut stdin =
# child.stdin.take()...`, `if let Some(s) = child.stdin...`, a parameter
# typed `ChildStdin`) — must not be followed IMMEDIATELY by `.unwrap()` or
# `.expect(..)`. Use `boss_testing::feed_stdin(&mut child, bytes)`: a
# closed pipe is the child's verdict, read from its exit status; any other
# write error still fails the test by name.
#
# WHAT IS NOT THE SHAPE, and each is a judgement a reader can check:
#   * a write whose result is discarded (`let _ =`) or propagated (`?`) —
#     neither panics on a closed pipe;
#   * an unwrap INSIDE the write's arguments (`write_all(&x.unwrap())`);
#   * a writer that is not a child's stdin (a File, a Vec) — a name counts
#     only when this file binds it from `.stdin` or types it `ChildStdin`;
#   * comments and string literals — masked out before anything is
#     matched, so prose telling the story (and this lint's own proof,
#     whose fixtures are strings) is not a hit;
#   * `src/` — production code writing to a child owns its own error
#     handling; this rule is about tests racing a refusal they asked for.
#
# The scanner lexes each file (comments and string contents blanked,
# newlines kept, so `file:line` is the real line and a `;` or `{` inside a
# string cannot split a statement) and then reads statement by statement.
#
# EXIT STATUS (house style, infra/lint/lib/git-answer.sh):
#   0  the tree was read and no test unwraps a write to a child's stdin
#   1  the tree was read and a violation was found — the author's to fix
#   3  the tree was never read — a fact about the MACHINE; never `clean`
#
# The proof is crates/core/boss-testing/tests/a_write_to_a_childs_stdin_is_fed.rs.
#
# USAGE
#   infra/lint/a-write-to-a-childs-stdin-is-fed.sh
set -uo pipefail

NAME="a-write-to-a-childs-stdin-is-fed"
LINT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=infra/lint/lib/scanned.sh
. "$LINT_DIR/lib/scanned.sh" || exit 3
# shellcheck source=infra/lint/lib/git-answer.sh
. "$LINT_DIR/lib/git-answer.sh" || exit 3
cd "$LINT_DIR/../.." || exit 3

git_can_answer "$NAME" || exit "$LINT_CANNOT_ANSWER"
files=$(git_answer "$NAME" 0 ls-files -- ':(glob)crates/*/*/tests/**/*.rs') || exit "$LINT_CANNOT_ANSWER"

set --
while IFS= read -r f; do
    [ -n "$f" ] && set -- "$@" "$f"
done <<EOF
$files
EOF

if [ "$#" -eq 0 ]; then
    lint_scanned "$NAME" 0 "Rust test file(s) under crates/*/*/tests/"
fi

findings=$(python3 - "$@" <<'PY'
import re, sys

def mask(src):
    """Comments and string/char contents blanked, newlines kept."""
    out = list(src)
    n = len(src)

    def blank(a, b):
        for k in range(a, b):
            if out[k] != "\n":
                out[k] = " "

    raw = re.compile(r'b?r(#*)"')
    char = re.compile(r"'(?:\\.[^']*|[^\\'\n])'")
    i = 0
    while i < n:
        if src.startswith("//", i):
            j = src.find("\n", i)
            j = n if j < 0 else j
            blank(i, j); i = j; continue
        if src.startswith("/*", i):
            depth, j = 1, i + 2
            while j < n and depth:
                if src.startswith("/*", j): depth += 1; j += 2
                elif src.startswith("*/", j): depth -= 1; j += 2
                else: j += 1
            blank(i, j); i = j; continue
        m = raw.match(src, i)
        if m and (i == 0 or not (src[i - 1].isalnum() or src[i - 1] == "_")):
            close = '"' + m.group(1)
            end = src.find(close, m.end())
            end = n if end < 0 else end
            blank(m.end(), end); i = end + len(close); continue
        c = src[i]
        if c == '"':
            j = i + 1
            while j < n and src[j] != '"':
                j += 2 if src[j] == "\\" else 1
            blank(i + 1, min(j, n)); i = j + 1; continue
        if c == "'":
            m = char.match(src, i)
            if m:
                blank(i + 1, m.end() - 1); i = m.end(); continue
        i += 1
    return "".join(out)

def close_of(s, open_at):
    depth = 0
    for k in range(open_at, len(s)):
        if s[k] == "(": depth += 1
        elif s[k] == ")":
            depth -= 1
            if depth == 0: return k
    return -1

FIELD = r"\.\s*stdin\b(?!\s*\()"
BINDS = [
    re.compile(r"\blet\s+(?:mut\s+)?([A-Za-z_]\w*)\s*(?::[^=;]*)?=[^;]*?" + FIELD),
    re.compile(r"\bSome\s*\(\s*(?:ref\s+)?(?:mut\s+)?([A-Za-z_]\w*)\s*\)\s*=[^;{]*?" + FIELD),
    re.compile(r"\b([A-Za-z_]\w*)\s*:\s*(?:&\s*(?:mut\s+)?)?(?:std\s*::\s*process\s*::\s*)?ChildStdin\b"),
]
METHOD = re.compile(r"\.\s*write(?:_all|_fmt)?\s*\(")
MACRO = re.compile(r"\bwrite(?:ln)?!\s*\(")
PANICS = re.compile(r"\s*\.\s*(?:unwrap|expect)\s*\(")

status = 0
for path in sys.argv[1:]:
    try:
        src = open(path, encoding="utf-8", errors="replace").read()
    except OSError as e:
        print(f"CANNOT ANSWER — read {path}: {e}", file=sys.stderr)
        sys.exit(3)
    code = mask(src)
    lines = src.split("\n")
    names = {m.group(1) for b in BINDS for m in b.finditer(code)}
    named = re.compile(r"(?<![\w.])(?:" + "|".join(map(re.escape, sorted(names))) + r")\s*$") if names else None
    whole = re.compile(r"^\s*(?:&\s*mut\s+)?(?:" + "|".join(map(re.escape, sorted(names))) + r")\s*$") if names else None
    hits = []
    for m in METHOD.finditer(code):
        close = close_of(code, m.end() - 1)
        if close < 0 or not PANICS.match(code, close + 1):
            continue
        start = max(code.rfind(";", 0, m.start()), code.rfind("{", 0, m.start()), code.rfind("}", 0, m.start())) + 1
        recv = code[start:m.start()]
        if re.search(FIELD, recv) or (named and named.search(recv)):
            hits.append(m.start())
    for m in MACRO.finditer(code):
        close = close_of(code, m.end() - 1)
        if close < 0 or not PANICS.match(code, close + 1):
            continue
        comma = code.find(",", m.end(), close)
        arg = code[m.end():comma] if comma >= 0 else ""
        if re.search(FIELD, arg) or (whole and whole.match(arg)):
            hits.append(m.start())
    for at in sorted(set(hits)):
        line = code.count("\n", 0, at) + 1
        print(f"{path}:{line}: {lines[line - 1].strip()}")
sys.exit(status)
PY
)
status=$?
if [ "$status" -ne 0 ]; then
    echo "$NAME: CANNOT ANSWER — the scanner exited $status, so no verdict on the tree." >&2
    exit "$LINT_CANNOT_ANSWER"
fi

lint_scanned "$NAME" "$#" "Rust test file(s) under crates/*/*/tests/"

if [ -z "$findings" ]; then
    echo "$NAME: clean — no test unwraps a write to a child's stdin"
    exit 0
fi

count=$(printf '%s\n' "$findings" | wc -l | tr -d ' ')
{
    echo "$NAME: $count unwrapped write(s) to a child's piped stdin:"
    echo ""
    printf '%s\n' "$findings" | sed 's/^/  /'
    echo ""
    echo "A child that exits before it reads (a refusal under test) closes the pipe,"
    echo "and the unwrap turns its correct verdict into a BrokenPipe panic — three"
    echo "red trains so far (28f29f0b, d0eafe94, fec29a02). Feed it through the door:"
    echo ""
    echo "    boss_testing::feed_stdin(&mut child, bytes);"
    echo ""
    echo "and assert on the child's exit status, which is the verdict."
} >&2
exit 1
