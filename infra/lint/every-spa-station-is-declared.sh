#!/usr/bin/env bash
# every-spa-station-is-declared.sh — every station name the SPA
# hardcodes is a PLATFORM station, declared under
# infra/platform/stations/ and therefore served by every instance.
#
# THE ABSENCE THIS FILLS (backlog 31c371b3, 2026-09-22). The yard's
# approach lane read `fetchStationQueue('publish-dock')` from the day it
# was drawn (a031da14, 2026-08-31). No stations row by that name was
# ever authored — not by a migration, not in this bundle — so the read
# answered 404 on every load, the fetch swallowed it as "no reading",
# and the lane drew EMPTY, which is exactly what "nothing is publishing"
# looks like. GET /api/stations served 15 stations; publish-dock was
# never one. It is the workflow-kind defect of 423a531d one registry
# over (every-spa-workflow-kind-is-published.sh), and the fix is the
# same: correcting the literal fixes one surface, holding every literal
# to the registry closes the absence.
#
# THE CHECKED PROPERTY. A station name written as a literal in the
# shared SPA must be one of `infra/platform/stations/*.toml` — the
# stations the platform seed publishes on every instance, the directory
# being the definition (CLAUDE.md §9a). A station an operator authored
# live on one instance (the `q.<role>.<kind>` queues) is not in every
# instance, so a shared surface must not name it either; reading one is
# a runtime question, answered from `GET /api/stations`.
#
# WHAT COUNTS AS A LITERAL, three shapes:
#
#   1. `/api/stations/<name>/` — a station's own path (queue, publish).
#      The trailing `/` is what keeps `/api/stations/load` and
#      `/api/stations/flow`, which are ENDPOINTS over every station,
#      out of the set.
#   2. `fetchStationQueue('<name>')` — the yard's reader, which builds
#      shape 1 from its argument one line away.
#   3. `<NAME>_STATION = '<name>'` — a constant that is then
#      interpolated into shape 1 (watchlist.ts's WATCHLIST_STATION).
#
# An interpolated name (`/api/stations/${encodeURIComponent(name)}/…`)
# is a runtime value, not a literal, and is not checked. Comments are
# stripped first (lib/strip-comments.sh), so prose naming a station is
# a mention, not a use.
#
# Usage: infra/lint/every-spa-station-is-declared.sh [--self-test]
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/strip-comments.sh
. "$here/lib/strip-comments.sh" || exit 3
# shellcheck source=infra/lint/lib/scanned.sh
. "$here/lib/scanned.sh" || exit 3
export -f strip_comments

# The shared product sources this lint reads, one path per line — the
# same set every-spa-workflow-kind-is-published.sh reads, for the same
# reasons: a test may name a missing station on purpose, and the dev
# server is a routing table, not a surface.
sources() {
    find "$1/apps/web/src" "$1/libs/web-kit/src" "$1/apps/simulator/src" \
        \( -name '*.ts' -o -name '*.svelte' \) \
        ! -name '*.test.ts' ! -name 'dev-server.ts' -type f 2>/dev/null | sort
}

# The stations every instance serves: one file per station under
# infra/platform/stations/, named for it. Sorted, one per line.
declared() {
    find "$1/infra/platform/stations" -maxdepth 1 -name '*.toml' -type f 2>/dev/null \
        | sed -E 's|^.*/||; s|\.toml$||' | sort -u
}

# A station name, as the registry spells them: `loading-dock`,
# `q.platform-admin.task`.
NAME='[a-z][a-z0-9.-]*'

# The three literal shapes, out of one stripped stream. Sorted, unique.
hardcoded() {
    sources "$1" | xargs -d '\n' -r bash -c 'strip_comments "$@"' _ \
        | grep -oE "/api/stations/${NAME}/|fetchStationQueue\\([\"'\`]${NAME}[\"'\`]|[A-Z_]+_STATION = [\"'\`]${NAME}[\"'\`]" \
        | sed -E "s|^/api/stations/||; s|/\$||; s|^fetchStationQueue\\(.||; s|^[A-Z_]+_STATION = .||; s|[\"'\`]\$||" \
        | sort -u
}

# Hardcoded stations that no platform file declares.
undeclared() { comm -23 <(hardcoded "$1") <(declared "$1"); }

