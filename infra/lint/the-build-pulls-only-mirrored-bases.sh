#!/usr/bin/env bash
# the-build-pulls-only-mirrored-bases.sh — every image build-image runs
# in or builds FROM is a forge tag, and every such tag is one the mirror
# list puts there.
#
# WHY THIS EXISTS (2026-09-07). build-image pulled its kaniko executor
# from gcr.io and the boss-ci Dockerfile's bases from docker.io, so a
# train's hot path resolved public names through a resolver with no
# redundancy — and `lookup gcr.io ... no such host` redded trains #236
# and #250 with every car clean. The bases are now copies in the forge
# registry, put there by infra/forge/mirror-base-images.sh, whose
# IMAGES list is the one source of truth for the tags. That leaves a
# fact living in two places — the list, and the refs ci.yml and the
# Dockerfile actually pull — which is the §9a pair this lint pins:
#
#   1. every `container: image:` in .forgejo/workflows/ci.yml, and every
#      FROM (or COPY/ADD --from=) in infra/forge/boss-ci/Dockerfile that
#      names no build stage, references the forge registry. A public ref
#      — gcr.io/..., docker.io/..., an unqualified name:tag — fails.
#   2. every forge tag so referenced is a destination in the mirror
#      list. A tag the build pulls but nothing mirrors fails here rather
#      than as a 404 on the train, which the retry loop rightly refuses
#      to treat as weather. Repos this pipeline pushes itself (boss-ci,
#      boss) are built, never mirrored, and are exempt from (2).
#
# The list is read by asking the script (`--check`) rather than parsing
# its source, as gate.sh is asked for its roster: the script owns its
# format. SCOPE: `services:` images (the test job's postgres:16) are
# pulled by the runner for a sibling job, not by the build, and are not
# covered here.
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 1

REGISTRY="10.20.0.15:3000"
FORGE_BASE="$REGISTRY/david"
# Repos this pipeline pushes: built by build-image, never mirrored.
BUILT_REPOS="boss-ci boss"
WORKFLOW=".forgejo/workflows/ci.yml"
DOCKERFILE="infra/forge/boss-ci/Dockerfile"
MIRROR="infra/forge/mirror-base-images.sh"

for f in "$WORKFLOW" "$DOCKERFILE" "$MIRROR"; do
    [ -f "$f" ] || { echo "the-build-pulls-only-mirrored-bases: $f not found" >&2; exit 1; }
done

