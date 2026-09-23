#!/bin/sh
# ops-runner — answer ops-request packets filed against this host.
#
# The PULL half of host observability (packet 729329c6). David: "I am
# really tired of the copy pasting after sshing." A debug read is a
# packet: somebody files an `ops-request` Job naming a host and an
# allowlisted read-only verb; this runner, on that host under a
# ~1-minute systemd timer, polls the system of record, executes the
# verb, and completes the packet's `execute` step with stdout/stderr
# and the exit code. The operator stops being the transport.
#
# ## Security posture (phase 1)
#
# - READ-ONLY. Every verb in the allowlist is a read; mutating verbs
#   are phase 2, behind per-verb policy, and are NOT in this script's
#   world at all.
# - THE ALLOWLIST IS THE AUTHORITY: infra/ops/verbs/<name>.json, one
#   file per verb, in-tree, reviewed, versioned (the verb's name IS
#   the file name; infra/ops/verbs-allowlist.sh assembles the
#   directory — 5086842d, after two verb cars collided on the one file
#   it used to be). A packet carries only a verb NAME and args; the
#   command words come from the file. The runner never executes a
#   packet-supplied string.
# - NO SHELL INTERPOLATION OF ARGS, EVER. The runner builds an argv
#   ARRAY (`set -- word word ...`) and execs it directly — no sh -c,
#   no eval, nothing packet-supplied ever becomes program text. Args
#   are validated against strict per-param patterns first (no
#   whitespace, no leading '-'), which is also what makes the
#   newline-split of jq's argv output below exact rather than hopeful.
# - A VERB SERVES NAMED HOSTS, and a host serves only the verbs that
#   name it: every allowlist entry declares `hosts` (estate node ids),
#   and a verb whose `hosts` does not list this runner's HOST_ID is
#   REFUSED — absent `hosts` included, so a new verb cannot reach a
#   host by forgetting to say. Not a privilege boundary (every runner
#   reads the same file) but a door that tells the truth about what it
#   opens: 11 of the 16 verbs name a script under the FORGE's checkout,
#   so when boss-gcp got a runner (2026-09-11) the unscoped allowlist
#   would have advertised a vocabulary of which 11 could only fail on
#   ENOENT. An exec failure is not a verdict; a refusal naming the
#   verb, this host and the hosts that verb does serve is.
# - Anything else — unknown verb, wrong arg shape, pattern miss —
#   drives the packet to its `refused` terminal with the reason in
#   `output` AND, named, in `reason` — the same text the journal line
#   carries, so a refusal never has to be re-derived from the host's
#   journal (6964f9e8; CLAUDE.md §Diagnosis: a verdict must name what
#   failed). Refusing loudly in the SoR beats guessing.
# - A param may be a LITERAL LIST (`one_of`) instead of a pattern: the
#   packet selects one of the reviewed words in the allowlist, and
#   nothing packet-supplied reaches the argv — so a word there may
#   lead with '-' (publish-github-pr's `--check`). With `optional`,
#   an absent arg DROPS its placeholder word from the argv rather
#   than passing an empty one.
#
# ## Behaviour
#
# - Polls open ops-request Jobs whose metadata.host equals HOST_ID,
#   exactly. A packet for a host with no runner is nobody's to guess
#   at; it sits open and visibly unanswered.
# - READS ITS OWN QUEUE before it walks it — depth and the oldest
#   packet's wait — on one journal line per run, and writes what each
#   answered request waited onto that request. The walk is serial and
#   has no per-verb fairness, so this depth is the only real bound on
#   raising any probe cadence further (backlog 1ffb3305). The depth is
#   the server's `total` for this host, and a reading that could not
#   see the whole queue prints `>=` and `truncated:` instead of passing
#   a page off as the count (2cfb4562).
# - Executes with a wall-clock timeout (OPS_TIMEOUT, default 30s) and
#   an output cap (OPS_OUTPUT_CAP, default 100KB); both truncations
#   are LOUD — a marker line in the recorded output says what was cut.
# - Completes the `execute` step with metadata MERGED, never replaced:
#   `PUT .../steps/{id}` swaps `metadata` wholesale, so sending only
#   new keys silently wipes the rest, including `authority_role`
#   (the boss-step.sh lesson).
# - Records the verb's exit ONCE, as `exit_code` on that step.
#   `answered` means the verb ran; `exit_code` says how it went, and
#   every reader — `boss ops --wait`, the answered-ops-request judges,
#   the yard — takes it from there (50fede8b; see the merge door below
#   for the request-level copy this replaced).
# - Records how long the verb RAN, as `duration_ms` beside that exit
#   (b7bfe821). The request's own stamps span this runner's poll
#   latency — up to a minute — so they cannot cost a verb; this is
#   taken around the exec. A refusal ran nothing and records neither.
# - A per-packet problem (refusal, missing step) never kills the loop;
#   a transport failure to the SoR fails the unit loudly, systemd
#   records it red, and the same loud-local-failure posture as the
#   estate observers applies (3ddd8333: silent-on-curl-failure does
#   not get a second landing).
# - No maintenance-wrap packet pair, deliberately: this fires every
#   minute, and a packet per firing would drown the board. Its
#   product IS packets — the ops-requests it answers — and its
#   failure modes are a red systemd unit plus a filed packet aging
#   visibly unanswered.
#
# ## Env
#
#   HOST_ID        (required) estate node id this runner answers for
#   BOSS_JOBS_URL  (required, no default — see below) the SoR
#   OPS_VERBS_DIR  (default: verbs/ beside this script) — one file per verb
#   OPS_TIMEOUT    (default 30) seconds before a verb is killed, unless
#                  the verb's allowlist entry declares its own `timeout`
#   OPS_OUTPUT_CAP (default 102400) bytes of output kept
#   BOSS_MACHINE_TOKEN (optional) forwarded as x-boss-machine-token
#
# All JSON parsing is jq with payloads on stdin or via --arg/--argjson/
# --rawfile, never spliced into the program text (the boss-step.sh /
# feedback-queue.sh rule).
set -u

