#!/usr/bin/env bash
#
# forge-log — READ-ONLY: the Forgejo container's own server log over a
# bounded window, and the bare repository's reflog of refs/heads/main
# over the same window.
#
#   forge-log.sh <since> <until> [lines]
#
#   since, until   RFC 3339 UTC, to the second: 2026-09-25T20:10:00Z.
#                  until > since, and the window is at most 15 minutes.
#   lines          1..2000, default 2000 — how many server-log lines
#                  are printed; the count of all of them is always said.
#
# WHY IT EXISTS (backlog 620bb69e, car 2 of design d812f1b7, D4)
# --------------------------------------------------------------
# On 2026-09-25 between 20:10:41Z and 20:11:14Z something wrote the
# forge's refs/heads/main back from c85941b4 (train #687's merge) to
# 777a5888. The one record that names who — a receive-pack line with
# the user, the source address and the time — is this container's log,
# and no door read it: pod-logs reads cluster pods, journal-tail reads
# systemd units, disk-report runs `docker system df` and never `docker
# logs`. David, 2026-09-25 ~21:40Z: "I actually want you to be able to
# do this operationally" — an agent reads the forge's own log through a
# door, never a person at a shell; and ~21:45Z, no by-hand read of the
# window while this door was built, even at the cost of the evidence.
#
# WHICH DAEMON — the SYSTEM one, pinned on the argv
# -------------------------------------------------
# The packet said "rootless docker"; the tree says otherwise.
# OPERATIONS.md names the system daemon as "the daemon CI jobs and
# Forgejo run on", and forge-backup.sh pins it because a DOCKER_HOST
# inherited from anywhere else (the converge builds on david's rootless
# daemon) "would answer 'no such container' about a container that is
# running". That wrong-target answer is the defect this verb must not
# have, so the pin rides the ARGV as `--host`: this reaches the daemon
# the way disk-report.sh does, `sudo -n docker`, and sudo's env_reset
# would drop an exported DOCKER_HOST before docker ever saw it. The ops
# runner runs as root (boss-ops-runner.service carries no User=), so
# sudo -n answers there; run as an account with no rule, it refuses,
# and this says it was sudo that refused.
#
# WHAT IT PRINTS — the verdict first, then the evidence
# -----------------------------------------------------
#   forge-log: container forgejo on the system daemon (<socket>): running=…, created …, started …, log driver <type> <config>
#   forge-log: window <since> .. <until> (<n> s)
#   forge-log: server log: <M> line(s) in the window, all printed
#              (or: the FIRST <N> printed — narrow the window to read the rest)
#   [forge-log: NOTE — the container was created after the window began …]
#   --- reflog of refs/heads/main in <repo> (inside the container): <k> of <m> entries in the window ---
#   <time> <old>..<new> <ident> <message>          one per entry in the window
#   --- server log (docker logs --timestamps; …redacted) ---
#   <docker timestamp> <line>                        stdout and stderr, as the daemon ordered them
#
# The reflog section rides BEFORE the server log because it is a handful
# of lines and the log may be 2000: the ops runner cuts a verb's output
# at 100 KB, and what is cut is the tail.
#
# The container's creation time is on the first line because a window
# that reads empty has two causes a reader must tell apart: the log was
# quiet, or the container was recreated since (its log dies with it —
# which is why D4 refused to switch the log driver to journald: the
# switch is a recreation). Rotation by the json-file driver is the
# third; the log config on the first line says whether it rotates.
#
# THE REFLOG — read inside the container, never on the host
# ---------------------------------------------------------
# /data/git/repositories/david/boss.git is the CONTAINER's path: the
# compose file at /opt/forgejo/docker-compose.yml binds the host's
# /opt/forgejo/data at the container's /data (OPERATIONS.md, failure
# mode 2; publish-github-pr.sh derives the same mount). Reading it
# through `docker exec -u git forgejo` reads what Forgejo itself sees,
# as the account that owns it, with no host path to derive. HEAD is read
# first as the control: a repository that is not there is exit 4 naming
# the path, never "no reflog", which is what a wrong path would
# otherwise answer. A repository with no logs/refs/heads/main says git
# kept none — Forgejo's [git.reflog] may be off — and that is an answer.
#
# REDACTED — before anything is printed
#   query-string VALUES: `?token=abc&limit=5` prints as
#   `?token=[redacted]&limit=[redacted]` — the keys stay, so a reader
#   sees which parameter was sent (a token in a query string is how a
#   Forgejo API credential lands in a router line);
#   URL credentials: `https://user:secret@host` prints as
#   `https://[redacted]@host` (a push-mirror URL carries one).
# Nothing else is. A log line is what the process printed: THE CALLER
# READS the packet before quoting it anywhere wider.
#
# EXIT
#   0  answered — the header, the reflog section and the log are on stdout
#   2  refused — a bound above; stderr names it; nothing on stdout, and
#      docker was never called
#   4  cannot answer — docker (or sudo) failed on the container check or
#      the log read: stderr carries the tool's own words, NOTHING on
#      stdout, because "0 lines" is exactly what a wrong target looks
#      like. A reflog that cannot be read is also 4, but the log is
#      still printed and the reflog section carries the words: one
#      missing half must not hide the other.
#
# ENV
#   BOSS_FORGE_LOG_DOCKER  the docker command, default `sudo -n docker`.
#                          The test seam (a stub docker, or a stub sudo).
# The ops runner passes no HOME; nothing here needs one. Tested against
# stubs in crates/core/boss-testing/tests/forge_log_sh.rs.

