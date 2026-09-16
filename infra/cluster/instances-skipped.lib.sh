# instances-skipped.lib.sh — ONE parser for the converge's
# `instances_skipped` string. Sourced, never run.
#
# The cluster converge (infra/forge/cluster-deploy-runner.sh) skips an
# instance whose Secrets are not minted and records the fact on its
# packet as
#
#   instances_skipped: <ns> (<reason>: <detail>)[; <ns> (<reason>: …)]
#
# e.g. `boss-playground (secrets absent: boss-secrets, boss-tls)`. That
# string is the one definition of "what this converge skipped", and it
# is handed verbatim, as BOSS_INSTANCES_SKIPPED, to every later reader:
# check-manifests-applied.sh (a skipped instance's absent objects count
# `skipped`, not `missing` — backlog 07d7549c) and, since 40d46042,
# render-tunnel-config.sh (a skipped instance's hostname is served by
# the source instance's gateway until it is provisioned). Two readers
# of one format each carried their own parse until the second arrived;
# a third copy is what CLAUDE.md §9a bans, so the parse lives here.
#
# skip_reason NS — prints the reason the converge gave for skipping NS
# (the text before the first colon inside its parentheses), or nothing
# when NS is not named. A namespace is matched whole, so `boss` never
# matches `boss-x`. Reads BOSS_INSTANCES_SKIPPED; unset or empty means
# nothing was skipped.
skip_reason() {
    local entry rest
    [ -n "${BOSS_INSTANCES_SKIPPED:-}" ] || return 0
    while IFS= read -r -d ';' entry; do
        entry="${entry# }"
        [ "${entry%% *}" = "$1" ] || continue
        rest="${entry#* (}"; rest="${rest%%)*}"
        printf '%s\n' "${rest%%:*}"
        return 0
    done <<< "$BOSS_INSTANCES_SKIPPED;"
}
