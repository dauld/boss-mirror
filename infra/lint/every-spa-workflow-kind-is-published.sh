#!/usr/bin/env bash
# every-spa-workflow-kind-is-published.sh — every workflow kind the SPA
# hardcodes is a PLATFORM workflow, authored under
# infra/platform/workflows/ and therefore published by every instance.
#
# THE ABSENCE THIS FILLS (backlog 423a531d, 2026-09-22). Nothing held a
# rendered or queried workflow kind to anything. Five sites in the
# shared SPA filtered on three kinds the running instance does not
# publish, and each rendered a titled page that then said, correctly and
# permanently, "No jobs match":
#
#   App.svelte             initialKind="field-service"  -> /ux/service
#   App.svelte             initialKind="sale"           -> /ux/sales
#   support/SupportPage    /api/jobs?kind=field-service&limit=5000
#   accounts/AccountsList  /api/jobs?kind=field-service&limit=5000
#   jobs/types.ts          a campaign Subject links to /jobs?kind=marketing-motion
#
# All three are TENANT kinds — `field-service` and `marketing-motion`
# are authored in examples/used-device-shop/seeds/workflows.toml,
# `sale` in examples/brewery/seeds/workflows.toml — hardcoded into core
# frontend that every instance ships (CLAUDE.md §10). The Algedonic
# instance runs neither tenant, so all five reads were structurally 0
# against 5700 packets. The read itself was honest (JobsListPage tells
# a failed read from an empty one); the FILTER was about a protocol
# nobody had authored here.
#
# THE CHECKED PROPERTY. A workflow kind written as a literal in the
# shared SPA must be one of `infra/platform/workflows/*.toml` — the
# protocols the platform itself runs, which every instance has. A
# tenant's kind is not, and a department's work is not one kind anyway:
# Sales runs `receive-a-sponsorship` AND `receive-an-inquiry`, so the
# honest filter for a department surface is `/api/jobs?department=<code>`,
# which the server resolves through the workflow rows declaring that
# department. That is the fix this lint points at, not a bigger
# hardcoded list.
#
# WHAT COUNTS AS A LITERAL, and why only these two shapes:
#
#   1. `[?&]kind=<word>` on a line that names a jobs listing — a URL
#      holding `/jobs?`. The path qualifier is what keeps
#      `/api/messages/unread/{uid}?kind=direct` out: `direct` is a
#      MESSAGE kind, and a lint that read every `kind=` would have
#      reported it (the packet's own first sweep did, along with six
#      `subject_kind=` tails).
#   2. `initialKind="<word>"` — the JobsListPage prop that becomes the
#      same query one component away.
#
# An interpolated kind (`kind=${encodeURIComponent(k)}`) is not a
# literal and is not checked here: what it carries is a runtime value,
# and holding THAT to the registry is a boot-time question, not a
# build-time one. Comments are stripped first (lib/strip-comments.sh),
# so prose naming a kind is a mention, not a use.
#
# Usage: infra/lint/every-spa-workflow-kind-is-published.sh [--self-test]
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/strip-comments.sh
. "$here/lib/strip-comments.sh" || exit 3
# shellcheck source=infra/lint/lib/scanned.sh
. "$here/lib/scanned.sh" || exit 3
export -f strip_comments

# The shared product sources this lint reads, one path per line. Test
# files are excluded — a test may assert on a tenant kind on purpose —
# as is the dev server, which is a routing table, not a surface.
sources() {
    find "$1/apps/web/src" "$1/libs/web-kit/src" "$1/apps/simulator/src" \
        \( -name '*.ts' -o -name '*.svelte' \) \
        ! -name '*.test.ts' ! -name 'dev-server.ts' -type f 2>/dev/null | sort
}

# The workflow kinds every instance publishes: one file per protocol
# under infra/platform/workflows/, the directory being the definition
# (CLAUDE.md §9a — no manifest to drift from it). Sorted, one per line.
published() {
    find "$1/infra/platform/workflows" -maxdepth 1 -name '*.toml' -type f 2>/dev/null \
        | sed -E 's|^.*/||; s|\.toml$||' | sort -u
}

# The two literal shapes, out of one stripped stream. Sorted, unique.
hardcoded() {
    local root="$1"
    {
        sources "$root" | xargs -d '\n' -r bash -c 'strip_comments "$@"' _ \
            | grep -E '/jobs\?' | grep -oE '[?&]kind=[a-z][a-z0-9-]*' | sed -E 's/^.kind=//'
        sources "$root" | xargs -d '\n' -r bash -c 'strip_comments "$@"' _ \
            | grep -oE "initialKind=[\"'][a-z][a-z0-9-]*" | sed -E "s/^initialKind=.//"
    } | sort -u
}