set -uo pipefail

ME="forge-log"
say() { echo "$ME: $*" >&2; }
refuse() { say "REFUSED — $*"; exit 2; }
CANNOT_ANSWER=4
MAX_LINES=2000
MAX_SPAN_S=900
CONTAINER="forgejo"
SYSTEM_DAEMON="unix:///var/run/docker.sock"
REPO="/data/git/repositories/david/boss.git"
REF="refs/heads/main"
UTC='^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$'

# --- the arguments' shape, before any docker call --------------------------
[ $# -ge 2 ] && [ $# -le 3 ] \
    || refuse "usage: $ME <since> <until> [lines 1..$MAX_LINES] — RFC 3339 UTC, e.g. 2026-09-25T20:10:00Z; got $# argument(s)"
SINCE="$1"; UNTIL="$2"; LINES="${3:-$MAX_LINES}"

[[ "$SINCE" =~ $UTC ]] || refuse "since '$SINCE' is not an RFC 3339 UTC time (YYYY-MM-DDTHH:MM:SSZ)"
[[ "$UNTIL" =~ $UTC ]] || refuse "until '$UNTIL' is not an RFC 3339 UTC time (YYYY-MM-DDTHH:MM:SSZ)"

# The epoch of a stamp, and only if it prints back as itself — GNU date
# refuses 2026-02-30 outright, and the round trip refuses any stamp date
# would quietly normalise.
epoch_of() {
    local e
    e=$(date -u -d "$1" +%s 2>/dev/null) || return 1
    [ "$(date -u -d "@$e" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null)" = "$1" ] || return 1
    printf '%s\n' "$e"
}
S=$(epoch_of "$SINCE") || refuse "since '$SINCE' is not a real time"
U=$(epoch_of "$UNTIL") || refuse "until '$UNTIL' is not a real time"
[ "$U" -gt "$S" ] || refuse "until must be after since (got $SINCE .. $UNTIL)"
SPAN=$(( U - S ))
[ "$SPAN" -le "$MAX_SPAN_S" ] \
    || refuse "the window is $SPAN s; this verb reads at most 15 minutes ($MAX_SPAN_S s) — file one request per quarter hour"
case "$LINES" in
    ''|*[!0-9]*) refuse "lines must be a number 1..$MAX_LINES, got '$LINES'" ;;
esac
[ "$LINES" -ge 1 ] 2>/dev/null && [ "$LINES" -le "$MAX_LINES" ] \
    || refuse "lines must be 1..$MAX_LINES, got $LINES"

# --- the daemon: the system one, on the argv --------------------------------
read -r -a DOCKER <<<"${BOSS_FORGE_LOG_DOCKER:-sudo -n docker}"
export DOCKER_HOST="$SYSTEM_DAEMON"
D=("${DOCKER[@]}" --host "$SYSTEM_DAEMON")

TMP=$(mktemp -d) || { say "CANNOT ANSWER — no working directory under ${TMPDIR:-/tmp}"; exit "$CANNOT_ANSWER"; }
trap 'rm -rf "$TMP"' EXIT

# Stop, with the tool's own words and nothing on stdout.
cannot() { # <what failed> <file holding the tool's words>
    say "CANNOT ANSWER — $1; ${D[*]} said:"
    sed 's/^/    /' "$2" >&2
    if grep -q '^sudo:' "$2"; then
        say "sudo -n refused: this verb reads the system daemon as root, and the ops runner runs as root — so it ran as an account with no sudo rule for docker. Nothing was read."
    fi
    exit "$CANNOT_ANSWER"
}

redact() {
    sed -E \
        -e 's#([A-Za-z][A-Za-z0-9+.-]*://)[^/@[:space:]]+@#\1[redacted]@#g' \
        -e 's/([?&][^=&?[:space:]"<>]+)=[^&[:space:]"<>]*/\1=[redacted]/g'
}

# --- the container, checked before it is read -------------------------------
# `--type container` so an IMAGE named forgejo cannot answer for it.
FMT='{{.State.Running}}|{{.Created}}|{{.State.StartedAt}}|{{.HostConfig.LogConfig.Type}}|{{json .HostConfig.LogConfig.Config}}'
"${D[@]}" inspect --type container --format "$FMT" "$CONTAINER" > "$TMP/inspect" 2> "$TMP/inspect.err" \
    || cannot "the container '$CONTAINER' could not be inspected on the system daemon" "$TMP/inspect.err"
IFS='|' read -r RUNNING CREATED STARTED DRIVER LOGCFG < "$TMP/inspect"
[ -n "${RUNNING:-}" ] || {
    echo "(docker inspect answered, but with nothing)" > "$TMP/inspect.err"
    cannot "the container '$CONTAINER' answered an empty inspect" "$TMP/inspect.err"
}

