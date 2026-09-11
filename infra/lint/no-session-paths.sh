#!/usr/bin/env bash
# no-session-paths.sh — tracked source must not name an agent session's
# filesystem.
#
# WHY THIS EXISTS. Two mocked playwright specs shipped with screenshot
# paths under /Users/david/.claude/jobs/<id>/tmp/ — an agent session's
# scratch directory, committed as if it were a place. They stayed green
# on the forge CI because the boss-ci container runs as root and
# mkdir -p'd the absolute path INSIDE the container; GitHub's
# unprivileged runner refused, and PR #231's web check was the first
# thing anywhere to say so (2026-08-20). An environment that can
# silently absorb a wrong path is exactly why a lint has to hold the
# line instead.
#
# Scope: apps/ crates/ infra/ — code and infra, where a machine-local
# path is always a bug. docs/ is exempt on purpose: session reports and
# runbooks legitimately NAME such paths when telling their story.
set -euo pipefail
cd "$(dirname "$0")/../.."

# --line-number over tracked files only; the pattern catches macOS
# home paths and agent scratch dirs in one pass.
# The scan body is shared (lib/pattern-scan.sh): it excludes this file
# and any test that must name the pattern to prove the rule. The docs/
# exemption below is this lint's own judgement and stays visible here —
# runbooks legitimately TELL the story of a machine-local path.
#
# `|| exit $?` is load-bearing: the scan returns 3 when git could not
# run, and on 2026-09-11 this lint printed `clean` and exited 0 in a
# workspace where every git command was refusing (backlog 6b2f4a1a).
# A scan that did not happen is not a clean tree.
. "$(dirname "$0")/lib/pattern-scan.sh"
hits=$(pattern_scan '/Users/[a-z]+/|\.claude/jobs/' -- 'apps/' 'crates/' 'infra/') || exit $?
if [ -n "$hits" ]; then
    echo "no-session-paths: tracked source names a machine-local or session path:" >&2
    echo "$hits" >&2
    echo "Use a relative path, an env var, or (in tests) testInfo.outputPath()." >&2
    exit 1
fi
echo "no-session-paths: clean"
