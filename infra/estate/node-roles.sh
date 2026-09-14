#!/usr/bin/env bash
# infra/estate/node-roles.sh — ONE definition of how a managed host reads
# its own roles off the system of record. Sourced by every host converge
# (infra/gcp/boss-gcp-converge.sh, infra/forge/forge-converge.sh); the
# second host to need it is why it is a file and not a second copy
# (CLAUDE.md §9a).
#
# read_node_roles <node-id>
#   Sets and exports BOSS_NODE_ROLES (comma-separated) from
#   /api/estate/nodes, with a short ceiling, and REMEMBERS a successful
#   read in BOSS_NODE_ROLES_CACHE (default /var/lib/boss/node-roles.<id>,
#   beside the host's other state). A system of record that cannot be
#   reached then installs THE LAST DECLARATION THIS HOST HAS EVIDENCE
#   FOR — the cache — and says so; with no cache at all it installs
#   [always] only (the sentinel `registry-unread` matches no roles.toml
#   section), never every row. Until 2026-09-14 a dark registry meant
#   "install every row", defensible while boss-gcp declared every role
#   and a hazard the day it stopped declaring legacy-stack: a roll of
#   the SoR at a converge tick would have re-enabled the ten chores the
#   retire verb had just stopped, and the record would have said the
#   stack was retired (6cd124c4). An arm that needs the patient is not
#   an arm — so the converge keeps going on a dark registry, but never
#   WIDENS what a host runs on a failed read. The caller's packet says
#   which source answered (run_summary_note, when the caller has sourced
#   run-summary.sh). A BOSS_NODE_ROLES already set by the caller wins
#   (a test, a hand run).
#
# has_role <role>
#   True when BOSS_NODE_ROLES names the role. Exact match on the
#   comma-separated list, so `operator` never matches `cluster-operator`.
read_node_roles() { # <node-id>
    local node_id="$1"
    local nodes_url="${BOSS_ESTATE_NODES_URL:-http://10.20.0.34:7900/api/estate/nodes}"
    local prefix="${BOSS_CONVERGE_NAME:-converge}"
    local cache="${BOSS_NODE_ROLES_CACHE:-/var/lib/boss/node-roles.${node_id}}"
    if [ -z "${BOSS_NODE_ROLES+set}" ]; then
        local roles_json
        roles_json="$(curl -fsS --max-time 10 "$nodes_url" 2>/dev/null)" || roles_json=""
        if [ -z "$roles_json" ]; then
            if [ -s "$cache" ]; then
                BOSS_NODE_ROLES="$(tr -d '[:space:]' < "$cache")"
                echo "$prefix: $nodes_url did not answer — installing the cached declaration read $(date -u -r "$cache" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo earlier): $BOSS_NODE_ROLES"
                if declare -F run_summary_note >/dev/null; then
                    run_summary_note "roles: $nodes_url did not answer — cached declaration installed ($BOSS_NODE_ROLES)"
                fi
            else
                BOSS_NODE_ROLES="registry-unread"
                echo "$prefix: $nodes_url did not answer and there is no cached declaration at $cache — installing [always] only, never every row"
                if declare -F run_summary_note >/dev/null; then
                    run_summary_note "roles: $nodes_url did not answer, no cache — [always] only installed"
                fi
            fi
        else
            BOSS_NODE_ROLES="$(printf '%s' "$roles_json" \
                | jq -r --arg id "$node_id" '[.data[] | select(.id == $id) | .roles[]?] | join(",")' 2>/dev/null)" \
                || BOSS_NODE_ROLES=""
            if [ -z "$BOSS_NODE_ROLES" ]; then
                echo "$prefix: $node_id declares no roles in the registry — installing every row"
            else
                echo "$prefix: $node_id declares roles: $BOSS_NODE_ROLES"
            fi
            # Remember what was read, for the next dark tick. Best-effort:
            # a cache that cannot be written costs the fallback, not the
            # converge.
            if mkdir -p "$(dirname "$cache")" 2>/dev/null; then
                printf '%s\n' "$BOSS_NODE_ROLES" > "$cache" 2>/dev/null || true
            fi
        fi
    fi
    export BOSS_NODE_ROLES
}

has_role() { # <role>
    case ",${BOSS_NODE_ROLES:-}," in
        *",$1,"*) return 0 ;;
        *) return 1 ;;
    esac
}
