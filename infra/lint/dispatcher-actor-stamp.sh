#!/usr/bin/env bash
# dispatcher-actor-stamp — every downstream call a dispatcher handler
# makes must carry the rule-as-actor `x-boss-user` header.
#
# Why this exists. `products.produce` read the ledger with a raw
# `client.post(&url)` and no header. That was harmless for as long as
# `/api/ledger/*` was ungated, and became a hard 403 the moment it was
# — which stopped the WIP→FG cost transfer, so WIP accumulated with
# ZERO credits ($4.9M and climbing), finished goods were never
# produced, and the invoice-consume handler then failed too for want
# of stock. One missing header, three broken invariants, and nothing
# in CI could see it: the handler's own tests mock the downstream
# service, so they pass whether or not the call is authenticated.
#
# The rule is structural, which is why it belongs here rather than in
# a test: an unauthenticated internal call is a latent outage that
# fires when somebody else adds a gate, possibly months later.
#
# Enforcement shape mirrors the other ratchets: grep, explicit
# allow-list, non-zero exit on anything new.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

HANDLERS=crates/orchestrators/boss-dispatcher-handlers/src/handlers
# The assignment dispatcher lives in the core crate and carries its
# identity as a reqwest DEFAULT header, so it has no `.header(...)`
# line to grep for. That is exactly how it stayed invisible while its
# writes landed on simulated Jobs marked real.
ASSIGNMENT=crates/core/boss-dispatcher/src

# Files whose calls legitimately carry no actor. Keep this empty
# unless there is a recorded reason — a public webhook to a third
# party has no BOSS actor to present, for example.
#
#   webhook_notify.rs — outbound to a counterparty's URL, not a BOSS
#   service: there is no internal identity to stamp.
#
#   credential_issuer.rs — the credential broker's EXTERNAL adapters
#   (7ee101aa): the forge's admin API (authenticated by the broker
#   root token) and the cluster API server (authenticated by the pod
#   ServiceAccount bearer). Neither is a BOSS service, so there is no
#   actor or sim-origin to present. The broker HANDLER's jobs-api
#   calls live in credential_rotate_forgejo.rs, which stays covered.
ALLOW="webhook_notify.rs credential_issuer.rs"

failures=""

# Walk BOTH trees recursively, never one directory of them. The
# assignment path was added here after it leaked sim-origin, and a
# non-recursive glob read the top of `boss-dispatcher/src` only — so
# `src/rules/jobs_spawn.rs` stayed invisible and leaked the same
# header for the same reason, on 55 events across three kinds. A
# check that covers one directory of a tree reports "ok" for the
# rest of it.
while IFS= read -r path; do
    skip=0
    for allowed in $ALLOW; do
        if [ "$(basename "$path")" = "$allowed" ]; then skip=1; fi
    done
    if [ "$skip" -eq 1 ]; then continue; fi

    # `.post(&url)` / `.get(&url)` — a request aimed at a URL variable.
    # The builder chain runs until `.send()`; the headers must appear
    # somewhere in it (windowed to 20 lines, like the chains are).
    # The test module is dropped first: mocks are not the production
    # call path. Identity may ride on the client's default headers
    # (the assignment dispatcher does that), but sim-ness cannot: it
    # belongs to the event being handled, not to the client.
    out=$(awk -v FILE="$path" '
        {
            line[NR] = $0
            if (index($0, "default_headers") > 0) has_default_headers = 1
        }
        END {
            cut = NR
            for (i = 1; i <= NR; i++)
                if (index(line[i], "#[cfg(test)]") > 0) { cut = i - 1; break }
            for (i = 1; i <= cut; i++) {
                if (line[i] !~ /\.(post|get)\([[:space:]]*&?[a-z_]*url([^a-zA-Z0-9_]|$)/)
                    continue
                chunk = ""
                end = i + 19
                if (end > cut) end = cut
                for (j = i; j <= end; j++) {
                    chunk = chunk line[j] "\n"
                    if (index(line[j], ".send()") > 0) break
                }
                stripped = line[i]
                gsub(/^[[:space:]]+/, "", stripped)
                gsub(/[[:space:]]+$/, "", stripped)
                if (index(chunk, "x-sim-origin") == 0)
                    printf "%s:%d: no x-sim-origin — %s\n", FILE, i, stripped
                else if (index(chunk, "x-boss-user") == 0 && !has_default_headers)
                    printf "%s:%d: no actor — %s\n", FILE, i, stripped
            }
        }' "$path")
    if [ -n "$out" ]; then
        failures="${failures}${out}
"
    fi
done < <({ find "$HANDLERS" -name '*.rs' | LC_ALL=C sort
           find "$ASSIGNMENT" -name '*.rs' | LC_ALL=C sort; })

if [ -n "$failures" ]; then
    echo "FAIL — dispatcher calls missing an actor or sim-origin header:"
    printf '%s' "$failures" | sed 's/^/  /'
    echo
    echo 'Stamp it: .header("x-boss-user", dispatcher_actor_header(rule_name))'
    echo "or use common::post_json, which does it for you."
    exit 1
fi

echo "ok: every dispatcher downstream call stamps its actor and its origin"
