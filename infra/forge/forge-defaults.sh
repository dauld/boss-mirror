# forge-defaults.sh — the defaults every forge verb used to carry alone.
#
# Source it after `set -u`, then name the image repos the verb reads:
#
#   . "$(dirname "$0")/forge-defaults.sh"
#   forge_need REGISTRY            # and/or CI_IMAGE_REPO, REGISTRY_BASE
#
# WHY ONE FILE. The audit of 2026-09-18 (§1 "forge script defaults",
# backlog 5222163e) counted the image repo the converge pushes in four
# verbs, the converge's stamp file in three, the hold file in two and
# the postgres target in four — each a literal, three of them pinned
# one-way by a test and two by nothing. A fact that lives twice gets an
# equality test; a fact that lives once needs none (CLAUDE.md §9a). This
# is the infra/files-root.sh pattern: the value is defined here, the
# verbs read it, and a verb that needs a different one sets the variable
# before sourcing.
#
# THE REGISTRY HOST comes from /etc/boss/sor.env through infra/lib/sor.sh
# (the one tree source is infra/estate/estate.toml). A verb that runs
# where that file is absent — a test, a workstation — names its image
# repo explicitly (BOSS_FORGE_REGISTRY, BOSS_CI_IMAGE_REPO,
# BOSS_FORGE_REGISTRY_BASE) or is refused by name when it asks; there is
# no literal to fall back on. The image repos are resolved on demand
# (`forge_need`) rather than at source time, so a verb that never
# touches the registry — converge-hold, the census — is never refused
# for want of it.
#
# EVERY NAME BELOW KEEPS THE OVERRIDE IT HAD: the tests and the units
# that set BOSS_FORGE_REGISTRY, REGISTRY, BOSS_FORGE_LAST_BUILT,
# BOSS_CONVERGE_HOLD and the rest still win.

. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/sor.sh"

# The forge user whose repositories hold every image.
FORGE_OWNER="${BOSS_FORGE_OWNER:-david}"

# forge_need NAME... — resolve an image repo from its override, else
# from the env file's registry host; refuse by name when neither says.
#
#   REGISTRY       the image the converge pushes and the watchdog /
#                  rollback roll to. `REGISTRY` is the watchdog's and
#                  rollback-to's spelling of the override; the converge's,
#                  the sweep's and the prune's is BOSS_FORGE_REGISTRY.
#                  Either wins; both are set on return.
#   CI_IMAGE_REPO  the CI runner image (one tag per train, pruned by
#                  the sweep); override BOSS_CI_IMAGE_REPO.
#   REGISTRY_BASE  the owner's namespace on the registry — where the
#                  mirrored bases live and where build.sh pushes the
#                  runner; override BOSS_FORGE_REGISTRY_BASE (mirror) or
#                  BOSS_CI_REGISTRY (build.sh).
forge_need() {
    local n
    for n in "$@"; do
        case "$n" in
            REGISTRY)
                if [ -n "${BOSS_FORGE_REGISTRY:-}" ]; then
                    REGISTRY="$BOSS_FORGE_REGISTRY"
                elif [ -z "${REGISTRY:-}" ]; then
                    sor_require BOSS_FORGE_REGISTRY_HOST
                    REGISTRY="$BOSS_FORGE_REGISTRY_HOST/$FORGE_OWNER/boss"
                fi
                BOSS_FORGE_REGISTRY="$REGISTRY" ;;
            CI_IMAGE_REPO)
                if [ -z "${BOSS_CI_IMAGE_REPO:-}" ]; then
                    sor_require BOSS_FORGE_REGISTRY_HOST
                    BOSS_CI_IMAGE_REPO="$BOSS_FORGE_REGISTRY_HOST/$FORGE_OWNER/boss-ci"
                fi
                CI_IMAGE_REPO="$BOSS_CI_IMAGE_REPO" ;;
            REGISTRY_BASE)
                if [ -n "${BOSS_FORGE_REGISTRY_BASE:-}" ]; then
                    REGISTRY_BASE="$BOSS_FORGE_REGISTRY_BASE"
                elif [ -n "${BOSS_CI_REGISTRY:-}" ]; then
                    REGISTRY_BASE="$BOSS_CI_REGISTRY"
                else
                    sor_require BOSS_FORGE_REGISTRY_HOST
                    REGISTRY_BASE="$BOSS_FORGE_REGISTRY_HOST/$FORGE_OWNER"
                fi ;;
            *)
                echo "forge-defaults: no default named $n" >&2; exit 1 ;;
        esac
    done
}

# The converge's stamp — the last build it rolled and verified — and the
# quarantine stamp for a head whose boot failed. Under $HOME by default
# (the converge runs as david; a verb under the ops runner has no HOME
# and names the file, as prune-registry-versions.sh does from the
# checkout's owner).
LAST_BUILT_NAME=".boss-last-built"
STAMP_FILE="${BOSS_FORGE_LAST_BUILT:-${HOME:-}/$LAST_BUILT_NAME}"
FAILED_FILE="${BOSS_FORGE_LAST_FAILED:-${HOME:-}/.boss-last-failed}"

# An operator's hand on the converge (converge-hold.sh). A FIXED path,
# not $HOME: the ops runner executes verbs as root with no HOME in its
# environment (2026-09-05: "HOME: unbound variable" on the first
# release-converge), and the converge reads the hold as david — two
# homes would be two files. /var/tmp is writable by both and survives
# a reboot.
HOLD_FILE="${BOSS_CONVERGE_HOLD:-/var/tmp/boss-converge-hold}"

# The instance database, as infra/cluster/manifests/boss.yaml declares
# it: the StatefulSet `postgres`, container `postgres`, POSTGRES_USER=
# boss. The namespace is each verb's argument; the database is the
# instance Secret's. tenant_census_sh.rs and switch_instance_database_
# sh.rs hold these words equal to the manifest (§9a).
PG_WORKLOAD="sts/postgres"
PG_CONTAINER="postgres"
PG_USER="boss"
