#!/usr/bin/env bash
# the-estate-address-lives-once.sh — the system of record's IP and the
# forge's are spelled in ONE tree file, infra/estate/estate.toml, and
# nowhere else a machine reads.
#
# WHY. Audit H10 (2026-09-18, backlog 5222163e, design 42277636):
# `10.20.0.34:7900` was in 47 files under three spellings while
# infra/dev/sor-url called itself "the ONE spelling", and the forge's
# `10.20.0.15` in 62 — Exec-line prefixes, `${JOBS_API:-…}` fallbacks,
# registry prefixes, clone URLs, a journal door. Every one of them is a
# fallback that keeps answering on the day the address moves (CLAUDE.md
# §Doors: a wrong target answers instead of erroring), and the
# boss.algedonic.dev cutover would have had to find them all. Now a
# host's install renders /etc/boss/sor.env from the source, units read it
# with EnvironmentFile=, scripts through infra/lib/sor.sh, and this lint
# is what stops the literal creeping back.
#
# WHAT IT CHECKS. Both IPs are READ FROM THE SOURCE — this file carries
# neither — and every tracked file is scanned for them, except:
#
#   * the source itself, and this lint;
#   * docs (docs/, every *.md): prose about history names addresses;
#   * tests (crates/*/tests/, *.test.ts): fixtures are allowed to spell
#     the address they stub;
#   * migrations (infra/postgres/schema/): append-only history — the
#     estate rows and the credential issuer strings are what they were;
#   * comment lines (`#`): prose, like docs — see lines_with below;
#   * the ALLOWANCE below: a file that cannot read the env file yet, each
#     with the reason. A named set, never a count (§9a). An entry whose
#     file still exists but no longer carries the literal is STALE and
#     fails the lint — remove it. An entry whose file is gone is skipped
#     with a note: the bare-metal deletion (H7, e109bd71) and this car
#     land independently, and a stale entry must not red the train that
#     carries both.
#
# Exit 0 clean; 1 with every offending `file:line: text` on stderr; 3
# when the tree could not be read (lib/git-answer.sh).
set -uo pipefail

LINT=the-estate-address-lives-once
LINT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/lint/lib/git-answer.sh
. "$LINT_DIR/lib/git-answer.sh" || exit 3
# shellcheck source=infra/lint/lib/scanned.sh
. "$LINT_DIR/lib/scanned.sh" || exit 3

# The tree to judge: the repository this lint lives in, or the one the
# self-test names (a scratch repository holding a planted violation).
TREE="${BOSS_LINT_TREE:-$LINT_DIR/../..}"
cd "$TREE" || exit 3
git_can_answer "$LINT" || exit 3

SOURCE="infra/estate/estate.toml"
[ -r "$SOURCE" ] || { echo "$LINT: $SOURCE is missing — the one source the addresses live in" >&2; exit 1; }

sor_ip=$(sed -n 's|^sor_url = "http://\([0-9.]*\):[0-9]*"[[:space:]]*$|\1|p' "$SOURCE" | sed -n 1p)
forge_ip=$(sed -n 's|^forge_host = "\([0-9.]*\)"[[:space:]]*$|\1|p' "$SOURCE" | sed -n 1p)
if [ -z "$sor_ip" ] || [ -z "$forge_ip" ]; then
    echo "$LINT: $SOURCE does not spell both sor_url (http://<ip>:<port>) and forge_host (<ip>) — nothing to hold the tree to" >&2
    exit 1
fi