# Where one station is written, `file:line` with the line, at most three.
sites() {
    local root="$1" name="$2" f esc
    esc="${name//./\\.}"
    while IFS= read -r f; do
        strip_comments "$f" \
            | grep -nE "(/api/stations/${esc}/|fetchStationQueue\\([\"'\`]${esc}[\"'\`]|_STATION = [\"'\`]${esc}[\"'\`])" \
            | sed "s|^|${f#"$root"/}:|"
    done < <(sources "$root") | sed -n '1,3p'
}

self_test() {
    local fx
    fx="$(mktemp -d)"
    # Not a RETURN trap: one set here fires again when a later
    # `.`-sourced file finishes, with $fx out of scope (2026-09-18).
    mkdir -p "$fx/infra/platform/stations" "$fx/apps/web/src/it" \
        "$fx/libs/web-kit/src" "$fx/apps/simulator/src"
    : >"$fx/infra/platform/stations/loading-dock.toml"
    : >"$fx/infra/platform/stations/my-watchlist.toml"
    : >"$fx/infra/platform/stations/q.platform-admin.task.toml"
    cat >"$fx/apps/web/src/it/yard.ts" <<'TS'
    /** Reads `/api/stations/ghost-block/queue` — a mention, in a docstring. */
    fetchStationQueue('loading-dock'),
    fetchStationQueue('publish-dock'),
    fetch('/api/stations/load');
    fetch(`/api/stations/flow?window_hours=${h}`);
    fetch(`/api/stations/${encodeURIComponent(name)}/queue`);
    fetch('/api/stations/q.platform-admin.task/queue'); // and /api/stations/ghost-line/queue
TS
    cat >"$fx/apps/web/src/it/Watch.svelte" <<'SV'
    <!-- /api/stations/ghost-html/queue is not read from here -->
    export const WATCHLIST_STATION = 'my-watchlist';
    export const REVIEW_STATION = 'ghost-review';
SV
    printf "fetchStationQueue('ghost-test');\n" >"$fx/apps/web/src/yard.test.ts"
    local got
    got="$(undeclared "$fx" | tr '\n' ' ' | sed 's/ $//')"
    [[ "$got" == "ghost-review publish-dock" ]] || {
        echo "every-spa-station-is-declared: self-test FAILED — expected the planted 'ghost-review publish-dock' alone, got '${got}'" >&2
        rm -rf "$fx"
        return 1
    }
    local where
    where="$(sites "$fx" publish-dock)"
    [[ "$where" == *"apps/web/src/it/yard.ts:3:"* ]] || {
        echo "every-spa-station-is-declared: self-test FAILED — expected the site report to name yard.ts line 3, got '${where}'" >&2
        rm -rf "$fx"
        return 1
    }
    echo "every-spa-station-is-declared: self-test ok — planted publish-dock (reader) and ghost-review (constant) caught, publish-dock located at its real line; loading-dock, my-watchlist and a dotted q. station declared; the load and flow endpoints, an interpolated name, three ghost mentions in a docstring, an HTML comment and a trailing comment, and a test file's station all ignored"
    rm -rf "$fx"
}

if [[ "${1:-}" == "--self-test" ]]; then
    self_test
    exit $?
fi
self_test || exit 1

repo="$(cd "$here/../.." && pwd)"
missing="$(undeclared "$repo")"
count="$(hardcoded "$repo" | wc -l | tr -d ' ')"
if [[ -n "$missing" ]]; then
    echo "every-spa-station-is-declared: FAIL — the SPA hardcodes station name(s) no platform station declares:" >&2
    while read -r name; do
        [[ -z "$name" ]] && continue
        echo "  ${name} — written at:" >&2
        sites "$repo" "$name" | sed 's|^|    |' >&2
    done <<<"$missing"
    {
        echo "  A station that is not a file under infra/platform/stations/ is not served by every"
        echo "  instance, so its queue read answers 404 and the surface draws an empty lane that"
        echo "  looks like an idle one. Declare the station there (one file, CLAUDE.md §9a), or read"
        echo "  what the lane wants where it lives — the packets themselves, /api/jobs?kind=<platform kind>."
    } >&2
    exit 1
fi
lint_scanned every-spa-station-is-declared "$count" "station name(s) hardcoded in the SPA"
echo "every-spa-station-is-declared: ${count} station names hardcoded in the SPA, every one declared by a platform station"
exit 0
