#!/usr/bin/env bash
# ci-image-report.sh — is the CI runner's local boss-ci image the one
# the registry serves? READ-ONLY, with ITS VERDICT.
#
# WHY. The image-freshness sweep (maintenance-sweep-image-freshness-
# daily) asks one question, recorded in its rule's `why`: a registry
# retag does not refresh a runner's LOCAL tag, and on forge train #1
# the runner ran a 31-hour-old boss-ci image while the registry served
# a newer one — a 25-minute mystery that fails in the worst direction,
# CI green or red against an image nobody is looking at (build.sh's
# header warned so, and the drift happened anyway: a comment is not a
# mechanism, CLAUDE.md §9a). Until 2026-09-18 the sweep measured
# itself with `disk-report`, a copy of disk-headroom's measure rule,
# whose verdict is the disk floor and says nothing about image age —
# so its Inspect step could never be judged by rule and sat on the
# agent (backlog 18df96c4, left by the builder of 970c0c94). This is
# the verb that answers the sweep's own question, in the shape the
# allowlist demands: one fixed script, no arguments, nothing mutated.
#
# WHAT IT COMPARES. CI pins every job to `boss-ci:<commit sha>`
# (.forgejo/workflows/ci.yml), which cannot be served stale — the
# runner has never seen the tag, so it pulls. The tag that CAN drift
# is the FLOATING one build.sh and build-image both push (`rust1.96`,
# the tag the gate runner and the dev pod pull), served from the
# runner's local cache once it holds one. So: the repo digest the
# SYSTEM docker daemon holds under `<repo>:<tag>` (the daemon Forgejo
# Actions jobs run in — `sudo -n docker`, the same read disk-report
# and disk-floor-sweep take, refused loudly when the daemon reached is
# not the one meant) against the digest the registry serves for the
# same tag, read over the registry's own HTTP API through the
# anonymous pull-token flow (the forge package is public; verified from
# the pod 2026-09-18, and install-cli-from-image.sh pulls the same way).
# The local image's AGE is read per-image from `{{.Created}}`, the way
# prune-ci-images.lib.sh reads it — never parsed out of docker's human
# "31 hours ago" column.
#
# THE READING CARRIES ITS VERDICT. The last line is ONE machine-
# readable verdict that judge-image-freshness-sweep-on-report-answered
# reads (maintenance.sweep.judge), in the shape every sweep report
# ends with (boss-testing/tests/sweep_report_verdicts_sh.rs):
#
#   verdict: clean
#       the runner's floating tag IS the registry's, or the runner
#       holds no floating tag at all (nothing can be served stale)
#   verdict: image_stale local=<digest> registry=<digest> age=<h>h
#       the runner's tag is behind the registry, and has been <h>
#       hours since the local image was built
#   verdict: unanswered (...)
#       the daemon or the registry could not be read — NOT a fresh
#       image, and the reason rides in the line
#
# The exit code stays 0 either way: a finding is an answer, not a
# failure (conformance-report.sh, disk-report.sh).
#
# Pinned by crates/core/boss-testing/tests/ci_image_report_sh.rs,
# which runs this script against a stubbed daemon and registry.
set -uo pipefail

say() { printf '%s\n' "$*"; }
hr()  { say "== $* =="; }

# The CI image repo, from forge-defaults.sh off /etc/boss/sor.env
# (BOSS_CI_IMAGE_REPO overrides; a test names it outright).
. "$(dirname "$0")/forge-defaults.sh"
forge_need CI_IMAGE_REPO
# The floating tag: build.sh's default, the tag ci.yml's build-image
# pushes beside the per-commit one.
TAG="${BOSS_CI_TAG:-rust1.96}"
# WHICH DAEMON, EXPLICITLY (disk-floor-sweep.sh says the long
# version): `sudo -n docker` is root's docker — the SYSTEM daemon the
# Actions jobs run in. Deliberately word-split, like the prune lib's.
SYSTEM_DOCKER="${BOSS_CI_IMAGE_DOCKER:-sudo -n docker}"
SYSTEM_DOCKER_ROOT="${BOSS_CI_IMAGE_DAEMON_ROOT:-/var/lib/docker}"
# The registry reader, overridable so the report is testable offline.
CURL="${BOSS_CI_REGISTRY_CURL:-curl}"

REPO="$CI_IMAGE_REPO"
# `host:port/owner/name` -> the registry's base URL and the repo path.
# Plain HTTP: the forge registry is HTTP on the LAN (prune-registry-
# tags.lib.sh's `--insecure` is the same fact).
REGISTRY_URL="http://${REPO%%/*}"
REPO_PATH="${REPO#*/}"

