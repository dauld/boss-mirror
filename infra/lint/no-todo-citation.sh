#!/usr/bin/env bash
# Reject `TODO #N` / `TODO.md #N` numeric citations in code comments.
#
# These references used to point at a now-dead numbered TODO.md
# scheme; the current TODO.md is prose-structured under H2/H3
# headings, so the numbers in code don't resolve to anything stable.
# A comment that says "see TODO #46" is no more useful than one that
# names the actual concern in plain English — the latter survives
# refactors, the former rots.
#
# Allowed: GitHub issue refs in code comments (we explicitly don't
# block `#NNN` on its own, only the `TODO #NNN` form), Markdown
# headers in TODO.md itself (the regex below skips *.md).
#
# Usage: ./infra/lint/no-todo-citation.sh
# CI: run from .github/workflows or pre-commit hook.

set -euo pipefail

cd "$(dirname "$0")/../.."
# shellcheck source=infra/lint/lib/scanned.sh
. infra/lint/lib/scanned.sh || exit 3

pattern='(TODO|TODO\.md) #[0-9]+'

# Restrict to the source trees we actually maintain. Excludes target/
# build dirs, generated/, node_modules, and Markdown (TODO.md itself
# is allowed to use whatever convention it wants).
violations=$(grep -rEon "$pattern" crates/ apps/ \
  --include="*.rs" \
  --include="*.ts" \
  --include="*.svelte" \
  2>/dev/null || true)

if [ -z "$violations" ]; then
  lint_scanned no-todo-citation "$(find crates apps -type f \( -name '*.rs' -o -name '*.ts' -o -name '*.svelte' \) | wc -l | tr -d ' ')" "source file(s) under crates/ and apps/"
  echo "no-todo-citation: clean"
  exit 0
fi

count=$(echo "$violations" | wc -l | tr -d ' ')
echo "no-todo-citation: $count violation(s) — these comments cite a numbered TODO that doesn't resolve:"
echo ""
echo "$violations"
echo ""
echo "Replace each citation with a description of the actual concern."
exit 1