: "${HOST_ID:?HOST_ID is required and must match the estate node id}"

# WHERE THE PACKET GOES IS NOT A DEFAULT, IT IS A DECISION — same
# refusal as boss-step.sh. Defaulting to 127.0.0.1 is how nightly
# maintenance packets spent weeks landing on a non-authoritative
# instance (2026-08-17). A runner with no system of record configured
# refuses, loudly, and systemd records a failed unit — which is a
# state somebody notices.
if [ -z "${BOSS_JOBS_URL:-}" ]; then
    echo "$(basename "$0"): BOSS_JOBS_URL is not set, and there is no safe default." >&2
    echo "    Defaulting to 127.0.0.1 is how nightly maintenance packets spent weeks" >&2
    echo "    landing on a non-authoritative instance (2026-08-17). Name the system of" >&2
    echo "    record explicitly:" >&2
    echo "        BOSS_JOBS_URL=http://<jobs-api-host>:<port> $(basename "$0")" >&2
    echo "    The installed unit pins it on the Exec line (see boss-ops-runner.service)." >&2
    exit 78   # EX_CONFIG — a configuration fault, not a run-time one.
fi
BASE="$BOSS_JOBS_URL"

VERBS_DIR="${OPS_VERBS_DIR:-$(dirname "$0")/verbs}"
# THE CHECKOUT THIS RUNNER IS PART OF. A verb's script is named in its
# verb file RELATIVE to the repo (infra/forge/disk-report.sh) and resolved
# here, against the checkout the runner itself runs from — never an
# absolute path baked into the allowlist. Until 2026-09-12 eleven of
# sixteen verbs carried /home/david/boss/…, the FORGE's checkout path,
# so none could run on boss-gcp (/opt/boss) and every reader of the
# file — two lints, the test harness — substituted that prefix for its
# own: one assumption in four places (66077f9c, CLAUDE.md §9a). A bare
# command (systemctl, df) stays a bare command, resolved on PATH.
OPS_REPO_ROOT="${OPS_REPO_ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}"
OPS_TIMEOUT="${OPS_TIMEOUT:-30}"
OPS_OUTPUT_CAP="${OPS_OUTPUT_CAP:-102400}"

workdir=$(mktemp -d) || exit 1
trap 'rm -rf "$workdir"' EXIT

