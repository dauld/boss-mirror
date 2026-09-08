#!/usr/bin/env bash
# session-key-persists.sh — the gateway's session key must be loaded
# from a path on a volume that survives a restart.
#
# WHY THIS EXISTS (2026-08-26). `BOSS_SESSION_KEY` names a KEY and holds
# a PATH:
#
#     let session_key_path: std::path::PathBuf = std::env::var("BOSS_SESSION_KEY")
#         .unwrap_or_else(|_| "/var/lib/boss-gateway/session.key".into())
#         .into();
#
# The Deployment supplied it from a Secret — 64 hex characters that look
# exactly like a signing key. The gateway took those characters as a
# RELATIVE PATH, resolved them against the container's working directory,
# found no file there, minted a fresh random key, and wrote it to a file
# literally named after the secret on the ephemeral overlay filesystem.
#
# Nothing failed. Nothing logged an error. Every deploy simply issued a
# new signing key, so every session cookie in existence stopped
# verifying and every operator was signed out. There were three deploys
# on the day this was found, and the symptom reported was "again" — a
# person being bounced to /login and assuming the site was down. The
# site was up the whole time.
#
# Two properties are pinned, and the second is the one that matters:
#
#   1. the variable is not fed from a Secret — a secret VALUE handed to
#      the ENV is silently read as a filename, which is the whole bug;
#   2. the path it names lives under a mountPath backed by a
#      persistentVolumeClaim OR a Secret volume — because a key written
#      to the container filesystem is a new key on every restart, which
#      is the same outage wearing a different hat.
#
# Note the two senses of "secret" here, which are opposite: property (1)
# forbids a secret VALUE on the ENV (secretKeyRef — the value lands as a
# nonexistent filename); property (2) ALLOWS a secret VOLUME (a real
# file the key is read from). session.key moved from the boss-auth PVC
# to the read-only `boss-session-key` Secret, which projects identical
# bytes into every pod — the RollingUpdate-correct form of persistence,
# not just restart-safe but same-across-pods.
#
# The default in main.rs (/var/lib/boss-gateway/session.key) satisfies
# neither in this deployment: nothing is mounted there. So "just unset
# it" is not a fix, and this lint says so by checking the manifest
# rather than the code.
set -euo pipefail
cd "$(dirname "$0")/../.."

MANIFEST=infra/cluster/manifests/boss.yaml
VAR=BOSS_SESSION_KEY

[ -f "$MANIFEST" ] || { echo "session-key-persists: $MANIFEST not found" >&2; exit 1; }

# The entry, in either the block or the flow form the file uses.
entry=$(grep -nE "name:[[:space:]]*$VAR([,}[:space:]]|$)" "$MANIFEST" || true)
if [ -z "$entry" ]; then
    echo "session-key-persists: $VAR is not set in $MANIFEST." >&2
    echo "    Unset means the gateway default, and nothing is mounted there," >&2
    echo "    so the key would be regenerated on every restart." >&2
    exit 1
fi
line=${entry%%:*}

# (1) A Secret here is read as a filename, not as a key.
if sed -n "${line},$((line + 2))p" "$MANIFEST" | grep -qE 'valueFrom|secretKeyRef'; then
    echo "session-key-persists: $VAR is supplied from a Secret." >&2
    echo "    It is a PATH, not a key. A secret value lands as a filename," >&2
    echo "    the file does not exist, and the gateway mints a new signing" >&2
    echo "    key on every restart — signing out every session." >&2
    echo "    Give it a path on the persisted auth volume instead." >&2
    exit 1
fi

value=$(sed -n "${line}p" "$MANIFEST" \
    | sed -E 's/.*value:[[:space:]]*//; s/[},].*$//; s/^"//; s/"$//' \
    | tr -d "[:space:]")

case "$value" in
    /*) ;;
    *)  echo "session-key-persists: $VAR is '$value', which is not an absolute path." >&2
        echo "    A relative path resolves against the container working" >&2
        echo "    directory and is lost on restart." >&2
        exit 1 ;;
esac

# (2) Which mountPaths are actually backed by a volume that survives a
# restart with the SAME bytes — a persistentVolumeClaim, or a Secret
# volume (projected identically into every pod). The `secret:` match is
# anchored to `{` or end-of-line so it catches the volume key and NOT
# `secretKeyRef`/`secretName`/`imagePullSecrets`.
persisted_volumes=$(awk '
    /^[[:space:]]*-[[:space:]]*name:[[:space:]]*[A-Za-z0-9_-]+[[:space:]]*$/ {
        sub(/^[^:]*:[[:space:]]*/, ""); gsub(/[[:space:]]/, "");
        cur = $0; next
    }
    /persistentVolumeClaim/ { if (cur != "") { print cur; cur = "" } }
    /^[[:space:]]*secret:[[:space:]]*(\{|$)/ { if (cur != "") { print cur; cur = "" } }
' "$MANIFEST" | sort -u)

persisted=""
for v in $persisted_volumes; do
    # `head -1` BEFORE stripping whitespace, not after: a volume can be
    # mounted by more than one container (boss-auth is mounted twice
    # here), and a whitespace strip across a two-line stream deletes the
    # newline and welds the paths into one nonexistent directory.
    p=$(grep -oE "\{name:[[:space:]]*$v,[[:space:]]*mountPath:[[:space:]]*[^,}]+" "$MANIFEST" \
        | sed -E 's/.*mountPath:[[:space:]]*//' | head -1 | tr -d "[:space:]")
    [ -n "$p" ] && persisted="$persisted $p"
done

if [ -z "$persisted" ]; then
    echo "session-key-persists: no PVC- or Secret-backed mountPath found in $MANIFEST." >&2
    echo "    Either the volumes moved or this lint's parse is stale; fix" >&2
    echo "    the lint rather than deleting it — the failure it guards is" >&2
    echo "    silent." >&2
    exit 1
fi

for p in $persisted; do
    case "$value" in
        "$p"/*) echo "session-key-persists: $value is on persisted volume $p"; exit 0 ;;
    esac
done

echo "session-key-persists: $VAR is '$value', which is not under a persisted volume." >&2
echo "    Persisted mountPaths in this manifest:$persisted" >&2
echo "    A key on the container filesystem is a NEW key after every" >&2
echo "    restart, and every existing session stops verifying." >&2
exit 1
