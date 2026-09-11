#!/usr/bin/env bash
#
# dispatcher-rules-ratchet — the justification guard on reactive rules
# (protocol-policy-publish.md, the rule census).
#
# THE PRINCIPLE
# -------------
# Under the 3P admission edge, a dispatcher rule is a reaction the
# protocol definition could not express. The census classed the 38
# rules of 2026-08-12: seven jobs-internal consequences that move
# into WorkflowSpec `on` blocks whole, ~22 domain effects that become
# admission-staged obligations, and nine external-glue reactions that
# stay. Every migration deletes its rule; nothing should quietly add
# one.
#
# THE CHECKED PROPERTY
# --------------------
# Every rule in infra/dispatcher/rules/ says WHY it exists: one file
# per rule, holding exactly one `[[rule]]` named for the file, with a
# non-empty `why`. The count is REPORTED, derived from the directory.
#
# This used to be a shrink-only ceiling — `count <= BASELINE`, with
# BASELINE a hand-typed integer raised in the same diff as the rule,
# and a sentence here saying why the reaction could not be a protocol
# consequence. The ceiling was never the point; the sentence was. And
# the ceiling was a contended tail line: on 2026-09-08 two rule cars
# could not ride one train because both appended to rules.toml and both
# bumped the same integer here, and the second needed a re-rail
# (backlog 07e72962). So the sentence moved into the rule's own file,
# where it is per-rule instead of per-bump, covers all sixty rules
# instead of the twenty-two that arrived after this script was written,
# and cannot be edited by two cars at once. The count fell out of the
# directory at the same time, because a number derived from the files
# cannot drift from them (CLAUDE.md §9a: collapse it if you can).
#
# The loader enforces the same property at the door
# (`registry::parse_raw_dir` refuses a rule file with no `why`, and the
# unit test `a_rule_file_must_say_why_the_rule_exists` pins it). This
# script is the ~1s front door that names the offending file without a
# compile.
#
# Usage:  infra/lint/dispatcher-rules-ratchet.sh

set -euo pipefail

RULES_DIR="infra/dispatcher/rules"

if [ ! -d "$RULES_DIR" ]; then
    echo "dispatcher-rules-ratchet: $RULES_DIR does not exist" >&2
    exit 1
fi

shopt -s nullglob
files=("$RULES_DIR"/*.toml)
shopt -u nullglob

if [ ${#files[@]} -eq 0 ]; then
    echo "dispatcher-rules-ratchet: no *.toml rule files in $RULES_DIR" >&2
    echo "" >&2
    echo "  An empty registry directory is a wrong path, not an empty" >&2
    echo "  registry — a reader must not report 'no rules' over a typo." >&2
    exit 1
fi

problems=0

for f in "${files[@]}"; do
    name="$(basename "$f" .toml)"

    blocks=$(grep -c '^\[\[rule\]\]' "$f" || true)
    if [ "$blocks" -ne 1 ]; then
        echo "dispatcher-rules-ratchet: $f holds $blocks [[rule]] blocks; expected exactly one" >&2
        echo "" >&2
        echo "  One file per rule is what makes the listing the definition:" >&2
        echo "  \`ls $RULES_DIR\` answers 'which rules', and adding one touches" >&2
        echo "  no shared line. Split the extra rule into its own file." >&2
        problems=$((problems + 1))
        continue
    fi

    if ! grep -q "^name = \"$name\"\$" "$f"; then
        echo "dispatcher-rules-ratchet: $f does not hold a rule named \`$name\`" >&2
        echo "" >&2
        echo "  The file name IS the rule name. Read wrong, the file would" >&2
        echo "  carry a reaction nobody can find by \`ls\`." >&2
        problems=$((problems + 1))
        continue
    fi

    # `why` is authored as a multi-line basic string. Non-empty means at
    # least one line of prose between the delimiters — a `why = """"""`
    # would satisfy a grep for the key and say nothing.
    why=$(awk '/^why = """$/{f=1;next} /^"""$/{f=0} f' "$f" | tr -d '[:space:]')
    if [ -z "$why" ]; then
        echo "dispatcher-rules-ratchet: rule \`$name\` has no \`why\` ($f)" >&2
        echo "" >&2
        echo "  A new dispatcher rule is a reaction the protocol definition" >&2
        echo "  could not express (protocol-policy-publish.md). Say in a" >&2
        echo "  why = \"\"\"…\"\"\" field which standing exemption it claims —" >&2
        echo "  timer, threshold, external ingress/glue, or cross-protocol" >&2
        echo "  reactor — and why. Otherwise: declare it in the Workflow" >&2
        echo "  definition instead. See $RULES_DIR/README.md." >&2
        problems=$((problems + 1))
    fi
done

if [ "$problems" -ne 0 ]; then
    echo "" >&2
    echo "dispatcher-rules-ratchet: $problems rule file(s) failed" >&2
    exit 1
fi

echo "dispatcher-rules-ratchet: OK (${#files[@]} rules, each saying why it exists)"
