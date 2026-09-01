#!/usr/bin/env bash
# Every binary the forge workflow invokes must be one the image has.
#
# WHY THIS EXISTS. Train 52's `web` job died on `bunx: command not
# found`. The Dockerfile carries a single line — `COPY --from=bun
# /usr/local/bin/bun /usr/local/bin/bun` — so the image has `bun` and
# not the `bunx` symlink that ships beside it. On a developer's Mac
# both exist, so the step passed every local check and then exited 127
# in the container, twenty minutes into a train.
#
# required-tools.txt already existed to answer exactly this question:
# "every binary the CI gate invokes, one per line", read by
# locomotive.sh so a missing tool is a red signal in seconds. But
# nothing connected the manifest to the WORKFLOW — the manifest listed
# what the image should carry, the workflow invoked whatever it liked,
# and the two drifted silently. That is CLAUDE.md §9a's fact living
# twice, and this is the equality test it asks for.
#
# WHAT IT CHECKS. The leading executable of every `run:` line in
# .forgejo/workflows/ci.yml, after stripping leading VAR=value
# assignments, must appear in required-tools.txt or in the allowlist
# below. It deliberately does not try to parse the whole shell — a
# pipeline's later stages, subshells, and `$(...)` are out of scope.
# The first word is where this class of failure lands, because it is
# the thing act resolves before any of the script runs.
#
# The allowlist is coreutils and shell builtins present in any Debian
# base. Adding to it is a decision; adding to required-tools.txt is
# how you say "the image must carry this", and that file is stamped
# into the image by build.sh, so the pair cannot drift unnoticed.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1

WORKFLOW=".forgejo/workflows/ci.yml"
# The image required-tools.txt describes, matched by REPO not tag. The
# downstream jobs pin boss-ci:${{ github.sha }} (a per-commit tag, so
# the runner cannot serve a stale image — see ci.yml's build-image
# note); an exact `:rust1.96` here would then match no job and the
# non-vacuity guard would fire on every run. The trailing colon keeps
# boss-ci-cache (a --cache-repo, never a container image) out. Jobs on
# any other image are out of scope and skipped.
IMAGE="10.20.0.15:3000/david/boss-ci:"
MANIFEST="infra/forge/boss-ci/required-tools.txt"

for f in "$WORKFLOW" "$MANIFEST"; do
    [ -f "$f" ] || { echo "ci-tools-declared: $f not found" >&2; exit 1; }
done

# Present in every Debian base; not worth a manifest line each.
ALLOWED_BUILTINS="bash sh echo cd set export test true false printf mkdir rm cp mv ln cat sed awk grep sort head tail tr xargs env sleep wait for if while do done then fi [ ] exit return shift local read eval"

declared=$(grep -vE '^\s*#|^\s*$' "$MANIFEST" | tr -d ' ')

# Leading executable of each `run:` step, but ONLY for jobs that run
# in the boss-ci image — required-tools.txt describes that image and
# has nothing to say about any other. `build-image` runs in kaniko and
# invokes /kaniko/executor, which is correct there and absent here;
# checking it against this manifest would be comparing a tool to the
# wrong contract.
#
# Also skips shell continuation lines: in a `foo \` / `--flag bar`
# block the second line's first word is an argument, not a command.
invoked=$(
    awk -v want="$IMAGE" '
        # A new job at two-space indent resets what we know.
        /^  [A-Za-z0-9_-]+:[[:space:]]*$/ { injob = 1; image = ""; inblock = 0; next }
        /^[A-Za-z]/                       { injob = 0; image = ""; inblock = 0 }
        # `container:` opens the block whose image the job RUNS IN.
        # `services:` also carries image: keys (the test job pulls
        # postgres:16) and mistaking one for the other silently made
        # this scraper skip that whole job — it found 3 commands where
        # it had found 5, and only the non-vacuity guard below caught
        # it.
        injob && /^    container:[[:space:]]*$/ { incontainer = 1; next }
        injob && /^    [A-Za-z0-9_-]+:/         { incontainer = 0 }
        injob && incontainer && /^[[:space:]]*image:[[:space:]]*/ {
            line = $0; sub(/^[[:space:]]*image:[[:space:]]*/, "", line); image = line; next
        }
        # Only scrape once the job is known to use the image the
        # manifest describes. Prefix, not equality: the tag is now
        # per-commit (boss-ci:${{ github.sha }}), so the fixed part is
        # the repo up to the colon.
        index(image, want) != 1 { next }
        /^[[:space:]]*run:[[:space:]]*[|>]/ { inblock = 1; cont = 0; next }
        inblock {
            if ($0 ~ /^[[:space:]]*$/) next
            if ($0 !~ /^[[:space:]][[:space:]]+/) { inblock = 0 }
            else {
                line = $0; sub(/^[[:space:]]+/, "", line)
                was_cont = cont
                cont = (line ~ /\\$/)
                if (!was_cont) print line
                next
            }
        }
        /^[[:space:]]*run:[[:space:]]*[^|>]/ {
            line = $0; sub(/^[[:space:]]*run:[[:space:]]*/, "", line); print line
        }
    ' "$WORKFLOW" |
    sed -E 's/^#.*//' |
    sed -E 's/^([A-Za-z_][A-Za-z0-9_]*=[^ ]*[[:space:]]+)+//' |
    awk '{ if (($1 == "bash" || $1 == "sh") && NF > 1) print $2; else print $1 }' |
    grep -vE '^$' |
    sort -u
)

problems=0
for tool in $invoked; do
    case " $ALLOWED_BUILTINS " in *" $tool "*) continue ;; esac

    # A path is a script from the CHECKOUT, not a binary from the
    # image, so the manifest has nothing to say about it. The useful
    # question is whether it is there and runnable — a workflow step
    # naming a script that moved fails the same way `bunx` did, with
    # a 127 deep inside a job.
    case "$tool" in
        */*)
            if [ ! -x "$tool" ]; then
                echo "ci-tools-declared: $WORKFLOW runs \`$tool\`, which is not an" >&2
                echo "                   executable file in this tree." >&2
                problems=$((problems + 1))
            fi
            continue
            ;;
    esac

    if ! printf '%s\n' "$declared" | grep -qxF -- "$tool"; then
        echo "ci-tools-declared: $WORKFLOW runs \`$tool\`, which is not in $MANIFEST" >&2
        echo "                   and not a shell builtin. Either add it to the manifest" >&2
        echo "                   (and to the Dockerfile — build.sh stamps the pair), or" >&2
        echo "                   invoke a tool the image already has." >&2
        problems=$((problems + 1))
    fi
done

# A check that scrapes nothing passes vacuously. The workflow has
# always had more than a handful of run steps; if the scrape collapses
# because the YAML was reformatted, say so instead of reporting green.
count=$(printf '%s\n' "$invoked" | grep -c . || true)
if [ "$count" -lt 4 ]; then
    echo "ci-tools-declared: only scraped $count command(s) from $WORKFLOW —" >&2
    echo "                   the parse broke, so a green result would mean nothing." >&2
    exit 1
fi

if [ "$problems" -gt 0 ]; then
    exit 1
fi
echo "ci-tools-declared: $count distinct commands, all declared"
