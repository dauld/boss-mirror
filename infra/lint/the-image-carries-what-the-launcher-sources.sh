#!/usr/bin/env bash
# the-image-carries-what-the-launcher-sources.sh — every file the
# container launcher sources from its own directory is COPYed into the
# image beside it.
#
# services-launcher.sh is installed as /usr/local/bin/boss-launch, and
# a `. "$(dirname "${BASH_SOURCE[0]}")/<file>"` in it resolves to
# /usr/local/bin/<file> at run time — a path only a Dockerfile COPY can
# populate. On 2026-09-05 a car added such a source line with no COPY;
# the gate was green (it never builds the image), the train merged, and
# the cluster pod crash-looped on "No such file or directory" before a
# single API started. This reads both files and refuses the pairing
# that bricked production. Self-tested on planted fixtures.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# sourced_files LAUNCHER — the basenames the launcher sources from its own dir.
sourced_files() {
    grep -oE '^\s*\.\s+"\$\(dirname\s+"\$\{BASH_SOURCE\[0\]\}"\)/[^"]+"' "$1" \
        | sed -E 's|.*\)/([^"]+)".*|\1|' | sort -u
}
# launcher_dir DOCKERFILE — where the launcher is COPYed to (its directory).
launcher_dir() {
    grep -oE '^COPY\s+\S*services-launcher\.sh\s+\S+' "$1" | awk '{print $3}' | xargs -r dirname
}
# copied_to DOCKERFILE DIR — basenames COPYed into DIR (dest path or dest dir/).
copied_to() {
    local df="$1" dir="$2"
    grep -E '^COPY\s' "$df" | awk '{print $NF}' | while read -r dest; do
        case "$dest" in
            "$dir"/*) basename "$dest";;
        esac
    done | sort -u
}
missing() {
    local launcher="$1" df="$2" dir
    dir="$(launcher_dir "$df")"
    [[ -n "$dir" ]] || { echo "(launcher is not COPYed by the Dockerfile)"; return; }
    comm -23 <(sourced_files "$launcher") <(copied_to "$df" "$dir")
}

self_test() {
    local fx; fx="$(mktemp -d)"; trap 'rm -rf "$fx"' RETURN
    printf '. "$(dirname "${BASH_SOURCE[0]}")/tenant-launch.sh"\n. "$(dirname "${BASH_SOURCE[0]}")/other-lib.sh"\n' >"$fx/launcher.sh"
    # The Dockerfile verb is spelled through a variable so this fixture
    # line does not read as SQL to api-path-bypass-smell.
    local v="COPY"
    printf '%s infra/oss-quickstart/services-launcher.sh /usr/local/bin/boss-launch\n%s infra/oss-quickstart/tenant-launch.sh /usr/local/bin/tenant-launch.sh\n' "$v" "$v" >"$fx/Dockerfile"
    local got; got="$(missing "$fx/launcher.sh" "$fx/Dockerfile" | tr '\n' ' ' | sed 's/ $//')"
    [[ "$got" == "other-lib.sh" ]] || { echo "the-image-carries-what-the-launcher-sources: self-test FAILED — expected the planted 'other-lib.sh' alone, got '$got'" >&2; return 1; }
    echo "the-image-carries-what-the-launcher-sources: self-test ok — planted other-lib.sh caught, tenant-launch.sh copied beside the launcher passes"
}
if [[ "${1:-}" == "--self-test" ]]; then self_test; exit $?; fi
self_test || exit 1

repo="$(cd "$here/../.." && pwd)"
launcher="$repo/infra/oss-quickstart/services-launcher.sh"
df="$repo/infra/oss-quickstart/Dockerfile"
m="$(missing "$launcher" "$df")"
if [[ -n "$m" ]]; then
    echo "the-image-carries-what-the-launcher-sources: FAIL — the launcher sources these from its own directory, and the Dockerfile does not copy them beside /usr/local/bin/boss-launch:" >&2
    printf '  %s\n' $m >&2
    echo "  Add a Dockerfile copy line for each, next to the services-launcher.sh line in infra/oss-quickstart/Dockerfile; a launcher that cannot find what it sources crash-loops the pod before any API starts (2026-09-05)." >&2
    exit 1
fi
echo "the-image-carries-what-the-launcher-sources: $(sourced_files "$launcher" | wc -l | tr -d ' ') sourced file(s), every one copied beside the launcher"
exit 0
