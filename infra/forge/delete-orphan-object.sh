#!/usr/bin/env bash
#
# delete-orphan-object — delete ONE cluster object the TREE ALREADY
# PROVES is undeclared, and nothing else, ever.
#
# WHY IT EXISTS (backlog a139d5bb, filed urgent 2026-09-10)
# --------------------------------------------------------
# The converge runs `kubectl apply` with no `--prune`, so deleting a
# manifest removes the DECLARATION and leaves the OBJECT running. Until
# this verb, clearing one was a sentence in a packet asking a person to
# type `kubectl -n boss delete svc <name>`, and an agent could not do it
# at all. That hand-off blocked real work: `Service/boss-docs-internal`
# outlived its manifest by nine days, and the orphan lint that finds it
# exits 1 inside the converge — so a gated-green car could not land,
# because landing it would fail every converge after it until a human
# intervened. The car was CORRECT to fail; the gap was that nothing but
# a human could clear what it detected.
#
# WHY NOT A `kubectl delete` VERB
# -------------------------------
# The allowlist's whole value is that every verb is a reviewed word with
# bounded params. "Delete any object by name" hands an agent
# namespace-wide destruction through an audited door, which is worse
# than no door: the audit would record exactly what was destroyed, with
# nothing having refused it. CLAUDE.md is explicit — reads and bounded
# reclaims are mechanical, destructive-by-policy actions are not, and
# keeping that line sharp is what makes handing over the first kind
# safe.
#
# SO THE AUTHORITY IS DERIVED, NOT GRANTED
# ----------------------------------------
# This script cannot delete anything the tree does not already prove is
# undeclared. It re-runs that computation itself, at call time, through
# the ONE definition of it — `infra/cluster/undeclared-objects.sh`, the
# same script the orphan lint reports from — and acts only if that
# computation names the object it was given. Five bounds, in the order
# they are applied:
#
#   1. THE ARGUMENT'S SHAPE. `<Kind>/<namespace>/<name>`, the same
#      pattern the allowlist validated, checked again here so the bound
#      does not depend on one layer (the run-car-probe convention).
#   2. A COMMITTED DECLARATION SET. The manifests directory must be
#      clean in git. An uncommitted edit — a rename in progress, a local
#      experiment — means the tree that proves the object undeclared is
#      not a tree anyone reviewed. This is the answer to "what if a
#      manifest is mid-rename": a rename that is not committed is not a
#      declaration set, and a rename that IS committed has removed the
#      old name on purpose.
#   3. THE DERIVATION. `undeclared-objects.sh --check` must say
#      undeclared. It refuses, by name, for a declared object, a
#      namespace the tree does not own, a kind the tree declares nothing
#      of, an excluded kind, an exempt object, a controller-owned
#      object, and an object that is not live; and it says CANNOT ANSWER
#      — never "undeclared" — when the credential cannot list the pair
#      or a manifest will not parse.
#   4. THE KIND FLOOR below, narrower than the derivation's scope: a
#      kind whose deletion destroys bytes or credentials stays a human
#      step even when the tree proves the object undeclared.
#   5. CAPTURE BEFORE DELETE. The object's own YAML is printed first, so
#      the packet that asked holds what it was. A deleted object cannot
#      be read back, and the ops-request is where the record lands.
#
# The delete is then issued with the kind, namespace and name from the
# DERIVATION's own output line — never from the argument string — so the
# packet's text is only ever compared, never executed against.
#
# `--dry-run` stops after bound 5: the way to ask the audited door
# "would this be allowed?" without acting, and the way the verb is
# exercised live without deleting anything.
#
# USAGE
#   delete-orphan-object.sh <Kind>/<namespace>/<name> [--dry-run]
#
# EXIT
#   0  deleted (or, with --dry-run, would be)
#   2  refused — the reason is named on stderr
#   1  could not answer, or the delete failed
#
# ENV
#   BOSS_CLUSTER_TREE  the tree whose manifests are the declaration set
#                      (default: the repository this script lives in).
#                      A test seam; a packet cannot set it, because the
#                      ops-runner passes no packet-supplied environment
#                      — only an argv built from the allowlist.
#   BOSS_KUBECTL       see undeclared-objects.sh; resolved there, once.
#   KUBECONFIG         a credential that can read the managed namespaces
#                      and delete in them. Under the ops-runner there is
#                      no HOME, so the kubeconfig is named explicitly by
#                      the resolver rather than found at ~/.kube/config.

