#!/usr/bin/env bash
#
# pod-logs — READ-ONLY: an instance pod's log, verdict first, through
# the deploy runner's kubectl.
#
#   pod-logs.sh <namespace> <deployment> <lines> [container]
#
# WHY IT EXISTS (backlog c09cab0b, filed urgent 2026-09-18)
# ---------------------------------------------------------
# Measured 2026-09-18 02:40Z: the playground (boss-playground, fresh
# database since 00:58Z) showed 42 product rules active, 0
# tenant:brewery rules and 0 workflow-design packets — so `boss tenant
# publish examples/brewery` either never ran at boot or stopped early.
# The launcher says which, loudly, in the POD LOG — and no door reads
# one: the dev pod's ServiceAccount cannot read pods or logs in
# boss-playground, `journal-tail` is systemd only, and the converge
# record carries only prod's roll. A degraded instance was invisible
# from every door. This verb is that door: the same kubectl +
# kubeconfig resolution the deploy runner, delete-orphan-object and
# tenant-census use (infra/cluster/undeclared-objects.sh --kubectl,
# resolved once), reading only.
#
# WHAT IT PRINTS — verdict first, then evidence
# ---------------------------------------------
#   one line per pod of the deployment:
#     <pod> phase=<phase> ready=<n>/<m> restarts=<sum> age=<age> [state=<reason>]
#   (`state=` only when a container is not running — CrashLoopBackOff,
#   Error, ImagePullBackOff, Completed — so the crashing pod LOOKS
#   troubled on its own line, CLAUDE.md §Diagnosis); a deployment with
#   no pods says `no pods` as its verdict, with the selector it used.
#   Then, per pod:
#     --- <pod> <container|all-containers> (last N lines) ---
#     kubectl logs --tail=N --prefix [--all-containers | -c <container>]
#   and, for a pod whose container is in CrashLoopBackOff or Error,
#   additionally the PREVIOUS container's last 60 lines:
#     --- <pod> <container> previous (last 60 lines) ---
#   because the words that explain a crash loop were printed by the
#   container that died, not the one now waiting.
#
# BOUNDS — each refused loudly, by name, before any kubectl call
#   namespace   one of instances.toml's namespaces, DERIVED through the
#               renderer (`infra/cluster/render-instance.sh --instances`,
#               second column) — never a list typed here (§9a). boss-dev
#               and every system namespace are outside it.
#   deployment  a plain DNS label, ^[a-z][a-z0-9-]{0,62}$ (re-checked
#               here, not only in the allowlist — the run-car-probe
#               convention)
#   lines       1..400. The ops-runner caps a verb's output at 100 KB,
#               so 400 lines × pods × containers may be cut at the tail
#               of the packet; the verdict lines are first and survive.
#   container   optional, the same label pattern; narrows every tail
#               (and the previous-tail) to that container.
#
# SECRETS. This script reads no Secret, no env, no ConfigMap — pods
# (their status) and their logs, nothing else. But a log line is what
# the process printed: if a process logs a credential, the line is the
# line. THE CALLER READS IT before quoting a tail anywhere wider than
# the ops-request packet it lands on.
#
# EXIT
#   0  the verdict and tails are on stdout
#   2  refused — a bound above; stderr names it; nothing on stdout
#   4  cannot answer — no kubectl, or the deployment/pods read failed;
#      stderr carries kubectl's own words; nothing on stdout. A missing
#      deployment is exit 4, never "no pods": a wrong name must not
#      answer like an empty one (CLAUDE.md §Doors).
#
# ENV
#   BOSS_KUBECTL   see undeclared-objects.sh; resolved there, once. The
#                  test seam (a stub kubectl).
#   KUBECONFIG     a credential that can read pods and logs in the
#                  instance namespaces.
# The ops runner passes no HOME; nothing here needs one.

set -uo pipefail