# `LINE:ref` for the image each job RUNS IN — the `container:` block's
# image: key, or the inline `container: <ref>` form. `services:` also
# carries image: keys; the 4-space-key reset keeps them out, exactly as
# ci-tools-declared.sh scrapes the same file.
ci_images() {
    awk '
        function clean(s) {
            sub(/[[:space:]]+#.*$/, "", s); sub(/^#.*$/, "", s)
            gsub(/["\047]/, "", s); sub(/[[:space:]]+$/, "", s); return s
        }
        /^  [A-Za-z0-9_-]+:[[:space:]]*$/ { injob = 1; incontainer = 0; next }
        /^[A-Za-z]/                       { injob = 0; incontainer = 0 }
        injob && /^    container:/ {
            line = $0; sub(/^    container:[[:space:]]*/, "", line); line = clean(line)
            if (line == "") incontainer = 1; else print NR ":" line
            next
        }
        injob && /^    [A-Za-z0-9_-]+:/ { incontainer = 0 }
        injob && incontainer && /^[[:space:]]*image:[[:space:]]*/ {
            line = $0; sub(/^[[:space:]]*image:[[:space:]]*/, "", line); line = clean(line)
            if (line != "") print NR ":" line
        }
    ' "$1"
}

# `LINE:ref` for every base the Dockerfile pulls: FROM refs and
# COPY/ADD --from= targets, minus build stages (the `AS <name>` set,
# read in a first pass), numeric stage indexes and `scratch`.
dockerfile_refs() {
    awk '
        FNR == NR {
            if (toupper($1) == "FROM")
                for (i = 2; i < NF; i++) if (toupper($i) == "AS") stages[$(i + 1)] = 1
            next
        }
        toupper($1) == "FROM" {
            ref = ""
            for (i = 2; i <= NF; i++) { if ($i ~ /^--/) continue; ref = $i; break }
            if (ref != "" && ref != "scratch" && !(ref in stages)) print FNR ":" ref
            next
        }
        toupper($1) == "COPY" || toupper($1) == "ADD" {
            for (i = 2; i <= NF; i++) if ($i ~ /^--from=/) {
                ref = substr($i, 8)
                if (ref !~ /^[0-9]+$/ && !(ref in stages)) print FNR ":" ref
            }
        }
    ' "$1" "$1"
}

# The mirror's destinations as `repo:tag`, from the script's own
# `--check` (no docker, no network). Exits non-zero if the list is
# malformed, which is a finding in its own right.
mirrored_tags() {
    BOSS_FORGE_REGISTRY_BASE="$FORGE_BASE" bash "$1" --check 2>/dev/null \
        | awk -v base="$FORGE_BASE/" '
            index($0, "  ->  ") { r = $NF; if (index(r, base) == 1) print substr(r, length(base) + 1) }'
    return "${PIPESTATUS[0]}"
}

# check_tree <ci.yml> <Dockerfile> <mirror-script> — prints one line
# per finding, returns the count.
check_tree() {
    local wf="$1" df="$2" mirror="$3" mirrored entry file line ref rest repo b found
    if ! mirrored="$(mirrored_tags "$mirror")"; then
        echo "$mirror --check failed: the IMAGES list is malformed (run it to see why)"
        return 1
    fi
    found=0
    while IFS= read -r entry; do
        [ -n "$entry" ] || continue
        file="${entry%%|*}"; entry="${entry#*|}"
        line="${entry%%:*}"; ref="${entry#*:}"
        case "$ref" in
            "$FORGE_BASE"/*)
                rest="${ref#"$FORGE_BASE"/}"
                repo="${rest%%:*}"; repo="${repo%%@*}"; repo="${repo%%/*}"
                for b in $BUILT_REPOS; do [ "$repo" = "$b" ] && continue 2; done
                if ! printf '%s\n' "$mirrored" | grep -qxF -- "$rest"; then
                    echo "$file:$line: $ref is a forge tag nothing mirrors — add '<external ref>|$rest' to $MIRROR's IMAGES list and run the mirror-base-images verb before referencing it"
                    found=$((found + 1))
                fi ;;
            *)
                echo "$file:$line: $ref is pulled from a public registry. mirror it first: add to infra/forge/mirror-base-images.sh, run the mirror-base-images verb, then reference the forge tag"
                found=$((found + 1)) ;;
        esac
    done <<EOF
$(ci_images "$wf" | sed "s|^|$wf\||")
$(dockerfile_refs "$df" | sed "s|^|$df\||")
EOF
    return "$found"
}

# SELF-TEST first: a checker that cannot see the bad fixture has no
# business passing the real tree. The good fixture carries the shapes
# the real files use (a `${{ github.sha }}` tag on a built repo, a
# services: image, a stage COPY); the bad one carries every refusal.
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
cat > "$tmp/good.yml" <<'EOF'
jobs:
  build-image:
    runs-on: docker
    container:
      # a comment between the key and its image must not reset the block
      image: 10.20.0.15:3000/david/kaniko-executor:v1.23.2-debug
    steps: []
  test:
    runs-on: docker
    container:
      image: 10.20.0.15:3000/david/boss-ci:${{ github.sha }}
    services:
      postgres:
        image: postgres:16
EOF
cat > "$tmp/good.Dockerfile" <<'EOF'
FROM 10.20.0.15:3000/david/bun:1.3-slim AS bun
FROM --platform=linux/amd64 10.20.0.15:3000/david/rust:1.96.1-slim-bookworm
COPY --from=bun /usr/local/bin/bun /usr/local/bin/bun
COPY --from=0 /x /y
EOF
cat > "$tmp/bad.yml" <<'EOF'
jobs:
  build-image:
    runs-on: docker
    container:
      image: gcr.io/kaniko-project/executor:v1.23.2-debug
  smoke:
    runs-on: docker
    container: docker.io/library/alpine:3
EOF
cat > "$tmp/bad.Dockerfile" <<'EOF'
FROM oven/bun:1.3-slim AS bun
FROM 10.20.0.15:3000/david/not-in-the-mirror-list:0.0
COPY --from=docker.io/library/alpine:3 /etc/os-release /tmp/
EOF
out="$(check_tree "$tmp/good.yml" "$tmp/good.Dockerfile" "$MIRROR")"; rc=$?
[ "$rc" -eq 0 ] || { echo "FAIL: the good fixture reported findings:"; echo "$out"; exit 1; } >&2
out="$(check_tree "$tmp/bad.yml" "$tmp/bad.Dockerfile" "$MIRROR")"; rc=$?
[ "$rc" -eq 5 ] || { echo "FAIL: the bad fixture should report 5 findings, got $rc:"; echo "$out"; exit 1; } >&2
for want in \
    "bad.yml:5: gcr.io/kaniko-project/executor:v1.23.2-debug is pulled from a public registry. mirror it first" \
    "bad.yml:8: docker.io/library/alpine:3 is pulled from a public registry" \
    "bad.Dockerfile:1: oven/bun:1.3-slim is pulled from a public registry" \
    "bad.Dockerfile:2: 10.20.0.15:3000/david/not-in-the-mirror-list:0.0 is a forge tag nothing mirrors" \
    "bad.Dockerfile:3: docker.io/library/alpine:3 is pulled from a public registry"; do
    grep -qF -- "$want" <<<"$out" || { echo "FAIL: the bad fixture did not report '$want':"; echo "$out"; exit 1; } >&2
done

# THE REAL TREE.
out="$(check_tree "$WORKFLOW" "$DOCKERFILE" "$MIRROR")"; rc=$?
if [ "$rc" -ne 0 ]; then
    echo "the-build-pulls-only-mirrored-bases: $rc base(s) the build pulls that the forge mirror does not carry:" >&2
    echo "$out" >&2
    echo "  the mirror list ($MIRROR IMAGES) is the source of truth; a ref not on it is a public lookup on the hot path of every train." >&2
    exit 1
fi
n=$(( $(ci_images "$WORKFLOW" | wc -l) + $(dockerfile_refs "$DOCKERFILE" | wc -l) ))
echo "the-build-pulls-only-mirrored-bases: self-test ok — a public ref, an inline container ref and an unmirrored forge tag are refused by name and line; $n ref(s) across $WORKFLOW and $DOCKERFILE are forge tags the mirror list carries or this pipeline builds"
exit 0