set -uo pipefail

ME="delete-orphan-object"
say() { echo "$ME: $*" >&2; }
refuse() { say "REFUSED — $*"; exit 2; }

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$SELF_DIR/.." && cd .. && pwd)"
DERIVE="$REPO/infra/cluster/undeclared-objects.sh"
TREE="${BOSS_CLUSTER_TREE:-$REPO}"
MANIFEST_REL="infra/cluster/manifests"

# --- bound 1: the argument's shape -----------------------------------------
TARGET="${1:-}"
MODE="${2:-}"
if [ -z "$TARGET" ] || [ "$#" -gt 2 ]; then
    say "usage: $ME <Kind>/<namespace>/<name> [--dry-run]"
    say "  One object, named in full. There is no form of this verb that takes a selector,"
    say "  a namespace on its own, or more than one object."
    exit 2
fi
DRY=0
case "$MODE" in
    "") ;;
    --dry-run) DRY=1 ;;
    *) refuse "the only second argument is --dry-run, not \`$MODE\`" ;;
esac
# The same pattern the allowlist applies, applied again here: a bound
# that exists in one layer only is a bound that disappears the day
# something else calls the script.
if ! [[ "$TARGET" =~ ^[A-Z][A-Za-z0-9]{0,62}/[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?/[a-z0-9]([a-z0-9.-]{0,251}[a-z0-9])?$ ]]; then
    refuse "\`$TARGET\` is not <Kind>/<namespace>/<name> — Kind capitalised, namespace and name DNS labels"
fi

[ -x "$DERIVE" ] || refuse "the derivation $DERIVE is missing or not executable, so there is no authority to act on"

TMP=$(mktemp -d) || exit 1
trap 'rm -rf "$TMP"' EXIT

# --- EVERY git CALL RUNS AS THE CHECKOUT'S OWNER, READS INCLUDED -----------
#
# Measured on the forge, 2026-09-10 (ops-request c9877f75): the first live
# run through the audited door refused with "/home/david/boss is not a git
# checkout". It is one — forge-converge.sh fetches into it every tick. What
# failed is that the ops-runner executes verbs AS ROOT, and since git
# 2.35.2 a repository owned by somebody else is "dubious ownership" and
# every command refuses. So a bound that was correct was also
# unsatisfiable, and the only thing that could satisfy it was a human.
#
# Two reasons to drop, and the read is only the first: root git WRITES in a
# user-owned clone leave root-owned objects that break the owner's later
# pulls (forge-converge.sh says so eight lines from where it avoids it).
# This script only reads, but silencing the ownership check with
# safe.directory would buy the read at the price of making that second
# hazard reachable by the next edit. Drop instead.
#
# The helper is infra/gcp/boss-gcp-converge.sh's `as_owner`, for the same
# hazard on the same shape of host; the invocation is the probe runner's
# non-login `runuser -u <user> --` with an explicit HOME
# (run-car-probe.sh), because these are reads that need no credential
# helper and a login shell's profile output would be captured as part of a
# `rev-parse`. The owner is READ OFF THE DIRECTORY, never hardcoded, so a
# host that lands the checkout under a different account does not silently
# start failing — or, worse, corrupting.
case "$TREE" in
    *[[:space:]\'\"]*) refuse "the tree path \`$TREE\` contains whitespace or a quote; name one without" ;;
esac
[ -d "$TREE" ] || refuse "the tree $TREE does not exist, so there is no declaration set to read"
OWNER="${BOSS_CLUSTER_TREE_OWNER:-$(stat -c %U "$TREE" 2>/dev/null)}"
OWNER_UID="$(stat -c %u "$TREE" 2>/dev/null)"
# `stat -c %U` prints UNKNOWN when no passwd entry exists for the owning
# uid — a sibling car learned that the hard way. Proceeding would run git
# as the caller again, which is the defect this block exists for, so
# refuse and name what could not be resolved.
if [ -z "$OWNER" ] || [ "$OWNER" = "UNKNOWN" ]; then
    refuse "cannot resolve the owner of $TREE (stat says '${OWNER:-}', uid ${OWNER_UID:-?}) — no passwd entry, so there is no account to read git as"
fi
if ! id -u "$OWNER" >/dev/null 2>&1; then
    refuse "the owner of $TREE is '$OWNER' (uid ${OWNER_UID:-?}), which is not an account on this host — refusing to read its git as $(id -un)"
fi
OWNER_HOME="$(getent passwd "$OWNER" | cut -d: -f6)"
as_owner() { # <command string>
    if [ "$(id -un)" = "$OWNER" ]; then
        bash -c "$1"
    else
        runuser -u "$OWNER" -- env HOME="${OWNER_HOME:-/}" PATH="$PATH" bash -c "$1"
    fi
}

# --- bound 2: a committed declaration set ----------------------------------
if ! as_owner "git -C '$TREE' rev-parse --git-dir" >/dev/null 2>"$TMP/git.err"; then
    say "REFUSED — cannot read $TREE as a git checkout (read as '$OWNER'), so the declaration set"
    say "  is not reviewable content. git said:"
    sed 's/^/    /' "$TMP/git.err" >&2
    say "  If that mentions dubious ownership, the read did NOT drop to the owner — which is"
    say "  the defect ops-request c9877f75 found, and what as_owner in this script prevents."
    exit 2
fi
dirty=$(as_owner "git -C '$TREE' status --porcelain -- '$MANIFEST_REL'" 2>/dev/null)
if [ -n "$dirty" ]; then
    say "REFUSED — $MANIFEST_REL has uncommitted changes, so the tree that would prove this object"
    say "  undeclared is not one anybody reviewed. A rename in progress is exactly this state:"
    printf '    %s\n' "$dirty" >&2
    say "  Commit or discard them, then ask again."
    exit 2
fi
# Read once, reported twice: the sha the declaration set came from.
HEAD_SHA=$(as_owner "git -C '$TREE' rev-parse --short HEAD" 2>/dev/null)

# --- bound 3: the derivation -----------------------------------------------
say "deriving authority from $MANIFEST_REL at ${HEAD_SHA:-?} (owner $OWNER) via ${DERIVE#"$REPO"/}"
BOSS_CLUSTER_TREE="$TREE" "$DERIVE" --check "$TARGET" >"$TMP/check.out" 2>"$TMP/check.err"
rc=$?
# The derivation's own words, always — on every path. A refusal whose
# reason somebody has to go re-derive is not a verdict.
cat "$TMP/check.err" >&2
case "$rc" in
    0) ;;
    3) say "REFUSED — the derivation does not name $TARGET as undeclared (see its reason above)."; exit 2 ;;
    *) say "CANNOT ANSWER — the derivation could not decide (exit $rc). Nothing deleted."; exit 1 ;;
