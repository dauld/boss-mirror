#!/usr/bin/env bash
# infra/estate/node-roles.sh — ONE definition of how a managed host reads
# its own roles off the system of record. Sourced by every host converge
# (infra/gcp/boss-gcp-converge.sh, infra/forge/forge-converge.sh); the
# second host to need it is why it is a file and not a second copy
# (CLAUDE.md §9a).
#
# read_node_roles <node-id>
#   Sets and exports BOSS_NODE_ROLES (comma-separated) from
#   /api/estate/nodes. BEST-EFFORT with a short ceiling: a system of
#   record that cannot be reached leaves the roles EMPTY, the installer
#   then installs every row exactly as it did before roles existed, and
#   the caller's packet says so (run_summary_note, when the caller has
#   sourced run-summary.sh). An arm that needs the patient is not an
#   arm — a converge's job is to keep the host current whether or not
#   the registry answers. A BOSS_NODE_ROLES already set by the caller
#   wins (a test, a hand run).
#
# has_role <role>
#   True when BOSS_NODE_ROLES names the role. Exact match on the
#   comma-separated list, so `operator` never matches `cluster-operator`.
read_node_roles() { # <node-id>
    local node_id="$1"
    local nodes_url="${BOSS_ESTATE_NODES_URL:-http://10.20.0.34:7900/api/estate/nodes}"
    local prefix="${BOSS_CONVERGE_NAME:-converge}"
    if [ -z "${BOSS_NODE_ROLES+set}" ]; then
        local roles_json
        roles_json="$(curl -fsS --max-time 10 "$nodes_url" 2>/dev/null)" || roles_json=""
        if [ -z "$roles_json" ]; then
            echo "$prefix: $nodes_url did not answer — roles unknown, installing every row"
            if declare -F run_summary_note >/dev/null; then
                run_summary_note "roles: $nodes_url did not answer — every row installed"
            fi
            BOSS_NODE_ROLES=""
        else
            BOSS_NODE_ROLES="$(printf '%s' "$roles_json" \
                | jq -r --arg id "$node_id" '[.data[] | select(.id == $id) | .roles[]?] | join(",")' 2>/dev/null)" \
                || BOSS_NODE_ROLES=""
            if [ -z "$BOSS_NODE_ROLES" ]; then
                echo "$prefix: $node_id declares no roles in the registry — installing every row"
            else
                echo "$prefix: $node_id declares roles: $BOSS_NODE_ROLES"
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