# THE ALLOWLIST IS ASSEMBLED ONCE PER RUN from the directory, by the one
# script every sh/python reader shares, into a file the per-packet jq
# below reads. A directory that cannot be loaded — missing, empty, a
# file that is not one verb — is a refusal to run at all (EX_CONFIG,
# the fault named by the assembler on stderr), never a partial
# allowlist that refuses every packet as "unknown verb".
VERBS_FILE="$workdir/allowlist.json"
if ! sh "$(dirname "$0")/verbs-allowlist.sh" "$VERBS_DIR" > "$VERBS_FILE"; then
    echo "ops-runner: allowlist directory $VERBS_DIR could not be loaded — refusing to run" >&2
    exit 78
fi

# An automated answer should read as automation in the audit trail.
ACTOR="${BOSS_OPS_ACTOR:-automation:ops-runner}"
BOSS_USER="{\"id\":\"$ACTOR\",\"role\":\"platform-admin\",\"access_tier\":\"operator\",\"territory_account_ids\":[],\"direct_report_ids\":[],\"department\":\"platform\"}"

# A LIMIT IS NOT A FILTER (backlog 2cfb4562). This read was
# `?kind=ops-request&status=open&limit=100` — every host's open
# requests, one page, with the `total` the API answers beside the rows
# thrown away — so above 100 the walk silently skipped the tail and the
# gauge below printed the page as the whole queue: a queue stuck at 400
# read 100 forever, and no threshold above 100 could ever be crossed.
# Now the SERVER narrows to this host (`metadata` containment, the
# host url-encoded by jq, never spliced), the page is the API's own
# ceiling (MAX_LIMIT in crates/core/boss-jobs/src/http/mod.rs — if the
# two ever disagree, the comparison below says so loudly rather than
# the page passing for the queue), and the rows are held against
# `total` before anything reads them as a count.
host_doc=$(jq -rn --arg h "$HOST_ID" '{host: $h} | tojson | @uri')
QUEUE_PAGE=1000
if ! jobs_json=$(curl -fsS -H "x-boss-user: $BOSS_USER" \
        "$BASE/api/jobs?kind=ops-request&status=open&metadata=$host_doc&limit=$QUEUE_PAGE" 2>&1); then
    echo "ops-runner: jobs-api unreachable at $BASE — $jobs_json" >&2
    exit 1
fi