esac

IFS=$'\t' read -r verdict KIND NS NAME < "$TMP/check.out"
if [ "${verdict:-}" != "undeclared" ] || [ -z "${KIND:-}" ] || [ -z "${NS:-}" ] || [ -z "${NAME:-}" ]; then
    say "CANNOT ANSWER — the derivation exited 0 but its answer did not parse: $(cat "$TMP/check.out")"
    exit 1
fi
# Belt and braces: the derivation answered about the object that was
# asked about. The delete below uses ITS fields, so this compares the two
# spellings rather than trusting either alone.
if [ "$KIND/$NS/$NAME" != "$TARGET" ]; then
    say "CANNOT ANSWER — the derivation answered about $KIND/$NS/$NAME, not $TARGET"
    exit 1
fi
printf '%s\n' "undeclared	$KIND	$NS	$NAME"

# --- bound 4: the kind floor ----------------------------------------------
# Narrower than the derivation's scope, and deliberately so. The
# derivation proves "the tree does not declare this"; it cannot prove
# "deleting this destroys nothing that cannot be rebuilt". These kinds
# are the ones where reapplying a manifest restores the object
# completely:
#
#   Service, ConfigMap, Deployment, CronJob, Job — configuration and
#   workloads. Deleting an undeclared one removes something the converge
#   does not manage and never did; the cluster's own controllers rebuild
#   nothing that was not declared.
#
# NOT on this list, each for its reason — these stay a named human step:
#   PersistentVolumeClaim, StatefulSet — bytes on Longhorn, including
#     the volumes holding the audit log and the system of record. A
#     deletion here is data loss, which no derivation makes reversible.
#   Secret — credential material, and already out of the derivation's
#     scope (the tree references secrets by name and creates them out of
#     band, so its declaration set for them is incomplete by design).
#   ServiceAccount — deleting one invalidates the tokens issued from it.
#   Role, RoleBinding — deleting a privilege can remove a live
#     capability nobody wrote down, which is exactly the shape of the
#     2026-08-28 dev-session grant (95f6aba5). Loud is better than
#     automatic here.
#   Everything cluster-scoped — out of the derivation's scope entirely.
#
# Widening this list is a PR against this file, reviewed like the
# allowlist entry itself. That is the point: the floor is a reviewed
# word, not a runtime decision.
DELETABLE_KINDS=(Service ConfigMap Deployment CronJob Job)
in_floor=0
for k in "${DELETABLE_KINDS[@]}"; do
    [ "$k" = "$KIND" ] && in_floor=1
