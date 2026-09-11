#!/usr/bin/env bash
# The internal CI must check out from the forge, not github.com.
#
# `uses: actions/checkout@...` is a JavaScript action the Forgejo
# runner pulls from github.com before the job's first line runs. The
# runner's container DNS has no redundancy — 127.0.0.11 to a single
# upstream with no secondary, the same hop build-image documents its
# retry loop against — so one dropped lookup there reds the whole train
# with no car at fault. That is backlog d9a34560, and it is the exact
# shape of the 2026-08-26 build-image DNS flake that took a six-car
# train down while every car was clean.
#
# The repo lives on the forge at a LAN IP, so a job can clone it by IP
# over git and resolve no name at all. This guards the INTERNAL
# workflow only: .github/workflows/ci.yml runs on GitHub, where
# actions/checkout is a local action and correct.
set -euo pipefail

f=".forgejo/workflows/ci.yml"
[ -f "$f" ] || { echo "ci-checks-out-from-the-forge: $f is missing"; exit 1; }

if grep -nE 'uses:[[:space:]]*actions/checkout' "$f"; then
    echo
    echo "ci-checks-out-from-the-forge: the line(s) above fetch actions/checkout"
    echo "  from github.com — a single-lookup DNS dependency that reds the whole"
    echo "  train on a blip (d9a34560). Check the repo out from the forge instead:"
    echo
    echo '      - name: Check out from the forge, not github.com'
    echo '        env:'
    echo '          FORGE_TOKEN: ${{ github.token }}'
    echo '        run: |'
    echo '          set -eu'
    echo '          git init -q .'
    echo '          git remote add origin \'
    echo '            "http://x-access-token:${FORGE_TOKEN}@10.20.0.15:3000/${GITHUB_REPOSITORY}.git"'
    echo '          git fetch -q --depth=1 origin "${GITHUB_REF}"'
    echo '          git checkout -q --force FETCH_HEAD'
    exit 1
fi

echo "ci-checks-out-from-the-forge: OK — $f resolves no github.com action for checkout"