unanswered=""

# ---------------------------------------------------------------------
hr "system docker (CI jobs run here; $SYSTEM_DOCKER)"
# shellcheck disable=SC2086
if ! root="$($SYSTEM_DOCKER info --format '{{.DockerRootDir}}' 2>/dev/null)" || [ -z "$root" ]; then
    say "system docker: not readable (\`$SYSTEM_DOCKER info\` refused — sudo -n denied, or no daemon there)"
    unanswered="system docker could not be read (\`$SYSTEM_DOCKER info\` refused)"
elif [[ "$root" != *"$SYSTEM_DOCKER_ROOT"* ]]; then
    say "system docker: \`$SYSTEM_DOCKER\` reached the daemon rooted at $root, not the one at $SYSTEM_DOCKER_ROOT — a report on the wrong daemon reads clean about an image it never saw"
    unanswered="\`$SYSTEM_DOCKER\` is the daemon rooted at $root, not $SYSTEM_DOCKER_ROOT"
fi

# The first 200 bytes of a captured stderr, on one line — `head` is the
# producer and `tr` drains it, so no reader exits early under pipefail
# (a_lint_that_cannot_read_does_not_say_clean.rs, no-producer-coin).
err_tail() { head -c 200 "$1" | tr -d '\n'; }

age_hours() {
    # Hours since an RFC3339 creation time; empty when unreadable.
    local epoch
    epoch="$(date -u -d "$1" +%s 2>/dev/null)" || return 1
    [ -n "$epoch" ] || return 1
    echo $(( ( $(date -u +%s) - epoch ) / 3600 ))
}

if [ -z "$unanswered" ]; then
    say "-- $REPO tags in the daemon (age from {{.Created}}, size MiB) --"
    # shellcheck disable=SC2086
    if listing="$($SYSTEM_DOCKER images "$REPO" --format '{{.ID}} {{.Tag}}' 2>/dev/null)"; then
        n=0
        while IFS=' ' read -r id tag; do
            [ -n "${id:-}" ] || continue
            n=$((n + 1))
            # shellcheck disable=SC2086
            meta="$($SYSTEM_DOCKER image inspect --format '{{.Created}} {{.Size}}' "$id" 2>/dev/null)" || meta=""
            created="${meta%% *}"; size="${meta##* }"
            case "$size" in ''|*[!0-9]*) size=0 ;; esac
            if [ -n "$created" ] && age="$(age_hours "$created")"; then
                say "$tag	${age}h	$(( size / 1048576 ))MiB"
            else
                say "$tag	?	(metadata unreadable)"
            fi
        done <<<"$listing"
        say "($n tag(s); per-commit tags are pruned by disk-floor-sweep, never served stale)"
    else
        say "\`$SYSTEM_DOCKER images $REPO\` failed — the tag set is unknown"
    fi
fi

# ---------------------------------------------------------------------
hr "the floating tag $REPO:$TAG — the runner's copy"
local_digest=""; local_age=""; local_absent=0
if [ -z "$unanswered" ]; then
    # shellcheck disable=SC2086
    if floating="$($SYSTEM_DOCKER image inspect --format '{{.Created}} {{join .RepoDigests " "}}' "$REPO:$TAG" 2>/dev/null)" && [ -n "$floating" ]; then
        created="${floating%% *}"
        digests="${floating#* }"
        [ "$digests" = "$floating" ] && digests=""
        for d in $digests; do
            case "$d" in "$REPO@sha256:"*) local_digest="${d#*@}" ;; esac
        done
        if local_age="$(age_hours "$created")"; then
            say "created: $created (${local_age}h ago)"
        else
            say "created: $created (age unreadable)"
            local_age=""
        fi
        if [ -n "$local_digest" ]; then
            say "repo digest: $local_digest"
        else
            say "repo digest: none for $REPO — this image was built here and never pushed or pulled, so no registry digest vouches for it"
            unanswered="the local $REPO:$TAG carries no repo digest for $REPO (built locally, never pushed or pulled)"
        fi
    else
        local_absent=1
        say "the runner holds no local $REPO:$TAG — a tag it has never pulled cannot be served stale"
    fi
fi