done
if [ "$in_floor" -ne 1 ]; then
    say "REFUSED — $KIND is undeclared, and still not a kind this verb deletes."
    say "  This verb deletes: ${DELETABLE_KINDS[*]} — kinds a manifest reapply restores completely."
    say "  $KIND is withheld because its deletion destroys bytes or privileges that no derivation"
    say "  makes reversible (see DELETABLE_KINDS in $ME for the per-kind reason). That delete stays"
    say "  a named human step; widening the floor is a reviewed change to this file."
    exit 2
fi

# --- the kubectl, resolved once, by the derivation -------------------------
KUBECTL_LINE=$(BOSS_CLUSTER_TREE="$TREE" "$DERIVE" --kubectl) || {
    say "CANNOT ANSWER — no kubectl to act with (see above). Nothing deleted."
    exit 1
}
read -r -a KUBECTL <<<"$KUBECTL_LINE"

# --- bound 5: capture before delete ---------------------------------------
# A deleted object cannot be read back, and this packet is where the
# record lands. If the capture fails, the delete does not happen: a
# destructive step whose evidence failed first is one nobody can review
# afterwards.
if ! "${KUBECTL[@]}" get "$KIND" "$NAME" -n "$NS" -o yaml --request-timeout=30s > "$TMP/object.yaml" 2> "$TMP/get.err"; then
    say "CANNOT ANSWER — could not read $KIND/$NS/$NAME to record what it was:"
    sed 's/^/    /' "$TMP/get.err" >&2
    say "  Nothing deleted: the capture is the only copy there will be."
    exit 1
fi
echo "--- $KIND/$NS/$NAME as it was, before the delete ---"
cat "$TMP/object.yaml"
echo "--- end of $KIND/$NS/$NAME ---"

if [ "$DRY" -eq 1 ]; then
    say "DRY RUN — would delete $KIND \`$NAME\` in \`$NS\`. Every bound passed; nothing was deleted."
    exit 0
fi

# --- the delete -----------------------------------------------------------
say "deleting $KIND \`$NAME\` in \`$NS\` — undeclared by ${MANIFEST_REL} at ${HEAD_SHA:-?}"
if ! "${KUBECTL[@]}" delete "$KIND" "$NAME" -n "$NS" --request-timeout=60s > "$TMP/del.out" 2>&1; then
    cat "$TMP/del.out" >&2
    say "the delete FAILED. The object is still there as far as this run knows."
    exit 1
fi
cat "$TMP/del.out"

# Verified gone, because "the command exited 0" and "the object is gone"
# are different claims.
if "${KUBECTL[@]}" get "$KIND" "$NAME" -n "$NS" --request-timeout=10s >/dev/null 2>&1; then
    say "the delete returned success and $KIND/$NS/$NAME is STILL THERE — something is recreating it."
    say "  Look for a controller or a hand-run apply; this verb will not try again."
    exit 1
fi
say "OK — $KIND \`$NAME\` is gone from \`$NS\`, and no manifest declared it."
exit 0
