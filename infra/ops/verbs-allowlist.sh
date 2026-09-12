#!/bin/sh
# verbs-allowlist.sh — assemble the ops-request allowlist from the
# directory of verb files, one JSON document on stdout:
#
#   {"verbs": {"<name>": <contents of infra/ops/verbs/<name>.json>, ...}}
#
# Usage: verbs-allowlist.sh [dir]      (default: verbs/ beside this script)
#
# THE DIRECTORY IS THE ALLOWLIST (backlog 5086842d). It was one file,
# infra/ops/verbs.json, one JSON object — and a JSON object has no
# uncontended insertion point: appending contends on the previous
# entry's trailing comma and the closing brace, inserting alphabetically
# contends with a neighbour. On 2026-09-12 three cars each added a verb,
# two inserted before the same key, and the conductor left one behind
# (`conflict: infra/ops/verbs.json`) — the same shape CLAUDE.md §9a
# records for rules.toml before rules became one file each. Now a verb
# is infra/ops/verbs/<name>.json, the NAME is the file name, and adding
# one touches no shared line.
#
# ONE DERIVATION, CALLED BY EVERY SH/PYTHON READER. ops-runner.sh (the
# runner on the forge and boss-gcp) and the two lints that read the
# allowlist all run this script rather than each re-deriving "name =
# file name minus .json" (§9a: a rule that lives in four places drifts
# in one). boss-cli is the exception by necessity: it compiles the
# verbs in with include_str!, so its build.rs lists the same directory
# at build time, and a test pins its set to this one.
#
# IT REFUSES RATHER THAN GUESSES. No directory, an empty one, a file
# that is not a JSON object, a file carrying the old whole-allowlist
# shape (`verbs` key) or a name the runner could not match — each is an
# EX_CONFIG exit (78) naming the fault on stderr, never an empty or
# partial allowlist that would refuse every packet as "unknown verb"
# and send somebody to re-derive why (CLAUDE.md §Diagnosis).
#
# sh + jq only, no python (directive 26d61c97): the runner's hosts have
# jq and nothing else is promised there. Prose about the allowlist's
# rules lives in infra/ops/verbs/README.md; a README is not a verb,
# which is why only *.json is read.
set -u

dir="${1:-$(dirname "$0")/verbs}"
me=$(basename "$0")

if [ ! -d "$dir" ]; then
    echo "$me: verb directory $dir is missing or not a directory" >&2
    exit 78
fi

set -- "$dir"/*.json
if [ ! -e "$1" ]; then
    echo "$me: $dir holds no verb (*.json) — an empty allowlist is a fault, not a policy" >&2
    exit 78
fi

for f in "$@"; do
    name=$(basename "$f" .json)
    case "$name" in
        *[!a-z0-9-]*|-*|"")
            echo "$me: $f — a verb name is lowercase letters, digits and dashes, not leading with a dash (a packet's metadata.verb must match it exactly)" >&2
            exit 78 ;;
    esac
    if ! jq -e 'type == "object" and (has("verbs") | not)' "$f" >/dev/null 2>&1; then
        echo "$me: $f is not a JSON object describing ONE verb (about, hosts, argv, params[, timeout]); the whole-allowlist {\"verbs\": {...}} shape is gone — one file per verb" >&2
        exit 78
    fi
done

# input_filename is the file the most recent input came from, so each
# document is keyed by its own name; `add` merges the singletons into
# one table. Names are unique by construction (one file each).
jq -n '{verbs: ([inputs | {(input_filename | sub(".*/"; "") | sub("\\.json$"; "")): .}] | add)}' "$@"
