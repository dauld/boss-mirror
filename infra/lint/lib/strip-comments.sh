# strip-comments.sh — comments out of a TS/Svelte source before a
# scanner reads it, KEEPING every line where it was.
#
# WHY. every-spa-api-path-is-routed grepped the SPA's sources for API
# paths without stripping comments, failed a car on `/api/agent-runs`
# appearing in a DOCSTRING that explained why that path is unreachable,
# and named the comment line as the "fetch site" (a1752a75). The builder
# then wrote the paths WITHOUT their prefix plus a paragraph explaining
# why — worse prose, to keep a check quiet. The sibling source pins in
# TypeScript (estate.test.ts, TriageBoard.test.ts) strip comments before
# scanning; this is the same three rules for the lints, once:
#
#   1. `<!-- … -->`   Svelte/HTML block comments
#   2. `/* … */`      block comments, across lines
#   3. `// …`         to end of line — unless the `//` follows a `:`,
#                     which is a URL (`http://…`), not a comment
#
# Line count is PRESERVED: a stripped block leaves its newlines behind,
# so a scanner that reports `file:line` still points at the real line.
# The same car recorded the other direction of this hazard — `/api/*`
# written in prose opened what a stripper read as a block comment and
# swallowed 250 lines — which is why rule 2 is non-greedy and why the
# self-test in every-spa-api-path-is-routed plants exactly that.
#
# Usage: strip_comments <file>...   (each file's stripped source on
# stdout, in order — ONE interpreter for all of them, because one per
# file made a lint that read 800 sources take eight seconds on the
# eleven-second pre-push door)

strip_comments() {
    python3 - "$@" <<'PY'
import re, sys
keep_lines = lambda m: "\n" * m.group(0).count("\n")
for path in sys.argv[1:]:
    src = open(path, encoding="utf-8", errors="replace").read()
    src = re.sub(r"<!--[\s\S]*?-->", keep_lines, src)
    src = re.sub(r"/\*[\s\S]*?\*/", keep_lines, src)
    src = re.sub(r"(^|[^:])//[^\n]*", r"\1", src, flags=re.M)
    sys.stdout.write(src)
    if not src.endswith("\n"):
        sys.stdout.write("\n")
PY
}
