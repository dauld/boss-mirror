#!/usr/bin/env bash
# install-cluster-operator.sh — what the `cluster-operator` role brings
# to a host, in ONE definition every managed host runs.
#
# WHY IT IS HERE AND NOT IN A HOST'S INSTALLER. Until 2026-09-20 this
# logic lived inline in infra/forge/install.sh, which was correct while
# the forge was the estate's only cluster-operator. Adding a second one
# (boss-gcp) would have meant a second copy in infra/gcp — the pair
# CLAUDE.md §9a is about, and the talosctl version pin is exactly the
# fact that must not drift: a client more than one minor from the
# cluster is the failure the pin exists to prevent, and two pins can
# disagree. Collapsed rather than pinned, for the reason §9a gives:
# a pin keeps the duplication it stops the drift on. This is the same
# move install-cli-from-image.sh made on 2026-09-18 (backlog 9f00a805)
# when the forge started running boss-gcp's CLI installer.
#
# SOURCED, NOT EXECUTED — like node-roles.sh beside it. The caller has
# already sourced infra/run-summary.sh, and a sourced file can report on
# the converge packet through it; a child process cannot.
#
#   . "$REPO/infra/estate/install-cluster-operator.sh"
#   has_role cluster-operator && install_cluster_operator
#
# CREDENTIALS ARE CHECKED, NEVER WRITTEN. /etc/boss-ops/talosconfig and
# /etc/boss-ops/kubeconfig are placed once by David — token admin is his
# — and must be root:root 0600. Anything else is reported on the
# converge packet as absent-or-wrong until fixed. The estate's own
# converge never writes a credential, on any host.

# The Talos client is the ONLY interface to the nodes (no ssh) and must
# stay within one minor of the cluster. v1.13.8 is the version David's
# own workstation client runs, so a managed host answers exactly as the
# workstation did. ONE pin, read by every host that holds the role.
BOSS_TALOSCTL_VERSION="v1.13.8"
BOSS_TALOSCTL_SHA256="406b56f9e4ff03b1557cc941b1f163aec8a6ebb36e28f0bbbe6d083589529261"

# Where the role's credentials live. A knob only so a test never reads
# or writes /etc.
BOSS_OPS_DIR="${BOSS_OPS_DIR:-/etc/boss-ops}"

install_cluster_operator() {
    _co_talosctl
    _co_credentials
}

# talosctl, pinned by sha. Absent-or-wrong is reported and does not
# stop the converge: a host with no Talos client is a host that cannot
# run cluster commands, which is worth saying loudly and is not worth
# refusing every other row over.
_co_talosctl() {
    [ "${INSTALL_TALOSCTL:-1}" = "1" ] || return 0
    [ ! -x /usr/local/bin/talosctl ] || return 0
    local tmp
    tmp="$(mktemp)"
    if curl -sfL -o "$tmp" "https://github.com/siderolabs/talos/releases/download/${BOSS_TALOSCTL_VERSION}/talosctl-linux-amd64" \
        && echo "${BOSS_TALOSCTL_SHA256}  ${tmp}" | sha256sum -c - >/dev/null; then
        install -m 0755 "$tmp" /usr/local/bin/talosctl
        echo "install-cluster-operator: talosctl ${BOSS_TALOSCTL_VERSION} installed"
    else
        echo "install-cluster-operator: talosctl download or checksum failed — the cluster-operator role has no Talos client until it is present" >&2
    fi
    rm -f "$tmp"
}

_co_credentials() {
    local missing="" cred f
    for cred in talosconfig kubeconfig; do
        f="$BOSS_OPS_DIR/$cred"
        if [ ! -f "$f" ]; then
            missing="$missing $cred:absent"
        elif [ "$(stat -c '%U:%G %a' "$f" 2>/dev/null)" != "root:root 600" ]; then
            missing="$missing $cred:$(stat -c '%U:%G %a' "$f")"
        fi
    done
    if [ -n "$missing" ]; then
        echo "install-cluster-operator: credentials not ready —${missing} (want root:root 600 under $BOSS_OPS_DIR; placed by hand, never by this script)"
        if declare -F run_summary_field >/dev/null; then run_summary_field ops_credentials "not ready:${missing}"; fi
    else
        echo "install-cluster-operator: credentials present (root:root 600)"
        if declare -F run_summary_field >/dev/null; then run_summary_field ops_credentials "present"; fi
    fi
}
