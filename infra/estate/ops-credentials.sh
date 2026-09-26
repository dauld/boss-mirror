#!/bin/sh
# ops-credentials.sh — the ONE check of the root material a
# cluster-operator host is declared to hold (design 1bc4b4ed; the
# boundary is design 835c0c9c: an admin kubeconfig and a talosconfig
# cannot be minted from anything the estate holds, so placing them is
# David's act, and nothing in the estate ever writes one).
#
# TWO READERS, ONE DEFINITION (CLAUDE.md §9a; backlog 714bc71f):
#   - install-cluster-operator.sh records it on the converge packet as
#     `ops_credentials` — a recorded not-ready, never a failure: no
#     converge can repair an absence only David can fill.
#   - observe-host.sh carries it on the host observation, where
#     `estate.compare` judges it against the host's DECLARED roles and
#     raises `ops_credentials_absent:<host>` for a cluster-operator that
#     lacks it. Until this reader existed the converge's field was read
#     by nobody, and the absence surfaced only as a twelve-hour red
#     every forge converge (car 78a88f65) with no alarm.
#
# SOURCED, POSIX sh: observe-host.sh runs under dash (#!/bin/sh), and
# infra/lint/a-sh-script-parses-under-sh.sh holds the shebang to it.
#
# ops_credentials_state
#   Prints one line and returns 0, whatever it finds — it reports, the
#   caller decides:
#     present                                  both root:root 600
#     not ready: talosconfig:<why> kubeconfig:<why>
#                                              <why> is `absent`, or the
#                                              owner:group mode found
#     unmeasured: <dir> is not searchable by <user>
#                                              this reader cannot tell —
#                                              a 0700 root directory seen
#                                              from an unprivileged
#                                              observer; never "absent",
#                                              which would be a guess
#   The directory is $BOSS_OPS_DIR (default /etc/boss-ops), a knob only
#   so a test never reads /etc.

ops_credentials_state() {
    _oc_dir="${BOSS_OPS_DIR:-/etc/boss-ops}"
    if [ -d "$_oc_dir" ] && [ ! -x "$_oc_dir" ]; then
        echo "unmeasured: $_oc_dir is not searchable by $(id -un 2>/dev/null || id -u)"
        return 0
    fi
    _oc_missing=""
    for _oc_cred in talosconfig kubeconfig; do
        _oc_f="$_oc_dir/$_oc_cred"
        if [ ! -f "$_oc_f" ]; then
            _oc_missing="$_oc_missing $_oc_cred:absent"
        else
            _oc_have="$(stat -c '%U:%G %a' "$_oc_f" 2>/dev/null)"
            [ "$_oc_have" = "root:root 600" ] || _oc_missing="$_oc_missing $_oc_cred:$_oc_have"
        fi
    done
    if [ -n "$_oc_missing" ]; then
        echo "not ready:$_oc_missing"
    else
        echo "present"
    fi
}