# --- the server log, both streams, in the daemon's order --------------------
# One file for stdout and stderr: the container writes its log on both,
# and on a failure the same file holds docker's words.
"${D[@]}" logs --timestamps --since "$SINCE" --until "$UNTIL" "$CONTAINER" > "$TMP/log" 2>&1 \
    || cannot "docker logs for '$CONTAINER' over $SINCE .. $UNTIL failed" "$TMP/log"
TOTAL=$(awk 'END { print NR }' "$TMP/log")

# --- the reflog, read inside the container -----------------------------------
# Into a file, so the header can go out first; REFLOG_FAILED decides the
# exit after the log is printed.
REFLOG_FAILED=no
reflog_section() {
    local head="--- reflog of $REF in $REPO (inside the container)"
    if ! "${D[@]}" exec -u git "$CONTAINER" cat "$REPO/HEAD" > "$TMP/head" 2> "$TMP/head.err"; then
        echo "$head ---"
        echo "CANNOT READ — no repository answered at $REPO (its HEAD would not read); ${D[*]} exec said:"
        sed 's/^/    /' "$TMP/head.err"
        REFLOG_FAILED=yes
        return
    fi
    "${D[@]}" exec -u git "$CONTAINER" test -f "$REPO/logs/$REF" 2> "$TMP/test.err"
    local rc=$?
    if [ "$rc" -eq 1 ] && [ ! -s "$TMP/test.err" ]; then
        echo "$head ---"
        echo "git kept no reflog for $REF in $REPO (no logs/$REF) — Forgejo's [git.reflog] may be off"
        return
    fi
    if [ "$rc" -ne 0 ] \
        || ! "${D[@]}" exec -u git "$CONTAINER" cat "$REPO/logs/$REF" > "$TMP/reflog" 2> "$TMP/reflog.err"; then
        [ -s "$TMP/test.err" ] && cat "$TMP/test.err" >> "$TMP/reflog.err"
        echo "$head ---"
        echo "CANNOT READ — the reflog at $REPO/logs/$REF would not read; ${D[*]} exec said:"
        sed 's/^/    /' "$TMP/reflog.err" 2>/dev/null
        REFLOG_FAILED=yes
        return
    fi
    # <old> <new> <ident…> <epoch> <tz>\t<message>. Keep the entries whose
    # epoch falls in the window; the ident may hold spaces, so the epoch
    # is the second-to-last word before the tab.
    awk -F'\t' -v s="$S" -v u="$U" '
        {
            n = split($1, a, " ")
            if (n < 5 || a[n-1] !~ /^[0-9]+$/) next
            e = a[n-1] + 0
            if (e < s + 0 || e > u + 0) next
            ident = a[3]
            for (i = 4; i <= n - 2; i++) ident = ident " " a[i]
            printf "%s\t%s\t%s\t%s\t%s\n", e, substr(a[1], 1, 12), substr(a[2], 1, 12), ident, (NF >= 2 ? $2 : "")
        }' "$TMP/reflog" > "$TMP/reflog.in"
    local all kept
    all=$(awk 'END { print NR }' "$TMP/reflog")
    kept=$(awk 'END { print NR }' "$TMP/reflog.in")
    echo "$head: $kept of $all entries in the window ---"
    while IFS=$'\t' read -r e old new ident msg; do
        printf '%s %s..%s %s %s\n' "$(date -u -d "@$e" +%Y-%m-%dT%H:%M:%SZ)" "$old" "$new" "$ident" "$msg"
    done < "$TMP/reflog.in" | redact
}
reflog_section > "$TMP/reflog.out"

# --- the verdict --------------------------------------------------------------
echo "$ME: container $CONTAINER on the system daemon ($SYSTEM_DAEMON): running=$RUNNING, created $CREATED, started $STARTED, log driver $DRIVER $LOGCFG"
echo "$ME: window $SINCE .. $UNTIL ($SPAN s)"
if [ "$TOTAL" -le "$LINES" ]; then
    echo "$ME: server log: $TOTAL line(s) in the window, all printed"
else
    echo "$ME: server log: $TOTAL line(s) in the window, the FIRST $LINES printed — narrow the window to read the rest"
fi
created_s=$(date -u -d "$CREATED" +%s 2>/dev/null) || created_s=""
if [ -n "$created_s" ] && [ "$created_s" -gt "$S" ]; then
    echo "$ME: NOTE — this container was created $CREATED, AFTER this window began: lines from before then went with the container it replaced"
elif [ "$TOTAL" -eq 0 ]; then
    echo "$ME: NOTE — the daemon holds no line in this window; with log driver $DRIVER $LOGCFG an older window may have rotated away"
fi

# --- the evidence ---------------------------------------------------------------
cat "$TMP/reflog.out"
echo "--- server log (docker logs --timestamps; query-string values and URL credentials redacted) ---"
head -n "$LINES" "$TMP/log" | redact

[ "$REFLOG_FAILED" = no ] || exit "$CANNOT_ANSWER"
exit 0