# THE ALLOWANCE — `<path>` TAB `<reason>`. Read the header before adding.
read -r -d '' ALLOWANCE <<'EOF'
.forgejo/workflows/ci.yml	the CI workflow: job-container image tags and the checkout-by-IP the runner needs before it has a checkout (ci-checks-out-from-the-forge.sh); the reclaim job's BOSS_JOBS_URL rides solo as a gate-blind car (follow-up)
.forgejo/workflows/install-smoke.yml	the nightly install-smoke workflow: its clone-by-IP on the forge runner (gate-blind, as ci.yml)
crates/core/boss-dispatcher/src/config.rs	the dispatcher's BOSS_FORGE_URL default, read as before (the CLI/Rust layer is out of this car: env is where units set it)
crates/core/boss-jobs/src/credentials/http.rs	credential issuer label "forgejo (<ip>)" — registry data mirrored from the migration
crates/core/boss-jobs/src/credentials/in_memory.rs	credential issuer label "forgejo (<ip>)" — registry data mirrored from the migration
crates/core/boss-jobs/src/probe.rs	#[cfg(test)] probe fixture inside a src file
crates/orchestrators/boss-cli/src/cadence.rs	the CLI names the record in a refusal message (reads BOSS_JOBS_URL as before)
crates/orchestrators/boss-cli/src/census.rs	#[cfg(test)] assertion that a refusal names the record
crates/orchestrators/boss-cli/src/credential.rs	BOSS_TRAIN_FORGE_URL default and a refusal message (the CLI reads env as before)
crates/orchestrators/boss-cli/src/gate.rs	refusal prose plus #[cfg(test)] probe fixtures
crates/orchestrators/boss-cli/src/git_auth.rs	BOSS_TRAIN_FORGE_URL default plus #[cfg(test)] fixtures
crates/orchestrators/boss-cli/src/host_readiness.rs	#[cfg(test)] estate fixture
crates/orchestrators/boss-cli/src/prove.rs	#[cfg(test)] probe fixture
crates/orchestrators/boss-cli/src/publish.rs	#[cfg(test)] clone-URL fixture
crates/orchestrators/boss-cli/src/queue.rs	#[cfg(test)] assertion that a refusal names the record
crates/orchestrators/boss-cli/src/running.rs	the CLI's BOSS_JOBS_URL default (reads env as before)
crates/orchestrators/boss-cli/src/train/forge.rs	the Forgejo adapter's BOSS_TRAIN_FORGE_URL default (the CLI reads env as before)
crates/orchestrators/boss-cli/src/train/jobs_api.rs	#[cfg(test)] fixture: the transport error a blip classifies
crates/orchestrators/boss-cli/src/train/mod.rs	the conductor Config's BOSS_TRAIN_FORGE_URL default (the CLI reads env as before)
crates/orchestrators/boss-cli/src/train/preflight.rs	refusal prose and #[cfg(test)] fixtures for the forge/adapter mismatch
crates/orchestrators/boss-cli/src/train/red_verdict.rs	#[cfg(test)] fixtures: the forge target_url a red check carries
crates/orchestrators/boss-dispatcher-handlers/src/handlers/estate_compare.rs	#[cfg(test)] estate fixtures
infra/cluster/manifests/boss-audit-integrity.yaml	a CronJob's image: the kubelet pulls it; a manifest reads no host file (the registry as a render parameter is a later car)
infra/cluster/manifests/boss-backup.yaml	CronJob images, as above
infra/cluster/manifests/boss-conservation-invariants.yaml	CronJob image
infra/cluster/manifests/boss-conductor.yaml	the conductor's image and BOSS_TRAIN_FORGE_URL — prod is the source instance; render-instance.sh substitutes only the instance keys
infra/cluster/manifests/boss-dev.yaml	the dev pod's images and forge credential key; editing it rolls the pod
infra/cluster/manifests/boss-files-gc.yaml	CronJob image
infra/cluster/manifests/boss-jobs-internal.yaml	THE MetalLB pin that defines the address; held equal to the source by the_estate_address_lives_once.rs
infra/cluster/manifests/boss-ledger-recognize.yaml	CronJob image
infra/cluster/manifests/boss-ledger-replay-check.yaml	CronJob image
infra/cluster/manifests/boss-messages-events-purge.yaml	CronJob image
infra/cluster/manifests/boss-playground-crawl.yaml	CronJob images and the clone-by-IP a Job needs before it has a checkout or a host file (gate-runner.yaml + run.sh's shape)
infra/cluster/manifests/boss-search-reindex.yaml	CronJob image
infra/cluster/manifests/boss-views-catchup.yaml	CronJob image
infra/cluster/manifests/boss.yaml	the instance's images and BOSS_BROKER_FORGE_URL (prod is the source instance)
infra/cluster/talos/check-declared.sh	names the mirror key the Talos patches declare, to split it
infra/cluster/talos/patches/cp-1.yaml	machine.registries.mirrors — Talos node config, applied by talosctl, reads no host file
infra/cluster/talos/patches/cp-2.yaml	machine.registries.mirrors, as above
infra/cluster/talos/patches/cp-3.yaml	machine.registries.mirrors, as above
infra/cluster/talos/patches/w-1.yaml	machine.registries.mirrors, as above
infra/forge/boss-ci/Dockerfile	FROM lines pull mirrored bases by registry tag; a Dockerfile reads no env
infra/gate-runner/gate-runner-local.yaml	the gate Job's images (a manifest rendered by gate.rs)
infra/gate-runner/gate-runner.yaml	the gate Job's images (a manifest rendered by gate.rs)
infra/gate-runner/run.sh	runs in a cluster Job before it has a checkout or a host file; gate.rs rendering both addresses from the source is the follow-up
infra/lint/ci-checks-out-from-the-forge.sh	the expected checkout line it holds ci.yml to
infra/lint/ci-images-are-pruned-by-age.sh	self-test fixtures for the sweep's image repo
infra/lint/ci-tools-declared.sh	the CI image prefix it strips from ci.yml's container lines
infra/lint/the-build-pulls-only-mirrored-bases.sh	self-test fixtures (Dockerfiles and manifests it plants)
infra/lint/the-converge-rolls-back-to-a-named-build.sh	self-test fixtures for the rollback target
infra/ops/verbs/mirror-base-images.json	the ops verb's prose names the registry the mirror fills
infra/oss-quickstart/Dockerfile	COPY --from a mirrored base by registry tag; a Dockerfile reads no env
infra/platform/workflows/emergency-merge.toml	workflow prose (registry data) naming where an operator reads /api/jobs/health
infra/platform/workflows/ship-a-change.toml	workflow prose (registry data) naming the forge for a builder
EOF

# lines_with <ip> <file> — `line:text` for every line spelling the IP
# as an address (not as a prefix of a longer dotted number). Comment
# lines are prose and are skipped, like docs: a machine reads neither,
# and the address in a comment about 2026-09-03 is history, not a
# target. (A recipe in a comment that names the address is still
# wrong; that is a review's job, and this lint's own count says how
# many such lines remain — see the report on backlog 5222163e.)
lines_with() {
    local esc="${1//./\\.}"
    grep -nE "(^|[^0-9.])${esc}([^0-9]|$)" -- "$2" | grep -vE '^[0-9]+:[[:space:]]*#'
}

# Every tracked file, minus the exclusions the header names.
files=$(git_answer "$LINT" 0 ls-files) || exit $?
problems=0
notes=0
scanned=0
while IFS= read -r f; do
    [ -f "$f" ] || continue
    case "$f" in
        "$SOURCE"|infra/lint/the-estate-address-lives-once.sh) continue ;;
        docs/*|*.md) continue ;;
        crates/*/tests/*|*/tests/*|*.test.ts) continue ;;
        infra/postgres/schema/*) continue ;;
    esac
    scanned=$((scanned + 1))
    reason=$(printf '%s\n' "$ALLOWANCE" | awk -F'\t' -v p="$f" '$1 == p { print $2; exit }')
    hits=$( { lines_with "$sor_ip" "$f"; lines_with "$forge_ip" "$f"; } 2>/dev/null | sort -t: -k1,1n -u)
    if [ -n "$reason" ]; then
        if [ -z "$hits" ]; then
            echo "$LINT: STALE allowance — $f no longer spells either address; remove its entry" >&2
            problems=$((problems + 1))
        fi
        continue
    fi
    if [ -n "$hits" ]; then
        printf '%s\n' "$hits" | sed "s|^|$f:|" >&2
        problems=$((problems + 1))
    fi
done <<< "$files"

# An allowance naming a file that is gone: skipped, and said (see header).
while IFS=$'\t' read -r p _; do
    [ -n "$p" ] || continue
    if [ ! -f "$p" ]; then
        echo "$LINT: note — allowance for $p, which no longer exists; the entry can go"
        notes=$((notes + 1))
    fi
done <<< "$ALLOWANCE"

# The count is a verdict (lib/scanned.sh): a tree of zero files read
# would certify nothing.
lint_scanned "$LINT" "$scanned" "tracked file(s) outside the source, docs, tests and migrations"
if [ "$problems" -ne 0 ]; then
    echo "$LINT: $problems file(s) spell the system of record ($sor_ip) or the forge ($forge_ip) outside $SOURCE." >&2
    echo "    A unit reads /etc/boss/sor.env (EnvironmentFile=); a script sources infra/lib/sor.sh and" >&2
    echo "    names what it needs (sor_require BOSS_JOBS_URL); a forge verb sources forge-defaults.sh." >&2
    echo "    A file that truly cannot read the env file goes in this lint's ALLOWANCE with its reason." >&2
    exit 1
fi
echo "$LINT: clean — $sor_ip and $forge_ip live in $SOURCE ($notes dead allowance note(s))"
