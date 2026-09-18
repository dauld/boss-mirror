# sor.sh — the system of record's address, and the forge's, for shell.
#
# Source it, then name what you read:
#
#   . "$(dirname "$0")/../lib/sor.sh"
#   sor_require BOSS_JOBS_URL
#
# WHERE THE VALUES COME FROM. /etc/boss/sor.env — rendered on every
# managed host by its install from the ONE tree source,
# infra/estate/estate.toml (infra/estate/render-sor-env.sh; the forge in
# infra/forge/install.sh, boss-gcp in infra/gcp/boss-gcp-converge.sh).
#
# WHICH WINS. With no file named, the environment outranks the default
# file: a systemd unit that carries `EnvironmentFile=/etc/boss/sor.env`
# has already loaded it (the same values), and a test that points a
# script at a stub server sets the variable itself. A value the file
# would set is only set when nothing set it first.
#
# A file NAMED with BOSS_SOR_ENV is the source, and REPLACES what the
# environment had. Naming it is the caller saying "this is the address":
# a converge names the file it has just rendered, while its own unit
# loaded the PREVIOUS one at start — under fill-only-unset, the tick
# that moves the address would still read the old one; and a lint names
# its scratch render, while the process around it may carry an address
# that is nobody's expected answer — the conductor pod runs the consist
# check with BOSS_JOBS_URL=http://boss-jobs-internal…:7900 and no
# /etc/boss/sor.env, and on 2026-09-18 that refused a car because the
# installer reported the pod's address instead of the file's.
#
# THERE IS NO FALLBACK. Until 2026-09-18 every script that read the
# address carried `${JOBS_API:-http://10.20.0.34:7900}` — a literal in
# 47 files, three spellings (backlog 5222163e, audit H10) — and the
# forge's IP in 62. A fallback is how a wrong target answers instead of
# erroring (CLAUDE.md §Doors): a script that guessed the address kept
# working on the day the address moved, against the wrong instance, and
# reported success. So `sor_require` REFUSES, naming the variable, the
# file, and the install that should have rendered it.
#
# BOSS_SOR_ENV names another file (a converge's fresh render, a test's
# scratch one); the path is the only knob.

SOR_ENV="${BOSS_SOR_ENV:-/etc/boss/sor.env}"

if [ -r "$SOR_ENV" ]; then
    while IFS= read -r _sor_line || [ -n "$_sor_line" ]; do
        case "$_sor_line" in ''|'#'*) continue ;; esac
        _sor_key="${_sor_line%%=*}"
        _sor_val="${_sor_line#*=}"
        case "$_sor_key" in *[!A-Z0-9_]*|'') continue ;; esac
        if [ -n "${BOSS_SOR_ENV:-}" ] || [ -z "${!_sor_key:-}" ]; then
            export "$_sor_key=$_sor_val"
        fi
    done < "$SOR_ENV"
    unset _sor_line _sor_key _sor_val
fi

# sor_require NAME... — exit 1, naming the first NAME still unset.
sor_require() {
    local n
    for n in "$@"; do
        if [ -z "${!n:-}" ]; then
            {
                echo "$(basename "${0:-sor.sh}"): $n is not set, and there is no fallback address."
                if [ -r "$SOR_ENV" ]; then
                    echo "    $SOR_ENV exists but carries no $n= line."
                else
                    echo "    $SOR_ENV is absent: this host's install has not rendered it."
                fi
                echo "    It is rendered from infra/estate/estate.toml by infra/estate/render-sor-env.sh —"
                echo "    the forge's infra/forge/install.sh (forge-converge, every 10 min) and boss-gcp's"
                echo "    infra/gcp/boss-gcp-converge.sh write it; a unit reads it with EnvironmentFile=."
                echo "    A wrong target answers instead of erroring, so this refuses (CLAUDE.md §Doors)."
            } >&2
            exit 1
        fi
    done
}
