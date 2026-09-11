#!/usr/bin/env bash
# one-palette.sh — the frontend has ONE palette, and it is ours.
#
# WHY THIS EXISTS. David, 2026-08-23, on
# /ux/marketing-assets/ma-seasonal-winter-2026: "I can't read the text
# on the tags on this page... Might be light/dark theme support, which
# we should eliminate. Everything should be our UI decision so we can
# make sure it looks good."
#
# The diagnosis was half right in a way worth writing down. There was
# no theme SWITCH — no `prefers-color-scheme` block, no [data-theme]
# attribute, no toggle — anywhere in apps/ or libs/. What produced an
# unreadable chip was a component that hand-rolled a LIGHT ground
# (#e7e5e4) under text the app paints in --fog, i.e. a second palette
# smuggled in as literal hex. Design Language v1.0 already collapsed
# .theme-exec / .theme-ops onto one palette (styles.css §Compatibility
# layer); the leftovers are per-component, not per-theme.
#
# So this lint guards the door the mechanism would come back through.
# `prefers-color-scheme` is the one construct that makes the app's
# appearance a function of the VIEWER'S OS rather than a decision we
# made: it forks every token it touches into two palettes that no
# review ever sees at once, and the fork is invisible to anyone whose
# machine sits on the other side of it. One palette means the design
# review that looked good is the design every operator gets.
#
# Scope: apps/ and libs/ — the frontend surfaces. Not crates/ (Rust
# has no stylesheet), not docs/ (prose legitimately names the media
# feature when explaining why we don't use it).
#
# NOT covered on purpose: Playwright's `emulateMedia({ colorScheme })`.
# apps/web/tests/mocked/chrome-consistency.mocked.spec.ts renders the
# chrome under an OS light preference and measures contrast — that is
# a test of the ONE palette holding up regardless of what the OS asks
# for, which is the property this lint defends, not a violation of it.
set -euo pipefail
cd "$(dirname "$0")/../.."

# The specs that PROVE this rule have to name the pattern to test it —
# the same exemption no-session-paths.sh gives docs. Excluding the
# lint alone was not enough: one-palette.mocked.spec.ts documents the
# rule in a comment, and the train that first carried both went red on
# it while the branch passed alone (2026-08-24).
#
# That exemption is now DISCOVERED rather than listed: lib/pattern-scan.sh
# exempts this file and any tracked path that names this lint and lives
# where tests live, which is exactly `apps/web/tests/mocked/
# one-palette.mocked.spec.ts` and nothing else in the tree. Collapsing
# onto the shared scan is §9a — the hand-written `|| true` here printed
# `clean` and exited 0 on a tree git had refused to read (measured
# 2026-09-11, backlog 6b2f4a1a), and it would have had to be fixed twice
# otherwise. `|| exit $?` carries the scan's refusal out.
. "$(dirname "$0")/lib/pattern-scan.sh"
hits=$(pattern_scan 'prefers-color-scheme' -- 'apps/' 'libs/') || exit $?
if [ -n "$hits" ]; then
    echo "one-palette: a frontend surface branches on the viewer's OS theme:" >&2
    echo "$hits" >&2
    echo >&2
    echo "BOSS ships one designed palette (--void/--ink/--fog/--hairline/" >&2
    echo "--static, apps/web/src/styles.css :root). Pick the token that says" >&2
    echo "what the element IS — a ground, a hairline, secondary text — rather" >&2
    echo "than forking it by what the viewer's machine prefers." >&2
    exit 1
fi
echo "one-palette: clean"
