#!/usr/bin/env bash
# reach.sh <ipv4> <port> — can this host open a TCP connection to
# ip:port? READ-ONLY.
#
# WHAT. One TCP connect through bash's /dev/tcp, under `timeout 4`.
# Nothing is sent on the socket and nothing is read from it: the
# handshake completing, being refused, or timing out IS the answer.
# Nothing on this host is mutated. Prints exactly one line —
#
#     reach: <ip>:<port> open (<N>ms)
#     reach: <ip>:<port> closed (<why the kernel refused>)
#     reach: <ip>:<port> timeout (4s)
#
# and exits 0 only when open (1 otherwise, 2 on bad usage), so an
# ops packet's exit_code carries the verdict as well as its output.
# `closed` names the reason because the two it hides are different
# answers to the question this was built for: "Connection refused"
# means the host is reachable and the port is shut; "No route to
# host" / "Network is unreachable" means the path itself is gone.
#
# WHY. The cluster dev pod cannot probe the network the way a host
# can: no dig/traceroute/ping, no CAP_NET_RAW, and no route into the
# WireGuard overlay (10.99.0.0/24). Two live questions need a
# LAN/tunnel-side vantage: is the WireGuard hub 10.99.0.1 (boss-gcp)
# reachable from the forge over wg0 — the tunnel David's bastion route
# into the LAN depends on — and, later, whether the 1.1.1.0/24
# blackhole sits at the gateway or at the ISP. The forge already
# answers ops-request packets (infra/ops/ops-runner.sh); this is the
# script behind its `reach` verb.
#
# BOUNDS. The verb's params in infra/ops/verbs/reach.json admit only a
# dotted IPv4 quad and a 1-5 digit port (max 65535): no hostname, so
# this host never resolves a name on a packet's behalf; no path, no
# scheme, no whitespace, no leading dash. The checks below repeat that
# shape so the script is just as bounded when run by hand, and the
# values reach the connecting shell as positional parameters, never
# as program text.
set -euo pipefail

usage() { echo "usage: reach.sh <ipv4> <port>" >&2; exit 2; }
[[ $# -eq 2 ]] || usage
ip="$1"; port="$2"
[[ "$ip" =~ ^[0-9]{1,3}(\.[0-9]{1,3}){3}$ ]] || usage
[[ "$port" =~ ^[0-9]{1,5}$ ]] && (( 10#$port >= 1 && 10#$port <= 65535 )) || usage

start=$(date +%s%N)
rc=0
err=$(timeout 4 bash -c 'exec 3<>"/dev/tcp/$1/$2"' reach "$ip" "$port" 2>&1 >/dev/null) || rc=$?
case "$rc" in
    0)
        ms=$(( ($(date +%s%N) - start) / 1000000 ))
        echo "reach: $ip:$port open (${ms}ms)"
        exit 0
        ;;
    124)
        echo "reach: $ip:$port timeout (4s)"
        exit 1
        ;;
    *)
        # bash reports "<shell>: line 1: /dev/tcp/<ip>/<port>: <reason>";
        # keep the reason, the last colon-separated field.
        echo "reach: $ip:$port closed (${err##*: })"
        exit 1
        ;;
esac