# ---------------------------------------------------------------------
hr "the floating tag $REPO:$TAG — what the registry serves"
registry_digest=""
# The registry's own API, through the anonymous pull-token flow
# (install-cli-from-image.sh is the long version): /v2/ answers 401
# with the realm and service of its token endpoint, that endpoint hands
# out a pull token with no credentials for a public package, and a
# manifest request for the tag carries Docker-Content-Digest — the
# digest `docker pull` records in RepoDigests, whatever the media type.
# Every answer is captured whole; nothing is retried or guessed.
if [ -z "$unanswered" ] && [ "$local_absent" -eq 0 ]; then
    tmp="$(mktemp -d)"
    accept='application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json, application/vnd.docker.distribution.manifest.list.v2+json'
    auth_header=""
    # shellcheck disable=SC2086
    code="$($CURL -sS -m 15 -o "$tmp/ping.body" -D "$tmp/ping.hdr" -w '%{http_code}' "$REGISTRY_URL/v2/" 2>"$tmp/ping.err")"; rc=$?
    case "$rc:$code" in
        0:200) say "$REGISTRY_URL/v2/ answered 200 without a token; reading anonymously" ;;
        0:401)
            auth="$(tr -d '\r' <"$tmp/ping.hdr" | sed -n 's/^[Ww][Ww][Ww]-[Aa]uthenticate: *//p')"
            auth="${auth%%$'\n'*}"
            realm="$(printf '%s' "$auth" | sed -n 's/.*realm="\([^"]*\)".*/\1/p')"
            service="$(printf '%s' "$auth" | sed -n 's/.*service="\([^"]*\)".*/\1/p')"
            if [ -z "$realm" ]; then
                unanswered="registry: $REGISTRY_URL/v2/ answered 401 naming no token realm (Www-Authenticate: '$auth')"
            else
                turl="$realm?service=$service&scope=repository:$REPO_PATH:pull"
                # shellcheck disable=SC2086
                code="$($CURL -sS -m 15 -o "$tmp/token.json" -D "$tmp/token.hdr" -w '%{http_code}' "$turl" 2>"$tmp/token.err")"; rc=$?
                token=""
                [ "$rc" -eq 0 ] && [ "$code" = 200 ] && token="$(jq -r '.token // .access_token // empty' "$tmp/token.json" 2>/dev/null)"
                if [ -n "$token" ]; then
                    say "pull token for $REPO_PATH: granted anonymously"
                    auth_header="Authorization: Bearer $token"
                else
                    unanswered="registry: the anonymous pull token for $REPO_PATH was refused ($turl answered HTTP $code, curl exit $rc: $(err_tail "$tmp/token.err"))"
                fi
            fi
            ;;
        *)
            unanswered="registry: $REGISTRY_URL/v2/ did not answer (HTTP ${code:-none}, curl exit $rc: $(err_tail "$tmp/ping.err"))"
            ;;
    esac
    if [ -z "$unanswered" ]; then
        murl="$REGISTRY_URL/v2/$REPO_PATH/manifests/$TAG"
        # shellcheck disable=SC2086
        code="$($CURL -sS -m 15 -I -o "$tmp/tag.body" -D "$tmp/tag.hdr" -w '%{http_code}' -H "Accept: $accept" ${auth_header:+-H "$auth_header"} "$murl" 2>"$tmp/tag.err")"; rc=$?
        if [ "$rc" -eq 0 ] && [ "$code" = 200 ]; then
            registry_digest="$(sed -n 's/^[Dd]ocker-[Cc]ontent-[Dd]igest: *//p' "$tmp/tag.hdr" | tr -d '\r')"
            registry_digest="${registry_digest%%$'\n'*}"
            if [ -n "$registry_digest" ]; then
                say "$murl: $registry_digest"
            else
                unanswered="registry: $murl answered 200 with no Docker-Content-Digest header"
            fi
        else
            unanswered="registry: $murl answered HTTP ${code:-none} (curl exit $rc: $(err_tail "$tmp/tag.err")) — a 404 means the registry holds no $TAG tag"
        fi
    fi
    rm -rf "$tmp"
fi

# ---------------------------------------------------------------------
hr "verdict"
if [ -n "$unanswered" ]; then
    # Not knowing is not clean (disk-report's disk_unmeasured, for the
    # same reason): the judging rule reads this as a finding.
    say "verdict: unanswered ($unanswered)"
elif [ "$local_absent" -eq 1 ]; then
    say "verdict: clean"
elif [ "$local_digest" = "$registry_digest" ]; then
    say "verdict: clean"
else
    # Twelve hex of each, the width `docker images` names an image by;
    # the full digests are above for anyone who wants them.
    l="${local_digest#sha256:}"; r="${registry_digest#sha256:}"
    say "verdict: image_stale local=${l:0:12} registry=${r:0:12} age=${local_age:-?}h"
fi
