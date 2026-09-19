#!/usr/bin/env bash
#
# the-image-carries-what-build-scripts-read — a crate that reads a file
# outside itself at BUILD time only works if the image's build stage
# COPYs that file.
#
# THE INCIDENT (2026-09-09) — twice, in consecutive trains, and neither
# was visible because the converge built quietly (backlog ddb0f7bd).
#
#   Train 283 added infra/forge/host-absent-tools.txt, which boss-cli's
#   gate.rs `include_str!`s in PRODUCTION code. Four converges failed
#   from 03:46.
#   Train 284 added boss-dispatcher-handlers/build.rs, which derives the
#   observer's spool directory and cap from infra/estate/observe-lib.sh
#   so the shell and Rust halves cannot drift — a §9a collapse, and the
#   right call. Every converge from 04:30 failed on its panic.
#
# Both cars were CORRECT and both gated GREEN, because the gate builds
# from a full checkout and `infra/oss-quickstart/Dockerfile`'s
# rust-build stage copies a SUBSET of the repo. That is the
# "a green gate only covers what it runs" class with a new face: the
# gate and the image see different trees, and only one of them ships.
#
# TWO MECHANISMS REACH OUT OF A CRATE, and this checks both. A build.rs
# RUNS inside that stage; an `include_str!` / `include_bytes!` is read
# by rustc while compiling in it. The first fails as a panic, the
# second as a compile error, and both land as an unreadable exit code.
#
# WHY IT DOES NOT TRY TO TELL TEST FROM PRODUCTION. One of the three
# paths today is `include_str!`d only inside a `#[cfg(test)]` module,
# so `--bins` never reads it. Distinguishing them would mean parsing
# Rust with a grep, and it would be wrong the day someone lifts a const
# out of a test module — the failure would return, silently, in the
# place we had just decided not to look. Requiring all of them costs a
# COPY line per file and removes the distinction entirely.
#
# ONLY THE STAGE THAT RUNS CARGO COUNTS (2026-09-19). Train #473 could
# not build its image: boss-core had gained
# `include_str!("../../../../infra/platform/tiers.toml")` and the
# rust-build stage never copied it — yet this lint said "carried",
# because the RUNTIME stage has `COPY infra/platform /opt/boss/...`
# for the services to read at run time and the ancestor probe matched
# that line. A COPY in another stage is a different filesystem; rustc
# never sees it. So the lines this lint reads are the COPYs of the
# stage(s) whose RUN invokes cargo, and nothing after the next FROM.
#
# WHAT IT CANNOT SEE. A path assembled at run time from variables
# rather than written as a literal. It says so rather than implying the
# check is total.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NAME=the-image-carries-what-build-scripts-read
cd "$here/../.."
# shellcheck source=infra/lint/lib/scanned.sh
. "$here/lib/scanned.sh" || exit 3

DOCKERFILE=${BOSS_IMAGE_DOCKERFILE:-infra/oss-quickstart/Dockerfile}
[ -f "$DOCKERFILE" ] || { echo "$NAME: $DOCKERFILE does not exist" >&2; exit 1; }

# The COPY lines of every stage that runs cargo — a stage is the text
# from one FROM to the next, and a stage runs cargo when a non-comment
# line of it invokes `cargo build` or `cargo install`. Buffered per
# stage because the RUN comes after the COPYs it depends on.
build_stage_copies() {
    awk '
        /^FROM[[:space:]]/ { if (cargo) printf "%s", buf; buf = ""; cargo = 0; next }
        /^COPY[[:space:]]/ { buf = buf $0 "\n" }
        /^[^#]*cargo[[:space:]]+(build|install)([[:space:]]|$)/ { cargo = 1 }
        END { if (cargo) printf "%s", buf }
    ' "$DOCKERFILE"
}
BUILD_COPIES=$(build_stage_copies)
if [ -z "$BUILD_COPIES" ]; then
    echo "$NAME: no stage of $DOCKERFILE both runs cargo and COPYs anything — the build stage has moved and this lint no longer knows where rustc reads from" >&2
    exit 1
fi

# Literal out-of-crate reaches, from both mechanisms, resolved to
# repo-relative paths.
reaches() {
    {
        find crates -name build.rs -print0 2>/dev/null \
          | xargs -0 grep -ho '"\(\.\./\)\+[A-Za-z0-9_./-]*"' 2>/dev/null
        grep -rho 'include_\(str\|bytes\)!("\(\.\./\)\+[A-Za-z0-9_./-]*")' crates 2>/dev/null \
          | sed 's/.*("//; s/")$/"/; s/"$//'
    } | tr -d '"' | sed 's|^\(\.\./\)*||' | grep -E '^[a-z]' | sort -u
}

sources=$(( $(find crates -name build.rs 2>/dev/null | wc -l) \
          + $(grep -rl 'include_str!\|include_bytes!' crates 2>/dev/null | wc -l) ))
if [ "$sources" -eq 0 ]; then
    echo "$NAME: found no build.rs and no include_str! under crates/ — this lint refuses to pass by matching nothing" >&2
    exit 1
fi

fail=0
carried=0
for path in $(reaches); do
    [ -e "$path" ] || continue          # not a real file in this tree
    carried=$((carried + 1))
    # The file itself, or ANY ancestor directory, being copied is
    # enough — `COPY examples ./examples` already carries every seed
    # beneath it, and checking only the immediate parent would demand
    # a redundant line for each one.
    covered=0
    probe="$path"
    while [ "$probe" != "." ] && [ "$probe" != "/" ]; do
        # A here-string, not a pipe: grep -q exits at its match and a
        # piped list would SIGPIPE its writer under pipefail (9840e529).
        if grep -qE "^COPY[[:space:]]+${probe}[[:space:]]" <<< "$BUILD_COPIES"; then
            covered=1
            break
        fi
        probe="$(dirname "$probe")"
    done
    [ "$covered" -eq 0 ] || continue
    echo "$NAME: a crate reads $path at build time and the cargo stage of $DOCKERFILE never COPYs it." >&2
    echo "  A build.rs runs INSIDE the image's build stage and an include_str! is read" >&2
    echo "  while compiling in it; that stage is a SUBSET of the repo. This compiles on" >&2
    echo "  any full checkout — including the gate's — and fails in the image, where on" >&2
    echo "  2026-09-09 it stopped delivery twice in one hour and on 2026-09-19 once more." >&2
    echo "  A COPY in the runtime stage does not count: rustc never sees that filesystem." >&2
    echo "  Add, in the stage that runs cargo:  COPY $path ./$path" >&2
    fail=1
done

[ "$fail" -eq 0 ] || exit 1
lint_scanned "$NAME" "$carried" "out-of-crate path(s) read at build time"
echo "$NAME: every out-of-crate path read at build time ($carried) is carried into the image"