# Envelope ({"data": [...]}) or bare array; keep open rows for THIS
# host only — the server was asked to narrow, and this still checks.
mine=$(printf '%s' "$jobs_json" | jq -c --arg h "$HOST_ID" '
    (if type == "object" and has("data") then .data else . end)
    | map(select(.status == "open" and (.metadata.host // "") == $h))')
n=$(printf '%s' "$mine" | jq 'length')

# THE DEPTH IS EXACT ONLY WHEN THE SERVER'S COUNT VOUCHES FOR IT. Three
# ways it cannot: no numeric `total` (a bare array, an older shape), a
# page that held fewer rows than `total`, or a row on the page for
# ANOTHER host — the evidence the containment filter was not applied,
# and then `total` is every host's (a wrong target answers instead of
# erroring, CLAUDE.md §Doors). The first and third leave only a lower
# bound; the second knows the depth but read part of it, and the list
# is newest first, so the oldest packets are exactly the ones unread.
# `truncated` names which, and the gauge prints `>=` in place of `=` so
# no reader parsing `depth=` can take a lower bound for the count.
depth="$n"; depth_exact=true; truncated=""
total=$(printf '%s' "$jobs_json" | jq -r '
    if type == "object" and (.total | type) == "number" then .total else "" end')
rows=$(printf '%s' "$jobs_json" | jq '
    (if type == "object" and has("data") then .data else . end) | length')
if [ "$rows" -ne "$n" ]; then
    depth_exact=false
    truncated="the list was not narrowed to host $HOST_ID ($rows rows, $n for it) — its total is not this host's"
else
    case ${total:-empty} in
        empty | *[!0-9]*)
            depth_exact=false
            truncated="the list carried no total, so how much it did not hold is unknown" ;;
        *)
            depth="$total"
            [ "$n" -ge "$total" ] || truncated="read $n of $total open — the walk takes the rest on later runs" ;;
    esac
fi

# THE READING, TAKEN BEFORE THE WALK (backlog 1ffb3305). This loop is
# serial and has no per-verb fairness: a latency-sensitive verb waits
# behind whatever is ahead of it in the same run, so a converge — the
# verb that deploys a fix — waits behind however many run-car-probes
# are in front of it. Measured 2026-09-19: run-car-probe was 171 of
# the last 300 ops-requests against 42 converges, and its cadence went
# daily to hourly the same day. Today's volumes are comfortable and
# that is the point of measuring now: the depth is the only real bound
# on raising a probe cadence further, and a queue whose depth nobody
# reads is one that gets discovered at its worst moment, by a converge
# that did not deploy when it should have. Per-verb fairness or a
# priority lane is the larger change and waits for this reading to say
# it is needed.
#
# One wait per packet, computed once here and carried into the walk —
# a request's own wait rides its metadata below, so the series is a
# jobs-API query rather than an ssh to read this journal. `opened_at`
# is what the filer stamps; a packet without one has NO age and says
# so, because `date -d ''` answers midnight rather than erroring and
# would report a fresh packet as a decades-old wait
# (crates/core/boss-jobs/src/probe.rs carries the same trap).
now_s=$(date -u +%s)
waitsf="$workdir/waits"
: > "$waitsf"
oldest_wait="-"
while IFS= read -r ts; do
    [ -n "$ts" ] || continue           # jq emits "-" for an absent stamp,
    w="-"                              # so an empty line is only the
    if [ "$ts" != "-" ]; then          # heredoc's own trailing newline.
        t=$(date -u -d "$ts" +%s 2>/dev/null) || t=""
        case ${t:-empty} in
            empty | *[!0-9]*) ;;
            *)
                w=$((now_s - t))
                [ "$w" -ge 0 ] || w=0  # a clock skew is not a negative wait
                if [ "$oldest_wait" = "-" ] || [ "$w" -gt "$oldest_wait" ]; then
                    oldest_wait="$w"
                fi
                ;;
        esac
    fi
    printf '%s\n' "$w" >> "$waitsf"
done <<TS
$(printf '%s' "$mine" | jq -r '.[] | .metadata.opened_at // "-"')
TS
# EVERY run, depth zero included: a gauge that appears only when it is
# non-zero cannot be told apart from a runner that stopped. A reading
# that could not see the whole queue says so on the same line (above).
if [ -z "$truncated" ]; then
    echo "ops-runner: queue host=$HOST_ID depth=$depth oldest_wait_s=$oldest_wait"
elif [ "$depth_exact" = true ]; then
    echo "ops-runner: queue host=$HOST_ID depth=$depth oldest_wait_s>=$oldest_wait truncated: $truncated"
else
    echo "ops-runner: queue host=$HOST_ID depth>=$depth oldest_wait_s>=$oldest_wait truncated: $truncated"
fi

if [ "$n" -eq 0 ]; then
    echo "ops-runner: no open ops-request for $HOST_ID"
    exit 0
fi

answered=0; refused=0; skipped=0; failed=0
i=0
while [ "$i" -lt "$n" ]; do
    job=$(printf '%s' "$mine" | jq -c ".[$i]")
    i=$((i + 1))
    # This packet's own wait, from the pass above — one line per
    # packet, in order, so the index IS the line number. "-" when the
    # packet carries no `opened_at`.
    wait_s=$(sed -n "${i}p" "$waitsf")
    job_id=$(printf '%s' "$job" | jq -r '.id')
    short=$(printf '%s' "$job_id" | cut -c1-8)

    # Slug first, title as fallback — the boss-step.sh idiom.
    step=$(printf '%s' "$job" | jq -c '
        ((.steps // []) | map(select(.spec_slug == "execute")) | .[0])
        // ((.steps // []) | map(select(.title == "execute")) | .[0])
        // empty')
    if [ -z "$step" ]; then
        echo "ops-runner: $short has no execute step — skipping" >&2
        skipped=$((skipped + 1))
        continue
    fi
    step_status=$(printf '%s' "$step" | jq -r '.status // ""')
    case "$step_status" in
        ready|active) ;;
        *)
            # pending (predicate not yet true) waits for the next
            # poll; completed/skipped is a race with the outcome step.
            echo "ops-runner: $short execute is '$step_status' — skipping this cycle" >&2
            skipped=$((skipped + 1))
            continue
            ;;
    esac
    step_id=$(printf '%s' "$step" | jq -r '.id')

    verb=$(printf '%s' "$job" | jq -r '.metadata.verb // ""')
    args=$(printf '%s' "$job" | jq -c '.metadata.args // []')

    # One jq pass over the ALLOWLIST decides: either a refusal reason
    # or a fully resolved argv. The packet's verb and args enter only
    # as --arg/--argjson values — data, never program text.
    decision=$(jq -c --arg verb "$verb" --arg host "$HOST_ID" --argjson args "$args" '
        def refuse(msg): {refuse: msg};
        .verbs[$verb] as $spec
        | if $verb == "" then refuse("metadata.verb is missing")
          elif $spec == null then
            refuse("verb \($verb) is not in the allowlist (infra/ops/verbs/); verbs: "
                   + (.verbs | keys | join(", ")))
          elif (($spec.hosts // []) | index($host)) == null then
            refuse("verb \($verb) does not serve host \($host) — infra/ops/verbs/\($verb).json scopes it to "
                   + (if (($spec.hosts // []) | length) == 0
                      then "no host (a verb that declares no `hosts` is refused everywhere)"
                      else ($spec.hosts | join(", ")) end)
                   + "; verbs this host serves: "
                   + ([.verbs | to_entries[] | select((.value.hosts // []) | index($host)) | .key] | join(", ")))
          elif ($spec.requires_approval // false) == true then
            refuse("verb \($verb) declares requires_approval, and this runner cannot verify "
                   + "an approval: nothing issues one yet (design 17835005 — a rendered plan "
                   + "hash, signed, single-use, verified before the argv is built). A runner "
                   + "that cannot check an approval refuses rather than assumes, so this verb "
                   + "is inert until that channel exists")
          elif ($args | type) != "array" or any($args[]; type != "string") then
            refuse("metadata.args must be a JSON array of strings")
          elif ($args | length) > ($spec.params | length) then
            refuse("verb \($verb) takes at most \($spec.params | length) arg(s), got \($args | length)")
          else
            [ $spec.params | to_entries[] | .key as $i | .value as $p
              | $args[$i] as $raw
              | if $raw == null then
                  (if $p | has("default") then {ok: $p.default}
                   elif ($p.optional // false) == true then {omit: true}
                   else {err: "missing required arg \($p.name)"} end)
                elif $p | has("one_of") then
                  (if any($p.one_of[]; . == $raw) then {ok: $raw}
                   else {err: "arg \($p.name) value \($raw) is not one of \($p.one_of | join(", "))"} end)
                elif ($raw | test($p.pattern)) | not then
                  {err: "arg \($p.name) value \($raw) does not match \($p.pattern)"}
                elif ($p | has("max")) and (($raw | tonumber) > $p.max) then
                  {err: "arg \($p.name) value \($raw) exceeds max \($p.max)"}
                else {ok: $raw} end
            ] as $vals
            | [ $vals[] | select(has("err")) | .err ] as $errs
            | if ($errs | length) > 0 then refuse($errs | join("; "))
              else {argv: [ $spec.argv[]
                            | if test("^\\{[0-9]+\\}$")
                              then . as $ph
                                   | $vals[($ph | ltrimstr("{") | rtrimstr("}") | tonumber) - 1]
                                   | if has("omit") then empty else .ok end
                              else . end ],
                    timeout: ($spec.timeout // null)}
              end
          end' "$VERBS_FILE")

    outf="$workdir/out"
    disp=""; rc_str=""; script=""; dur_ms=""
    reason=$(printf '%s' "$decision" | jq -r '.refuse // empty')
    if [ -n "$reason" ]; then
        disp="refused"; rc_str=""
        printf '%s' "$reason" > "$outf"
    else
        # Build the argv as positional parameters. Newline-split is
        # EXACT here, not hopeful: fixed argv words are reviewed file
        # content and substituted args have passed patterns that admit
        # no whitespace.
        set --
        while IFS= read -r w; do
            set -- "$@" "$w"
        done <<ARGV
$(printf '%s' "$decision" | jq -r '.argv[]')
ARGV
        # argv[0]: a repo-relative script resolves against this checkout
        # and must exist there — an absent script is a REFUSAL naming the
        # path, not an exec error dressed up as an answer. A word with no
        # slash is a bare command for PATH; an absolute path is left as
        # the allowlist wrote it (the lint refuses those).
        script=""
        case "$1" in
            /*) script="$1" ;;
            */*) script="$OPS_REPO_ROOT/$1" ;;
        esac
        if [ -n "$script" ] && [ ! -x "$script" ]; then
            reason="verb $verb names a script not in this checkout: $1 (resolved to $script under OPS_REPO_ROOT=$OPS_REPO_ROOT)"
            disp="refused"; rc_str=""
            printf '%s' "$reason" > "$outf"
        fi
    fi
    if [ "$disp" != "refused" ]; then
        if [ -n "$script" ]; then
            shift; set -- "$script" "$@"
        fi
        # A verb may declare its own `timeout` in the allowlist (a
        # reviewed number, like its argv); otherwise the runner's
        # default applies. publish-github-pr's first push of the whole
        # tree is minutes, not seconds.
        verb_timeout=$(printf '%s' "$decision" | jq -r '.timeout // empty')
        rawf="$workdir/raw"
        # The packet's id rides in the verb's ENVIRONMENT, never its
        # argv: a verb that starts a longer-running unit (converge-now)
        # can leave the unit a note saying which packet asked, so the
        # unit's outcome lands back on that packet instead of the
        # `answered` that `systemctl start --no-block` earns by merely
        # being accepted (backlog d66f92b2). Data, not program text.
        #
        # SO DOES THE ACTOR. A verb whose argv is the tree's own CLI
        # (`boss prove … --unattended`, run-car-probe since backlog
        # 9f00a805 car 2) signs its jobs-API writes as BOSS_ACTOR and
        # REFUSES a write unnamed (CLAUDE.md §Doors); this runner's own
        # account is the one it should sign as, the same identity the
        # step completion below carries. A unit that set BOSS_ACTOR
        # itself wins — the drop-in is the operator's say.
        # HOW LONG IT TOOK, taken around the exec and recorded beside
        # the exit (backlog b7bfe821). opened_at-to-closed_at is the
        # only duration the record used to carry and it is dominated
        # by up to a minute of this runner's poll latency, so costing
        # run-car-probe on 2026-09-19 meant inferring per-probe time
        # from the spread within a simultaneous batch. The number is
        # here, free, at the moment the exit is written. Milliseconds
        # because a probe is seconds. A `date` without GNU's %N leaves
        # a non-digit in the stamp; the guard below then records no
        # duration rather than a nonsense one.
        t0=$(date -u +%s%3N 2>/dev/null)
        OPS_REQUEST_ID="$job_id" BOSS_ACTOR="${BOSS_ACTOR:-$ACTOR}" \
            timeout "${verb_timeout:-$OPS_TIMEOUT}" "$@" > "$rawf" 2>&1 < /dev/null
        rc=$?
        t1=$(date -u +%s%3N 2>/dev/null)
        dur_ms=""
        case ${t0:-empty}${t1:-empty} in
            *[!0-9]*) ;;
            *)
                dur_ms=$((t1 - t0))
                [ "$dur_ms" -ge 0 ] || dur_ms=0   # a clock step is not a negative run
                ;;
        esac
        size=$(wc -c < "$rawf")
        if [ "$size" -gt "$OPS_OUTPUT_CAP" ]; then
            head -c "$OPS_OUTPUT_CAP" "$rawf" > "$outf"
            printf '\n[ops-runner: output truncated — kept %s of %s bytes]\n' \
                "$OPS_OUTPUT_CAP" "$size" >> "$outf"
        else
            cat "$rawf" > "$outf"
        fi
        if [ "$rc" -eq 124 ]; then
            printf '\n[ops-runner: command killed at %ss timeout]\n' "${verb_timeout:-$OPS_TIMEOUT}" >> "$outf"
        fi
        disp="answered"; rc_str="$rc"
    fi

    # Merge, never replace (see header). The output rides --rawfile so
    # arbitrary command output stays data. A refusal also writes the
    # reason under its own name: `output` on a refusal IS the reason
    # (one source — the same string the journal line below prints),
    # and a reader of the packet should not have to know that.
    merged=$(printf '%s' "$step" | jq -c --rawfile out "$outf" \
        --arg d "$disp" --arg rc "$rc_str" --arg h "$HOST_ID" --arg ms "${dur_ms:-}" '
        (.metadata // {})
        + {disposition: $d, output: $out, runner_host: $h}
        + (if $rc == "" then {} else {exit_code: $rc} end)
        + (if $ms == "" then {} else {duration_ms: ($ms | tonumber)} end)
        + (if $d == "refused" then {reason: $out} else {} end)')
    payloadf="$workdir/payload"
    printf '%s' "$merged" | jq -c '{status: "completed", metadata: .}' > "$payloadf"

    # THE QUEUE READING RIDES THE REQUEST (1ffb3305): the depth this
    # run faced, and what THIS request waited before the runner reached
    # it. Both are facts about the queue, not about the verb, so they
    # have no home on the execute step — and riding the merge door
    # makes the depth a question the jobs API answers, which the
    # journal line alone does not. This is the wait BEFORE the verb,
    # not the run: the run's own `duration_ms` rides the execute step
    # with the exit it belongs to (b7bfe821).
    #
    # THE VERB'S EXIT DOES NOT RIDE HERE (backlog 50fede8b). It used to:
    # f47861a5 added `exit` beside the step's `exit_code` so a reader of
    # the close would see it where the outcome is. Nothing ever read the
    # copy — `boss ops --wait`, `verb_failure` (the whole answered-ops-
    # request judge family) and the yard's shed and signals all read
    # `exit_code` off the execute step, and a list read carries each
    # row's steps, so a request-level reader never had to fetch them
    # separately. Two spellings of one fact, written by one act and held
    # equal by nothing, is what CLAUDE.md §9a refuses: the first writer
    # to move one without the other (a retry, a hand correction, a
    # second runner) hands a reader a stale exit. One spelling now:
    # `exit_code`, on the step, written by the PUT below.
    #
    # A refusal ran nothing and waited in no queue this run answered,
    # so it writes nothing here. A failed merge does not withhold the
    # answer — the step completion below still lands — but it is
    # counted, and the unit goes red for it.
    if [ "$disp" = "answered" ]; then
        exitf="$workdir/exit"
        # A lower bound rides under its own key, never as `queue_depth`
        # — a reader of that key takes it as the count (2cfb4562).
        jq -cn --arg w "$wait_s" --argjson d "$depth" --arg exact "$depth_exact" '
            (if $exact == "true" then {queue_depth: $d} else {queue_depth_at_least: $d} end)
            + (if $w == "-" then {} else {queued_s: ($w | tonumber)} end)' > "$exitf"
        if ! patch_err=$(curl -fsS -X PATCH -H "content-type: application/json" \
                -H "x-boss-user: $BOSS_USER" \
                ${BOSS_MACHINE_TOKEN:+-H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"} \
                --data-binary @"$exitf" \
                "$BASE/api/jobs/$job_id/metadata" 2>&1 >/dev/null); then
            echo "ops-runner: PATCH queue_depth=$depth failed on $short — $patch_err" >&2
            failed=$((failed + 1))
        fi
    fi

    if ! put_err=$(curl -fsS -X PUT -H "content-type: application/json" \
            -H "x-boss-user: $BOSS_USER" \
            ${BOSS_MACHINE_TOKEN:+-H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"} \
            --data-binary @"$payloadf" \
            "$BASE/api/jobs/$job_id/steps/$step_id" 2>&1 >/dev/null); then
        echo "ops-runner: PUT failed on $short — $put_err" >&2
        failed=$((failed + 1))
        continue
    fi

    if [ "$disp" = "answered" ]; then
        echo "ops-runner: answered $verb on $short (exit $rc_str, ${size}B, ${dur_ms:--}ms)"
        answered=$((answered + 1))
    else
        echo "ops-runner: refused $short — $reason"
        refused=$((refused + 1))
    fi
done

echo "ops-runner: $HOST_ID answered=$answered refused=$refused skipped=$skipped failed=$failed"
[ "$failed" -eq 0 ] || exit 1
