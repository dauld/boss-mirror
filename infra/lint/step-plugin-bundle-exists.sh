#!/usr/bin/env bash
# Every active step_plugins row points at a bundle that exists.
#
# WHY. A step plugin is TWO artefacts that must agree: a row in
# `step_plugins` naming a `frontend_url`, and a JavaScript file at
# `infra/step-plugins/<that name>`. Nothing connected them. They are
# written in different languages, land in different directories, and
# neither one fails to load if the other is absent.
#
# WHERE A ROW IS DECLARED. Since 2026-09-18 (backlog 393d3234,
# consolidation H4, car 2) a row lives in the platform bundle,
# `infra/platform/step-plugins/<kind>.toml`, which the seed publishes
# insert-if-missing at every start; the seven migrations that declared
# rows before that day stay as history and still produce rows on a
# fresh database. Both are read here: a bundle file whose JS is missing
# is the same broken step as a migration's, and a migration row whose
# JS was deleted is still live on every deployment that ran it.
#
# WHAT THE FAILURE LOOKS LIKE. The SPA prefers a plugin over its
# built-in surface whenever the registry has an active row for a step's
# kind (`apps/web/src/steps/StepSurface.svelte` → `hasActivePluginFor`).
# So a row whose bundle is missing does not fall back — it commits to
# the plugin path and then 404s fetching it. The step renders empty or
# broken on the ONE surface a person needs in order to act, and it
# happens only in a deployed environment, because the row lives in the
# database and the file lives in the image. Nothing in a local test run
# looks at either.
#
# The reverse — a bundle with no row — is deliberately NOT an error. A
# bundle can be committed before the migration that activates it, which
# is the normal order for authoring one, and `checklist.js`/`sign-off.js`
# back kinds the SPA renders inline.
#
# WHAT IT DOES NOT DO. It reads the migrations, not a live database, so
# it cannot see a row inserted by hand or a bundle missing from a built
# image. It catches the authoring mistake at the point it enters the
# tree, which is where it is cheap.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1
# shellcheck source=infra/lint/lib/scanned.sh
. infra/lint/lib/scanned.sh
SCHEMA="infra/postgres/schema"
ROWS="infra/platform/step-plugins"
BUNDLES="infra/step-plugins"
[ -d "$SCHEMA" ] || { echo "step-plugin-bundle-exists: $SCHEMA not found" >&2; exit 1; }
[ -d "$ROWS" ] || { echo "step-plugin-bundle-exists: $ROWS not found" >&2; exit 1; }
[ -d "$BUNDLES" ] || { echo "step-plugin-bundle-exists: $BUNDLES not found" >&2; exit 1; }

# Pull the frontend_url from every INSERT INTO step_plugins (the seeds
# are hand-written with the value list on its own lines, so the bundle
# name is the lone single-quoted token ending in .js) and from every
# bundle row's `frontend_url = "<name>.js"` line.
urls=$( {
    grep -rhoE "'[A-Za-z0-9._/-]+\.js'" "$SCHEMA"/*.sql 2>/dev/null | tr -d "'"
    grep -hoE '^frontend_url *= *"[A-Za-z0-9._/-]+\.js"' "$ROWS"/*.toml 2>/dev/null \
        | sed -E 's/^frontend_url *= *"//; s/"$//'
} | sort -u)

count=$(printf '%s\n' "$urls" | grep -c . || true)
if [ "$count" -lt 1 ]; then
    echo "step-plugin-bundle-exists: found no .js references in $SCHEMA or $ROWS —" >&2
    echo "  the scrape broke, so a green result would mean nothing." >&2
    exit 1
fi
declared=$(grep -lE '^frontend_url *= *"' "$ROWS"/*.toml 2>/dev/null | wc -l | tr -d ' ')
if [ "$declared" -lt 1 ]; then
    echo "step-plugin-bundle-exists: no bundle row under $ROWS declares a frontend_url —" >&2
    echo "  the TOML scrape broke, so a green result would mean nothing." >&2
    exit 1
fi

missing=""
for u in $urls; do
    if [ ! -f "${BUNDLES}/${u}" ]; then
        missing="${missing}${u}"$'\n'
    fi
done

if [ -n "$missing" ]; then
    echo "step-plugin-bundle-exists: a step_plugins row names a bundle that" >&2
    echo "  does not exist in ${BUNDLES}/:" >&2
    printf '%s' "$missing" | sed 's/^/    /' >&2
    echo "" >&2
    echo "  The SPA prefers a registered plugin over its built-in surface, so" >&2
    echo "  this does not degrade gracefully — the step renders broken on the" >&2
    echo "  one surface someone needs to act on, and only once deployed." >&2
    echo "  Either add the bundle or drop the row (a row lives in" >&2
    echo "  $ROWS/<kind>.toml; a migration's is history)." >&2
    exit 1
fi

lint_scanned step-plugin-bundle-exists "$count" "registered bundle path(s)"
echo "step-plugin-bundle-exists: $count registered bundle(s), all present"