ME="pod-logs"
say() { echo "$ME: $*" >&2; }
refuse() { say "REFUSED — $*"; exit 2; }
CANNOT_ANSWER=4
PREVIOUS_LINES=60
MAX_LINES=400
LABEL='^[a-z][a-z0-9-]{0,62}$'

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$SELF_DIR/../.." && pwd)"
RESOLVE="$REPO/infra/cluster/undeclared-objects.sh"
RENDER="$REPO/infra/cluster/render-instance.sh"

# --- the arguments' shape --------------------------------------------------
[ $# -ge 3 ] && [ $# -le 4 ] \
    || refuse "usage: $ME <namespace> <deployment> <lines 1..$MAX_LINES> [container] — got $# argument(s)"
NS="$1"; DEPLOY="$2"; LINES="$3"; CONTAINER="${4:-}"

[[ "$NS" =~ $LABEL ]] || refuse "the namespace '$NS' is not a plain DNS label"
[[ "$DEPLOY" =~ $LABEL ]] || refuse "the deployment '$DEPLOY' is not a plain DNS label (a bare name, not deploy/<name>)"
case "$LINES" in
    ''|*[!0-9]*) refuse "lines must be a number 1..$MAX_LINES, got '$LINES'" ;;
esac
[ "$LINES" -ge 1 ] && [ "$LINES" -le "$MAX_LINES" ] \
    || refuse "lines must be 1..$MAX_LINES, got $LINES (the runner caps output at 100 KB)"
if [ -n "$CONTAINER" ]; then
    [[ "$CONTAINER" =~ $LABEL ]] || refuse "the container '$CONTAINER' is not a plain DNS label"
fi

# --- the namespace, against instances.toml through the renderer ------------
INSTANCES=$("$RENDER" --instances) || {
    say "CANNOT ANSWER — infra/cluster/instances.toml would not render (see above), so the namespace bound cannot be read"
    exit "$CANNOT_ANSWER"
}
ALLOWED=$(printf '%s\n' "$INSTANCES" | cut -f2)
ok=no
while IFS= read -r ns; do
    [ "$ns" = "$NS" ] && ok=yes
done <<<"$ALLOWED"
[ "$ok" = yes ] || refuse "the namespace '$NS' is not an instance in infra/cluster/instances.toml; this verb reads $(printf '%s' "$ALLOWED" | tr '\n' ' ' | sed 's/ $//' | sed 's/ /, /g')"

command -v jq >/dev/null 2>&1 || { say "CANNOT ANSWER — jq is not on PATH"; exit "$CANNOT_ANSWER"; }

# --- the kubectl, resolved once, by the derivation the delete verb uses --
KUBECTL_LINE=$("$RESOLVE" --kubectl) || {
    say "CANNOT ANSWER — no kubectl to read with (see above). Nothing read."
    exit "$CANNOT_ANSWER"
}
read -r -a KUBECTL <<<"$KUBECTL_LINE"
K=("${KUBECTL[@]}" -n "$NS")

TMP=$(mktemp -d) || exit "$CANNOT_ANSWER"
trap 'rm -rf "$TMP"' EXIT

# --- the deployment's own selector, then its pods --------------------------
# A Deployment owns its pods through a ReplicaSet, so the honest read is
# the selector the Deployment declares, not a name prefix — a pod named
# boss-* in a namespace with a `boss-jobs` deployment would otherwise
# answer for the wrong workload.
if ! "${K[@]}" get deployment "$DEPLOY" -o json > "$TMP/deploy.json" 2> "$TMP/deploy.err"; then
    say "CANNOT ANSWER — the deployment read failed; kubectl said:"
    sed 's/^/    /' "$TMP/deploy.err" >&2
    exit "$CANNOT_ANSWER"
fi
SELECTOR=$(jq -r '.spec.selector.matchLabels // {} | to_entries | map("\(.key)=\(.value)") | join(",")' "$TMP/deploy.json" 2>/dev/null)
[ -n "$SELECTOR" ] || {
    say "CANNOT ANSWER — deployment $DEPLOY in $NS declares no matchLabels selector, so its pods cannot be named"
    exit "$CANNOT_ANSWER"
}
if ! "${K[@]}" get pods -l "$SELECTOR" -o json > "$TMP/pods.json" 2> "$TMP/pods.err"; then
    say "CANNOT ANSWER — the pods read failed; kubectl said:"
    sed 's/^/    /' "$TMP/pods.err" >&2
    exit "$CANNOT_ANSWER"