# Hardcoded kinds that no platform workflow publishes.
unpublished() { comm -23 <(hardcoded "$1") <(published "$1"); }

# Where one kind is written, `file:line` with the line, at most three.
sites() {
    local root="$1" kind="$2" f
    while IFS= read -r f; do
        strip_comments "$f" \
            | grep -nE "([?&]kind=${kind}\b|initialKind=[\"']${kind}[\"'])" \
            | sed "s|^|${f#"$root"/}:|"
    done < <(sources "$root") | sed -n '1,3p'
}

self_test() {
    local fx
    fx="$(mktemp -d)"
    # Not a RETURN trap: one set here fires again when a later
    # `.`-sourced file finishes, with $fx out of scope (2026-09-18).
    mkdir -p "$fx/infra/platform/workflows" "$fx/apps/web/src/it" \
        "$fx/libs/web-kit/src" "$fx/apps/simulator/src"
    : >"$fx/infra/platform/workflows/pr-train.toml"
    : >"$fx/infra/platform/workflows/ship-a-change.toml"
    cat >"$fx/apps/web/src/it/Yard.svelte" <<'SV'
    <!-- a Svelte comment naming /api/jobs?kind=ghost-html is a mention -->
    fetch('/api/jobs?kind=pr-train&limit=40');
    <JobsListPage initialKind="ship-a-change" />
    <JobsListPage initialKind="field-service" />
SV
    cat >"$fx/apps/web/src/it/reads.ts" <<'TS'
    /// The /api/jobs?kind=ghost-doc listing is not read from here.
    fetch(`/api/jobs?kind=${encodeURIComponent(k)}&limit=1`);
    fetch('/api/jobs?subject_kind=employee&limit=1');
    fetch(`/api/messages/unread/${uid}?kind=direct`);
    fetch('/api/jobs?kind=ship-a-change&limit=200'); // and /api/jobs?kind=ghost-line
TS
    printf "fetch('/api/jobs?kind=ghost-test');\n" >"$fx/apps/web/src/paginated.test.ts"
    local got
    got="$(unpublished "$fx" | tr '\n' ' ' | sed 's/ $//')"
    [[ "$got" == "field-service" ]] || {
        echo "every-spa-workflow-kind-is-published: self-test FAILED — expected the planted 'field-service' alone, got '${got}'" >&2
        rm -rf "$fx"
        return 1
    }
    local where
    where="$(sites "$fx" field-service)"
    [[ "$where" == *"apps/web/src/it/Yard.svelte:4:"* ]] || {
        echo "every-spa-workflow-kind-is-published: self-test FAILED — expected the site report to name Yard.svelte line 4, got '${where}'" >&2
        rm -rf "$fx"
        return 1
    }
    echo "every-spa-workflow-kind-is-published: self-test ok — planted field-service caught and located at its real line; pr-train and ship-a-change published; an interpolated kind, a subject_kind tail, a message kind on /api/messages, four ghost mentions in an HTML comment, a docstring and a trailing comment, and a test file's kind all ignored"
    rm -rf "$fx"
}

if [[ "${1:-}" == "--self-test" ]]; then
    self_test
    exit $?
fi
self_test || exit 1

repo="$(cd "$here/../.." && pwd)"
missing="$(unpublished "$repo")"
count="$(hardcoded "$repo" | wc -l | tr -d ' ')"
if [[ -n "$missing" ]]; then
    echo "every-spa-workflow-kind-is-published: FAIL — the SPA hardcodes workflow kind(s) no platform workflow publishes:" >&2
    while read -r kind; do
        [[ -z "$kind" ]] && continue
        echo "  ${kind} — written at:" >&2
        sites "$repo" "$kind" | sed 's|^|    |' >&2
    done <<<"$missing"
    {
        echo "  A kind authored only in examples/<tenant>/seeds/workflows.toml is not published by an"
        echo "  instance running another tenant, so the surface renders a titled page and a permanent"
        echo "  'No jobs match' (CLAUDE.md §10). A department's work is several kinds anyway: filter"
        echo "  with /api/jobs?department=<code>, which the server resolves through the workflow rows"
        echo "  declaring that department, or read the kind from the registry instead of writing it."
    } >&2
    exit 1
fi
lint_scanned every-spa-workflow-kind-is-published "$count" "workflow kind(s) hardcoded in the SPA"
echo "every-spa-workflow-kind-is-published: ${count} workflow kinds hardcoded in the SPA, every one published by a platform workflow"
exit 0