fi

# One row per pod: name, phase, ready n/m, restarts, creation stamp, the
# first non-running reason (or -), and the crashing containers as a
# space-joined list (or -). Tab-separated; jq does the reading, the
# shell only formats.
jq -r '
  .items[]
  | (.status.containerStatuses // []) as $cs
  | ($cs | map(select(.state.waiting.reason == "CrashLoopBackOff"
                   or .state.waiting.reason == "Error"
                   or .state.terminated.reason == "Error")) | map(.name)) as $crashing
  | ($cs | map(.state.waiting.reason // .state.terminated.reason // empty)) as $reasons
  | [ .metadata.name,
      (.status.phase // "Unknown"),
      "\($cs | map(select(.ready)) | length)/\($cs | length)",
      ($cs | map(.restartCount // 0) | add // 0),
      (.metadata.creationTimestamp // ""),
      (if ($reasons | length) > 0 then $reasons[0] else "-" end),
      (if ($crashing | length) > 0 then ($crashing | join(" ")) else "-" end) ]
  | @tsv' "$TMP/pods.json" > "$TMP/rows" 2> "$TMP/rows.err" || {
    say "CANNOT ANSWER — the pods read answered, but not with a pod list:"
    sed 's/^/    /' "$TMP/rows.err" >&2
    exit "$CANNOT_ANSWER"
}

# Age from an RFC 3339 stamp, compact (2d3h, 3h12m, 45m, 12s). GNU date —
# the forge is Linux; anything else reads `?` rather than failing the
# read.
age_of() {
    local then now s
    then=$(date -u -d "$1" +%s 2>/dev/null) || { echo '?'; return; }
    now=$(date -u +%s)
    s=$(( now - then ))
    [ "$s" -ge 0 ] || s=0
    if [ "$s" -ge 86400 ]; then echo "$(( s / 86400 ))d$(( (s % 86400) / 3600 ))h"
    elif [ "$s" -ge 3600 ]; then echo "$(( s / 3600 ))h$(( (s % 3600) / 60 ))m"
    elif [ "$s" -ge 60 ]; then echo "$(( s / 60 ))m"
    else echo "${s}s"
    fi
}

# --- the verdict -----------------------------------------------------------
if [ ! -s "$TMP/rows" ]; then
    echo "deployment $DEPLOY in $NS: no pods (selector $SELECTOR)"
    exit 0
fi
while IFS=$'\t' read -r name phase ready restarts stamp reason _crashing; do
    line="$name phase=$phase ready=$ready restarts=$restarts age=$(age_of "$stamp")"
    [ "$reason" = "-" ] || line="$line state=$reason"
    echo "$line"
done < "$TMP/rows"

# --- the tails -------------------------------------------------------------
# Per pod, the current tail; then the previous tail for every crashing
# container (or only the named one, when a container was given). A
# failed logs read prints kubectl's words under the header rather than
# stopping: the verdict is already out, and one pod's missing log must
# not hide another's.
tail_of() { # <pod> <label> <lines> <kubectl logs args...>
    local pod="$1" label="$2" n="$3"; shift 3
    echo "--- $pod $label (last $n lines) ---"
    "${K[@]}" logs "$pod" "$@" --prefix --tail="$n" 2>&1
}
while IFS=$'\t' read -r name _phase _ready _restarts _stamp _reason crashing; do
    if [ -n "$CONTAINER" ]; then
        tail_of "$name" "$CONTAINER" "$LINES" -c "$CONTAINER"
    else
        tail_of "$name" "all-containers" "$LINES" --all-containers
    fi
    [ "$crashing" = "-" ] && continue
    for c in $crashing; do
        [ -z "$CONTAINER" ] || [ "$c" = "$CONTAINER" ] || continue
        tail_of "$name" "$c previous" "$PREVIOUS_LINES" -c "$c" --previous
    done
done < "$TMP/rows"
exit 0
