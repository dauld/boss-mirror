#!/usr/bin/env bash
# cluster-deploy-lib — the parts of the cluster converge that decide
# WHERE the cluster lands, as functions a stub kubectl can exercise.
# Sourced by cluster-deploy-runner.sh; tested by
# infra/lint/the-converge-rolls-back-to-a-named-build.sh on every gate.
#
# WHY. On 2026-09-05 a head bricked its boot and the runner's guard
# fired correctly — then rolled back to PRE_REV, the revision its own
# `kubectl apply` had just created from the manifest's literal image
# tag: a month-old build whose jobs API cannot start against today's
# registry. The cluster stayed dark for four hours on a "rollback" that
# rolled to the wrong place (packet: the converge rolls back to the
# manifest's placeholder). CLAUDE.md §Diagnosis: "roll back" is a
# TARGET, not a verb. The target is the last CONVERGED build — the sha
# the runner itself stamps after every successful roll — named by image,
# verified Ready.
#
# Two more things follow from the same night. The manifest's image
# field is a placeholder the apply step used to put on the cluster for
# the seconds between apply and patch — a dark window on an old build
# on every converge; `manifests_with_image` writes the converged sha
# into the applied copy instead, so the apply never changes what runs.
# And a head that cannot even find what its launcher sources should
# never reach the cluster: `image_boots` runs the image's own launcher
# check before anything is applied.

# THE CONVERGE APPLIES `*.yaml` AND NOTHING ELSE — one definition, two
# readers (backlog e37a833d).
#
# `manifests_with_image` stages the apply directory with
# `cp "$src"/*.yaml`, so a manifest committed as `.yml` or `.json`, filed
# in a subdirectory, or named with a leading dot is in the tree,
# reviewed, merged — and never reaches the cluster. Nothing said so: no
# lint, no converge warning, and `kubectl apply` has no `--prune`, so
# nothing downstream can see the omission either. That is a change that
# looks delivered and is not, and it is indistinguishable from a manifest
# that has merely not converged YET.
#
# `.yaml` stays the only legal name rather than the converge widening,
# because SEVEN readers derive "what the tree declares to the cluster"
# from this directory, each with its own `*.yaml` (undeclared-objects.sh,
# check-manifests-applied.sh, the orphan lint's properties A and B,
# a-workload-declares-the-user-it-runs-as.sh, timers-leave-a-packet.sh
# twice, and this function). Widening leaves all seven free to disagree
# about the set; refusing makes `*.yaml` select the WHOLE directory, so
# they are equal by construction instead of by seven edits or seven pins
# (CLAUDE.md §9a prefers the collapse).
#
# manifests_the_converge_ignores DIR
#   Print every entry of DIR that `cp "$DIR"/*.yaml` would leave behind,
#   one path per line. Nothing printed = the converge applies the whole
#   directory. `*.md` is documentation and not a declaration, so it is
#   the one thing deliberately not applied; everything else — `.yml`,
#   `.json`, a nested directory, a dotfile that `*` never expands onto —
#   is something the apply drops on the floor.
#
#   Read by infra/lint/a-manifest-the-converge-ignores-is-refused.sh,
#   which is how this never fires in a converge: it fails the gate first.
manifests_the_converge_ignores() {
    local dir="$1"
    # A SUBSHELL, so `shopt` does not leak into the runner that sources
    # this. dotglob is on because `*.yaml` never expands onto a leading
    # dot: `.boss.yaml` ENDS in .yaml and is still not applied, which is
    # the spelling that survives a naive fix.
    (
        shopt -s nullglob dotglob
        local entry
        for entry in "$dir"/*; do
            case "${entry##*/}" in
                .*) printf '%s\n' "$entry"; continue ;;
            esac
            # A nested directory is the same defect: neither the `cp` nor
            # `kubectl apply -f <dir>` recurses.
            if [ -d "$entry" ]; then
                printf '%s\n' "$entry"
                continue
            fi
            case "$entry" in
                *.yaml | *.md) continue ;;
                *) printf '%s\n' "$entry" ;;
            esac
        done
    )
}

# manifests_with_image SRC_DIR DST_DIR REGISTRY TAG
#   Copy the manifests, rewriting the boss image tag to TAG so the
#   apply carries the build that is already converged. TAG "none"
#   (first ever converge) leaves the files untouched.
#
#   REFUSES, before it copies anything, a directory it could only partly
#   apply: a converge that applies 20 of 21 manifests and exits 0 claims
#   a convergence it did not perform. Refusing is self-clearing (rename
#   the file to `.yaml`) and loud, where the old behaviour was silent.
manifests_with_image() {
    local src="$1" dst="$2" registry="$3" tag="$4" ignored
    ignored=$(manifests_the_converge_ignores "$src")
    if [ -n "$ignored" ]; then
        echo "cluster-deploy-runner: REFUSING to stage $src — the apply copies *.yaml only, so these would never reach the cluster:" >&2
        printf '%s\n' "$ignored" | sed 's/^/  /' >&2
        echo "  Rename each to .yaml (the only name the converge applies), or move it out of the manifests directory." >&2
        echo "  Nothing was staged: a partial apply directory reads as a full convergence." >&2
        return 1
    fi
    mkdir -p "$dst"
    cp "$src"/*.yaml "$dst"/ || return 1
    [ "$tag" = "none" ] && return 0
    local f
    for f in "$dst"/*.yaml; do
        sed -i -E "s#(image: ${registry}):[A-Za-z0-9._-]+#\1:${tag}#g" "$f"
    done
}

# image_boots DOCKER IMAGE
#   The image's launcher checks itself (services-launcher.sh --check:
#   every file it sources is beside it). Non-zero = do not roll it.
image_boots() {
    local docker="$1" image="$2"
    $docker run --rm --entrypoint /usr/local/bin/boss-launch "$image" --check
}

# roll_deployment K REGISTRY HEAD LAST_GOOD FAILED_FILE [NAMESPACE]
#   Patch deploy/boss to REGISTRY:HEAD and wait for Ready. If it never
#   goes Ready: quarantine HEAD in FAILED_FILE and roll back to the
#   NAMED target — REGISTRY:LAST_GOOD when a converged build is known,
#   else the image that was running before the patch — then wait for
#   that to be Ready. Returns 0 only when HEAD is serving.
#
#   NAMESPACE defaults to `boss`, the source instance. Every other
#   instance (infra/cluster/instances.toml; backlog 07d7549c) runs the
#   same deploy/boss in its own namespace and is rolled by this same
#   function — one definition of "roll, prove Ready, or roll back to a
#   named build", not one per instance. The runner hands each instance
#   its OWN quarantine file: a head that booted on prod and fails on the
#   playground is the playground's environment (its secrets, its
#   volumes), not a bricked build, and must not hold prod's next
#   converge.
roll_deployment() {
    local k="$1" registry="$2" head="$3" last_good="$4" failed_file="$5" ns="${6:-boss}"
    local pre_image
    pre_image=$($k get deploy boss -n "$ns" -o jsonpath='{.spec.template.spec.containers[0].image}')
    _patch_boss_image "$k" "$registry:$head" "$ns"
    if $k rollout status deploy/boss -n "$ns" --timeout=420s; then
        return 0
    fi
    echo "$head" > "$failed_file"
    local target
    if [ -n "$last_good" ] && [ "$last_good" != "none" ]; then
        target="$registry:$last_good"
    else
        target="$pre_image"
    fi
    echo "cluster-deploy-runner: $head never went Ready in $ns — rolling back to $target (the last converged build, by name)" >&2
    if _patch_boss_image "$k" "$target" "$ns" \
        && $k rollout status deploy/boss -n "$ns" --timeout=300s; then
        echo "cluster-deploy-runner: rolled back — cluster serves $target in $ns; $head is quarantined there (rm $failed_file to retry it)" >&2
    else
        echo "cluster-deploy-runner: ROLLBACK OF $ns TO $target ALSO FAILED — the instance needs hands NOW" >&2
    fi
    return 1
}

_patch_boss_image() {
    local k="$1" image="$2" ns="${3:-boss}"
    $k patch deploy boss -n "$ns" --type=json \
        -p "[{\"op\":\"replace\",\"path\":\"/spec/template/spec/containers/0/image\",\"value\":\"$image\"},{\"op\":\"replace\",\"path\":\"/spec/template/spec/initContainers/0/image\",\"value\":\"$image\"}]"
}

# AN INSTANCE WITHOUT ITS SECRETS IS SKIPPED, BY NAME (backlog 07d7549c;
# car cb784b3a held on the dock 2026-09-16). Every Secret an instance's
# manifests reference is minted out of tree, once per namespace, by
# David. As first built the runner applied the playground and then
# waited 420 s + 300 s on a rollout its pods could not start, and every
# train's converge ended "Maintenance failed" until that ceremony —
# a scheduled alarm, not reliability. So before a second instance is
# applied the runner asks whether the Secrets it needs exist, and an
# instance missing any is skipped whole: the names ride the converge
# packet, the shapes to mint are printed (names and keys, never values),
# and the converge exits 0 with prod applied, rolled, stamped and
# verified exactly as before.
#
# manifest_secrets K PATH
#   The Secret names the objects at PATH (a file or a directory, as the
#   kubectl K sees it) REQUIRE: every env secretKeyRef, every secret
#   volume and every imagePullSecret, minus those marked `optional:
#   true` — the pod boots without those. DERIVED from the manifests
#   through kubectl's own parser (client dry run) and jq, never listed
#   here: a list would be the §9a pair that drifts the day a manifest
#   gains a reference. One name per line, sorted, unique.
manifest_secrets() {
    local k="$1" path="$2"
    # -s slurps kubectl's document stream into one array, so `unique`
    # spans every object rather than each document on its own.
    $k create --dry-run=client -o json -f "$path" | jq -rs '
        [ .[] | .. | objects | (
            (select(has("secretKeyRef")) | .secretKeyRef | select(.optional != true) | .name),
            (select(has("secretName")) | select(.optional != true) | .secretName),
            (select(has("imagePullSecrets")) | .imagePullSecrets[]? | .name)
        ) ] | unique | .[]'
}

# manifest_secret_keys K PATH
#   name<TAB>key for every required secretKeyRef at PATH — what the
#   `--from-literal` shapes below are built from.
manifest_secret_keys() {
    local k="$1" path="$2"
    $k create --dry-run=client -o json -f "$path" | jq -rs '
        [ .[] | .. | objects | select(has("secretKeyRef")) | .secretKeyRef
          | select(.optional != true) | "\(.name)\t\(.key)" ] | unique | .[]'
}

# secret_presence K NS NAME — the ONE read of "does this Secret exist",
#   for every loop in this file (CLAUDE.md §9a: instance_secret_gate and
#   connector_status each carried their own copy until 51c98681 needed a
#   third). Prints exactly one of:
#     present            the credential read it
#     absent             the server said not found — the Secret, or the
#                        whole namespace on a first converge; both are
#                        "it is not there"
#     cannot: REASON     a read the credential could not make (Forbidden,
#                        a dead apiserver) — neither answer, and the
#                        caller must not treat it as one
#   A here-string, not a pipe, for the grep: under pipefail a multi-line
#   error piped into `grep -q` can lose the race to SIGPIPE and read as
#   "cannot" (the #361-era gate flake).
secret_presence() {
    local k="$1" ns="$2" name="$3" out
    if out=$($k get secret -n "$ns" "$name" 2>&1); then
        echo present
        return 0
    fi
    if grep -qi 'not found' <<< "$out"; then
        echo absent
        return 0
    fi
    echo "cannot: $(head -n 1 <<< "$out")"
}

# instance_secrets_absent K KM NS PATH — the QUIET half of the gate.
#   Prints the required Secrets (manifest_secrets of PATH) that NS lacks,
#   space-separated in that order, empty when none, and returns 0; 2
#   when a read the credential could not make (the reason on stderr).
#   No verdict, no shapes: the runner asks this BEFORE it provisions
#   (provision_instance_secrets below) and instance_secret_gate after,
#   so a Secret the converge is about to mint is never printed as one a
#   person must (backlog dc1bc724).
instance_secrets_absent() {
    local k="$1" km="$2" ns="$3" path="$4"
    local required name answer absent=""
    required=$(manifest_secrets "$km" "$path") || return 2
    for name in $required; do
        answer=$(secret_presence "$k" "$ns" "$name")
        case "$answer" in
            present) ;;
            absent) absent="$absent $name" ;;
            *)  echo "cluster-deploy-runner: cannot read Secret $ns/$name — ${answer#cannot: }" >&2
                return 2 ;;
        esac
    done
    echo "${absent# }"
}

# instance_secret_gate K KM NS PATH
#   0  every required Secret exists in NS — apply the instance
#   1  at least one is absent — SKIP the instance; stdout is the absent
#      names, space-separated (for the packet), and stderr carries the
#      `kubectl create secret` shape for each (names and keys only).
#      A namespace that does not exist yet answers NotFound for every
#      Secret, which is the truth on the first converge after landing.
#   2  a read the credential could not make — CANNOT TELL, which is
#      neither "present" nor "absent" (CLAUDE.md §Doors: a wrong target
#      answers instead of erroring; this one refuses instead).
#   K reads the cluster; KM is the kubectl that can see PATH (the
#   runner's manifests mount) — the same command when there is one.
instance_secret_gate() {
    local k="$1" km="$2" ns="$3" path="$4"
    local name absent keys
    absent=$(instance_secrets_absent "$k" "$km" "$ns" "$path") || return 2
    [ -n "$absent" ] || return 0
    echo "$absent"
    keys=$(manifest_secret_keys "$km" "$path")
    echo "cluster-deploy-runner: instance $ns SKIPPED — these Secrets are not minted in it: $absent" >&2
    echo "  Secrets never live in the tree (infra/cluster/manifests/README.md). Mint each once, in the" >&2
    echo "  namespace, with prod's copy as the shape; the instance applies on the next converge:" >&2
    for name in $absent; do
        local literals=""
        while IFS=$'\t' read -r n key; do
            [ "$n" = "$name" ] && literals="$literals --from-literal=$key=..."
        done <<< "$keys"
        if [ -n "$literals" ]; then
            echo "    kubectl -n $ns create secret generic $name$literals" >&2
        elif [ "$name" = forgejo-registry ] || printf '%s' "$name" | grep -q 'registry'; then
            echo "    kubectl -n $ns create secret docker-registry $name --docker-server=... --docker-username=... --docker-password=..." >&2
        else
            echo "    kubectl -n $ns create secret generic $name --from-file=<key>=<file>   # keys: kubectl -n boss get secret $name -o jsonpath='{.data}' | jq keys" >&2
        fi
    done
    return 1
}

# PROVISIONING MINTS AN INSTANCE'S OWN SECRETS (backlog dc1bc724; David
# 2026-09-16: no hand work unless absolutely required). The gate above
# was built with every Secret a person's to mint, and the playground
# was skipped on every converge from 2026-09-15 for want of that
# ceremony. Measured against its render (six names, five keys), the
# six are not one kind of thing, and only one of them is a person's:
#
#   internal  boss-secrets (postgres-password, admin-password, and
#             database-url composed from the password and the
#             instance's OWN postgres Service), boss-session-key —
#             random bytes NOBODY needs to know. Minted here, once,
#             when absent; the values ride kubectl's stdin, never its
#             argv and never this journal (names only).
#   shared    forgejo-registry (the registry pull credential), resend
#             (the mail sender) — the SAME credential as the source
#             instance's. Copied from the namespace instances.toml
#             names as `shares_with`: type and data, none of the
#             source object's identity. No `shares_with`: left absent,
#             named, and the gate names them to a person.
#   root      boss-oidc (client-secret) — the Kanidm OIDC client the
#             identity provider issues; the estate cannot mint it.
#             MEASURED in boss-gateway oidc.rs OidcConfig::from_env: an
#             EMPTY client secret is "no OIDC" and the gateway boots
#             with guest sessions and local auth; an ABSENT Secret
#             wedges the pod (the ref is not optional). So the object
#             is created with every key the manifests read present and
#             EMPTY — the way ensure_declared_secrets creates a broker
#             Secret — and the ceremony that registers the client fills
#             it. Until then the instance is guest-only, which is what
#             a public example needs.
#   by hand   everything else — today boss-tls, the lego certificate,
#             whose reference leaves with 974d2015 (everything behind
#             the tunnel). Not created; the gate still names it.
#
# The class is a decision per NAME and lives here beside its recipe,
# not on a roster: a roster line could not carry how a value is made,
# and a second copy of the class would be the §9a pair. The KEYS an
# internal Secret needs are still DERIVED from the manifests, and a key
# the recipe does not know refuses by name — a Secret minted without a
# key the pod reads is the same wedge one step later.

# instance_secret_class NAME — internal | shared | root | by-hand
instance_secret_class() {
    case "$1" in
        boss-secrets | boss-session-key) echo internal ;;
        forgejo-registry | resend) echo shared ;;
        boss-oidc) echo root ;;
        *) echo by-hand ;;
    esac
}

# _random_hex N — N random bytes as 2N hex characters, from the kernel.
#   coreutils only (od), so the forge host needs nothing new; hex is
#   URL-safe, which is what lets the password ride inside database-url
#   unescaped.
_random_hex() {
    od -An -tx1 -v -N "$1" /dev/urandom | tr -d ' \n'
}

# _postgres_endpoint KM PATH — service<TAB>port<TAB>user<TAB>db of the
#   postgres StatefulSet in the rendered manifests at PATH: the
#   StatefulSet's serviceName, and from the container carrying
#   POSTGRES_USER its first containerPort, POSTGRES_USER and
#   POSTGRES_DB. Empty when there is none.
_postgres_endpoint() {
    local km="$1" path="$2"
    $km create --dry-run=client -o json -f "$path" | jq -rs '
        [ .[] | select(.kind == "StatefulSet") | . as $s
          | .spec.template.spec.containers[]
          | select(any(.env[]?; .name == "POSTGRES_USER"))
          | [ $s.spec.serviceName,
              (.ports[0].containerPort | tostring),
              (.env[] | select(.name == "POSTGRES_USER") | .value),
              (.env[] | select(.name == "POSTGRES_DB") | .value) ]
          | @tsv ] | first // empty'
}

# _secret_json NS NAME TYPE KEYMAP — a Secret object as JSON. KEYMAP is
#   key<TAB>variable per line; each data[key] is that ENVIRONMENT
#   variable's value, base64 by jq. Values pass through the environment
#   and stdin only — `ps` shows neither — and this journal never
#   prints the object.
_secret_json() {
    local ns="$1" name="$2" type="$3" keymap="$4"
    KEYMAP="$keymap" jq -n --arg ns "$ns" --arg name "$name" --arg type "$type" '
        { apiVersion: "v1", kind: "Secret", type: $type,
          metadata: { name: $name, namespace: $ns },
          data: ( [ $ENV.KEYMAP | split("\n")[] | select(length > 0) | split("\t")
                    | { key: .[0], value: ($ENV[.[1]] | @base64) } ]
                  | from_entries ) }'
}

# _create_secret KI JSON — the ONE create: the object on stdin of the
#   kubectl that reads stdin (the runner's KAPPLY; a `docker run`
#   without -i reads nothing, which was the step-plugins bug). `create`,
#   not `apply`: an object that exists is AlreadyExists, never
#   rewritten. Prints nothing on success; on failure the first line of
#   kubectl's error — which names the object and the reason, never its
#   data — and returns 1.
_create_secret() {
    local ki="$1" json="$2" out
    if out=$(printf '%s' "$json" | $ki create -f - 2>&1); then
        return 0
    fi
    head -n 1 <<< "$out"
    return 1
}

# provision_instance_secrets K KI KM NS PATH SHARE_NS ABSENT
#   For each Secret in ABSENT (instance_secrets_absent's answer for NS
#   against the rendered manifests at PATH): mint, copy from SHARE_NS
#   (a namespace, or empty when the instance shares with nobody),
#   create empty, or leave alone — by instance_secret_class. K reads
#   the cluster (the source's copy of a shared Secret), KI creates from
#   stdin, KM sees PATH. Prints ONE line for the converge packet:
#     minted a, b | copied c, d from SRC | empty e (…) | cannot f (REASON), … | by hand g, …
#   (each part only when non-empty; `nothing absent` for an empty
#   ABSENT). Returns 0 in every case: the line is the verdict, the gate
#   that follows decides whether the instance applies, and a Secret the
#   converge could not make is named there too. The runner's journal
#   carries this line and nothing else about these Secrets.
provision_instance_secrets() {
    local k="$1" ki="$2" km="$3" ns="$4" path="$5" share_ns="$6" absent="$7"
    local name keys required key bad json err endpoint keymap
    local svc port user db pw
    local minted="" copied="" empty="" cannot="" byhand=""
    if [ -z "$absent" ]; then
        echo "nothing absent"
        return 0
    fi
    keys=$(manifest_secret_keys "$km" "$path") || keys=""
    for name in $absent; do
        required=$(awk -F'\t' -v n="$name" '$1 == n { print $2 }' <<< "$keys")
        case "$(instance_secret_class "$name")" in
            internal)
                case "$name" in
                    boss-secrets)
                        # Every key the manifests read must be one the
                        # recipe makes; a recipe is not a guess.
                        bad=""
                        for key in $required; do
                            case "$key" in
                                postgres-password | admin-password | database-url) ;;
                                *) bad="$key" ;;
                            esac
                        done
                        if [ -n "$bad" ]; then
                            cannot="${cannot:+$cannot, }$name (the manifests read key $bad, which the converge has no recipe for)"
                            continue
                        fi
                        endpoint=$(_postgres_endpoint "$km" "$path")
                        if [ -z "$endpoint" ]; then
                            cannot="${cannot:+$cannot, }$name (no postgres StatefulSet in the rendered manifests to compose database-url from)"
                            continue
                        fi
                        IFS=$'\t' read -r svc port user db <<< "$endpoint"
                        pw=$(_random_hex 24)
                        # The URL the boss pod and every chore read
                        # (boss.yaml: DATABASE_URL / BOSS_POSTGRES_URL),
                        # pointing INTO this instance by its own
                        # Service's cluster DNS name — the name
                        # render-instance.sh substitutes per namespace.
                        json=$(V_PG="$pw" V_ADMIN="$(_random_hex 24)" \
                            V_URL="postgres://$user:$pw@$svc.$ns.svc.cluster.local:$port/$db" \
                            _secret_json "$ns" "$name" Opaque \
                            $'postgres-password\tV_PG\nadmin-password\tV_ADMIN\ndatabase-url\tV_URL') ;;
                    boss-session-key)
                        # boss.yaml projects key `session.key` to the
                        # path BOSS_SESSION_KEY names; boss-gateway
                        # load_or_create_session_key reads it as hex and
                        # wants at least 32 bytes decoded — 32 random
                        # bytes as 64 hex characters, the prod shape.
                        json=$(V_KEY="$(_random_hex 32)" \
                            _secret_json "$ns" "$name" Opaque $'session.key\tV_KEY') ;;
                esac
                if err=$(_create_secret "$ki" "$json"); then
                    minted="${minted:+$minted, }$name"
                else
                    cannot="${cannot:+$cannot, }$name ($err)"
                fi ;;
            shared)
                if [ -z "$share_ns" ]; then
                    cannot="${cannot:+$cannot, }$name (shared, but [$ns] declares no shares_with in instances.toml)"
                    continue
                fi
                # The object, not its presence (secret_presence is the
                # one presence read): type and data survive, the
                # source's identity — uid, resourceVersion, timestamps,
                # managedFields, the last-applied annotation — does not.
                if ! json=$($k get secret "$name" -n "$share_ns" -o json 2>&1); then
                    if grep -qi 'not found' <<< "$json"; then
                        cannot="${cannot:+$cannot, }$name (not found in $share_ns)"
                    else
                        cannot="${cannot:+$cannot, }$name ($(head -n 1 <<< "$json"))"
                    fi
                    continue
                fi
                json=$(jq --arg ns "$ns" '
                    { apiVersion, kind, type, data,
                      metadata: ({ name: .metadata.name, namespace: $ns }
                                 + (if .metadata.labels then { labels: .metadata.labels } else {} end)) }' <<< "$json")
                if err=$(_create_secret "$ki" "$json"); then
                    copied="${copied:+$copied, }$name"
                else
                    cannot="${cannot:+$cannot, }$name ($err)"
                fi ;;
            root)
                # Every key the manifests read, present and empty.
                keymap=""
                for key in $required; do
                    keymap="$keymap$key"$'\t'"V_EMPTY"$'\n'
                done
                json=$(V_EMPTY="" _secret_json "$ns" "$name" Opaque "$keymap")
                if err=$(_create_secret "$ki" "$json"); then
                    empty="${empty:+$empty, }$name"
                else
                    cannot="${cannot:+$cannot, }$name ($err)"
                fi ;;
            *)  byhand="${byhand:+$byhand, }$name" ;;
        esac
    done
    local line=""
    [ -z "$minted" ] || line="minted $minted"
    [ -z "$copied" ] || line="${line:+$line | }copied $copied from $share_ns"
    [ -z "$empty" ] || line="${line:+$line | }empty $empty (guest sessions only until a Kanidm client exists — root ceremony)"
    [ -z "$cannot" ] || line="${line:+$line | }cannot $cannot"
    [ -z "$byhand" ] || line="${line:+$line | }by hand $byhand"
    echo "$line"
    return 0
}

# THE TUNNEL CONNECTOR REPORTS AFTER THE ROLL (backlog 5a2bb0ce; design
# 4c565f8c, David 2026-09-16). Every public hostname reaches the cluster
# through a Cloudflare Tunnel whose connector is a declared prod
# workload (infra/cluster/manifests/cloudflared.yaml). Its credentials
# are one Secret minted once by David; the connector is applied with
# prod's manifests whether or not that Secret exists, and prod's apply
# has no secret gate (the gate above is for a SECOND instance). So the
# converge reads the connector's state after the roll and puts it on
# the packet, where the car's probe reads it — never in the journal
# alone (CLAUDE.md §Diagnosis: a check nobody reads is not running).
#
# connector_status K KM NS PATH DEPLOY
#   Print exactly one line — the connector's state as this converge read
#   it — and return 0 in every case: the tunnel is not what a train
#   delivers, and a converge that failed on a Cloudflare-side fault
#   would hold every train's packet red for something no train can fix.
#     connected                        the rollout completed: every replica
#                                      Ready, and readiness IS cloudflared's
#                                      own /ready, 200 only with a live
#                                      edge connection
#     not-ready                        the rollout did not complete in time
#                                      — the Secret exists; read the pods
#     skipped (secret absent: NAMES)   a Secret the manifest at PATH requires
#                                      is not minted in NS; the pods cannot
#                                      start, and no rollout is waited on
#     unknown (REASON)                 a read the credential could not make
#   The Secret names are DERIVED from the rendered manifest at PATH
#   through manifest_secrets, exactly as instance_secret_gate derives
#   an instance's — never listed here (§9a). K reads the cluster; KM is
#   the kubectl that can see PATH (the runner's manifests mount).
connector_status() {
    local k="$1" km="$2" ns="$3" path="$4" deploy="$5"
    local required name answer absent=""
    if ! required=$(manifest_secrets "$km" "$path"); then
        echo "unknown (cannot derive the Secrets from $path)"
        return 0
    fi
    for name in $required; do
        answer=$(secret_presence "$k" "$ns" "$name")
        case "$answer" in
            present) ;;
            absent) absent="$absent $name" ;;
            *)  echo "unknown (cannot read Secret $ns/$name: ${answer#cannot: })"
                return 0 ;;
        esac
    done
    if [ -n "$absent" ]; then
        echo "skipped (secret absent:$absent)"
        return 0
    fi
    if $k rollout status "deploy/$deploy" -n "$ns" --timeout=120s >/dev/null 2>&1; then
        echo connected
    else
        echo not-ready
    fi
}

# skipped_entry NS ABSENT — the ONE spelling of a skipped instance on
#   the packet: `<ns> (secrets absent: a, b)`. Two loops build the
#   `instances_skipped` string (the runner's apply loop, and
#   instances_skipped_by_gate below for the tick that applies nothing)
#   and three readers parse it (infra/cluster/instances-skipped.lib.sh);
#   the entry is spelled here so the two writers cannot drift
#   (CLAUDE.md §9a).
skipped_entry() {
    printf '%s (secrets absent: %s)' "$1" "${2// /, }"
}
# skipped_tenant_entry NS WHY REPO REF — the same spelling for an
#   instance skipped over its TENANT SOURCE (backlog f4f5c387): WHY is
#   `unreadable` (the deploying tick measured that its credential cannot
#   read REPO@REF) or `not delivered` (the no-op tick found no
#   boss-tenant ConfigMap in NS — the instance was never applied). The
#   parser reads the reason as `tenant source unreadable` / `tenant not
#   delivered`; the repo and ref are the detail.
skipped_tenant_entry() {
    case "$2" in
        unreadable) printf '%s (tenant source unreadable: %s@%s)' "$1" "$3" "$4" ;;
        *)          printf '%s (tenant not delivered: %s@%s)' "$1" "$3" "$4" ;;
    esac
}

# THE TENANT SOURCE PER INSTANCE (backlog f4f5c387, car 2 of fcc1d57b;
# David 2026-09-16 'Let's do it'). An instance's tenant is either a
# directory the image ships or a repository on the forge that this
# converge checks out beside the product and delivers as the
# `boss-tenant` ConfigMap — infra/cluster/instances.toml says which,
# and render-instance.sh --instances carries the repo and ref as its
# seventh and eighth columns (and the instance's optional site as the
# ninth). The functions below are the repo half, written so a fixture
# forge can exercise every verdict.
#
# ONE CREDENTIAL: the checkout's own. The runner fetches forge main
# through its `forgejo` remote, and the tenant is read with exactly
# that URL's scheme, host and credential — the repo path replaced,
# nothing else — so the first converge MEASURES whether the runner's
# token can read the tenant repo, and an unreadable one names itself
# on the packet instead of a person guessing at scopes. No URL ever
# reaches the journal: a forge remote may carry its token in the
# userinfo, and every git message is redacted before it is printed.

# _tenant_url_from_remote URL TENANT_REPO — pure string work: the
#   remote's URL with its trailing owner/name[.git] replaced by
#   TENANT_REPO.git. Handles scheme://…/owner/name and host:owner/name.
_tenant_url_from_remote() {
    printf '%s\n' "$1" | sed -E "s#[^/:]+/[^/]+(\.git)?/?\$#$2.git#"
}
# tenant_repo_url REPO TENANT_REPO — the URL for TENANT_REPO, derived
#   from REPO's forgejo remote; rc 2 (nothing printed) when REPO has no
#   such remote — CANNOT DERIVE, never a guess.
tenant_repo_url() {
    local url
    url=$(git -C "$1" remote get-url forgejo 2>/dev/null) || return 2
    [ -n "$url" ] || return 2
    _tenant_url_from_remote "$url" "$2"
}
# redact_url — a filter: the userinfo of every URL on stdin replaced by
#   <redacted>. Every git message about a tenant goes through it.
redact_url() {
    sed -E 's#://[^/@[:space:]]+@#://<redacted>@#g'
}
# tenant_source_check REPO TENANT_REPO REF — MEASURES readability:
#   `git ls-remote` for REF (a branch or a tag) with the derived URL.
#   Prints the sha REF resolves to and returns 0; returns 1 when the
#   repo cannot be read or has no such ref (the reason on stderr,
#   redacted); 2 when no URL can be derived. GIT_TERMINAL_PROMPT=0: a
#   unit has no terminal, and a prompt would hang, not fail.
tenant_source_check() {
    local repo="$1" tenant="$2" ref="$3" url out rc=0
    url=$(tenant_repo_url "$repo" "$tenant") || return 2
    out=$(GIT_TERMINAL_PROMPT=0 git ls-remote --exit-code "$url" "refs/heads/$ref" "refs/tags/$ref" 2>&1) || rc=$?
    if [ "$rc" -ne 0 ]; then
        printf 'tenant %s@%s is not readable with the converge'"'"'s credential (rc %s): %s\n' \
            "$tenant" "$ref" "$rc" "$out" | redact_url >&2
        return 1
    fi
    # A here-string, not a pipe: `head` exits at its first line and a
    # producer still writing is SIGPIPE under pipefail.
    head -n1 <<< "$out" | cut -f1
}
# tenant_checkout REPO TENANT_REPO REF DIR — a fresh shallow clone of
#   REF into DIR, replacing whatever DIR held: a stale checkout answering
#   for a moved ref is the wrong-target class. rc 1 with the reason
#   (redacted) when the clone fails; 2 when no URL can be derived.
tenant_checkout() {
    local url out rc=0
    url=$(tenant_repo_url "$1" "$2") || return 2
    rm -rf "$4"
    out=$(GIT_TERMINAL_PROMPT=0 git clone -q --depth 1 --branch "$3" "$url" "$4" 2>&1) || rc=$?
    if [ "$rc" -ne 0 ]; then
        printf 'tenant %s@%s could not be checked out (rc %s): %s\n' "$2" "$3" "$rc" "$out" | redact_url >&2
        rm -rf "$4"
        return 1
    fi
}
# tenant_stage SRC STAGE — the tenant directory SRC flattened into STAGE
#   as ONE ConfigMap's keys: every file directly under SRC/seeds, and
#   SRC/tenant.toml as `tenant.toml` over any seeds/ copy (the root is
#   the canonical spelling; docs/tenant-contract.md). Flat because a
#   ConfigMap is flat and a mount inside a read-only mount cannot be
#   created — so the pod mounts this at /opt/boss/tenant/seeds and reads
#   BOSS_TENANT_DIR=/opt/boss/tenant, the seeds/tenant.toml spelling.
#   Prose and subdirectories (a README, an engine's data/) are not
#   seeds and are not staged. Refuses (rc 2, nothing staged) a
#   directory with no manifest, and one a ConfigMap cannot hold (the
#   API's 1 MiB object cap; the brewery's 500 KB seeds would fit, its
#   data/ would not — which is why it stays image-sourced).
tenant_stage() {
    local src="$1" stage="$2" f size
    if ! [ -f "$src/tenant.toml" ] && ! [ -f "$src/seeds/tenant.toml" ]; then
        echo "tenant_stage: $src holds no tenant.toml or seeds/tenant.toml — not a tenant directory" >&2
        return 2
    fi
    mkdir -p "$stage"
    if [ -d "$src/seeds" ]; then
        for f in "$src"/seeds/*; do
            [ -f "$f" ] && cp "$f" "$stage/"
        done
    fi
    if [ -f "$src/tenant.toml" ]; then
        cp "$src/tenant.toml" "$stage/tenant.toml"
    fi
    size=$(du -sb "$stage" | cut -f1)
    if [ "$size" -gt 1000000 ]; then
        echo "tenant_stage: $src stages $size bytes — over the 1 MiB a ConfigMap holds; a tenant this size is image-sourced (tenant_dir), not delivered" >&2
        rm -rf "$stage"
        return 2
    fi
    return 0
}
# site_stage SRC STAGE — the tenant checkout's site/ directory flattened
#   into STAGE as ONE ConfigMap's keys (design b64c4377; backlog
#   c8f6b233): every file directly under SRC/site — the company website
#   the gateway serves under the instance's `site` hostname
#   (boss-gateway site.rs), delivered as the `boss-site` ConfigMap
#   beside boss-tenant, with the same shape and the same 1 MiB bound.
#   Prints the number of files staged. A checkout with NO site/ stages
#   nothing and prints 0 with rc 0 — an empty STAGE, from which the
#   runner applies an EMPTY ConfigMap: the tenant has no site yet, the
#   instance still converges, and the site hostname answers 404 until
#   the directory lands (the packet's `site_source` line says so). Not
#   a refusal, because a declared site whose content is one tenant
#   commit away must not hold the instance's converge. Over the bound
#   is rc 2, nothing staged, like tenant_stage.
site_stage() {
    local src="$1" stage="$2" f n=0 size
    rm -rf "$stage"
    mkdir -p "$stage"
    if [ -d "$src/site" ]; then
        for f in "$src"/site/*; do
            [ -f "$f" ] || continue
            cp "$f" "$stage/"
            n=$((n + 1))
        done
    fi
    size=$(du -sb "$stage" | cut -f1)
    if [ "$size" -gt 1000000 ]; then
        echo "site_stage: $src/site stages $size bytes — over the 1 MiB a ConfigMap holds; the site stays small by construction until it has a build of its own" >&2
        rm -rf "$stage"
        return 2
    fi
    printf '%s\n' "$n"
}

# instances_skipped_by_gate K KM SOURCE_NS INSTANCES MOUNT
#   The packet's `instances_skipped` string as the secret gate would
#   decide it NOW: instance_secret_gate asked for every instance but
#   the source, WRITING NOTHING — the reads the apply loop makes,
#   without the apply. INSTANCES is render-instance.sh --instances
#   (name<TAB>namespace<TAB>…); MOUNT is the rendered directory as KM
#   sees it. Prints the string (empty when nothing is skipped) and
#   returns 0; returns 2 when a read could not be made — the string is
#   then not knowable, and the caller must not guess one.
#   A repo-sourced instance (seventh column) with NO boss-tenant
#   ConfigMap in its namespace is skipped too (backlog f4f5c387): the
#   deploying tick delivers the ConfigMap before it applies, so its
#   absence means the instance was never applied — skipped over an
#   unreadable tenant, or not yet converged — and its hostname must
#   stay served by the source (the 40d46042 shape). The tick does not
#   re-measure readability: that is the deploying tick's finding, and
#   the cluster's state is what this read-only tick may ask.
# THE COLUMNS OF `render-instance.sh --instances` ARE TAB-SEPARATED AND
# MAY BE EMPTY. `IFS=$'\t' read` cannot read them: a tab is whitespace
# to `read`, and a run of whitespace delimiters collapses into one, so
# an empty column DISAPPEARS and every column after it shifts left.
# Measured 2026-09-16 23:34Z on the first converge after the prod flip
# (c7be253): prod's empty `shares_with` column shifted `tenant_repo`
# into `tenant_ref`, the runner asked the forge for repository `main`
# and answered `tenant_source: boss: unreadable (main@)` — nothing
# rolled, prod stayed on its old database while the Secret already
# named the new one. Every reader of the list goes through this: the
# tabs become a non-whitespace separator (ASCII 31, unit separator),
# which `read` keeps even when empty.
instance_rows() { # INSTANCES-TEXT — one row per line, columns joined by \037
    printf '%s\n' "$1" | tr '\t' '\037'
}
IFS_ROW=$'\037'

instances_skipped_by_gate() {
    local k="$1" km="$2" source_ns="$3" instances="$4" mount="$5"
    local iname ins_ns _t _s _h _share trepo tref absent err rc skipped=""
    while IFS="$IFS_ROW" read -r iname ins_ns _t _s _h _share trepo tref _site; do
        [ -n "$ins_ns" ] || continue
        [ "$ins_ns" = "$source_ns" ] && continue
        if [ -n "$trepo" ]; then
            rc=0
            err=$($k get configmap boss-tenant -n "$ins_ns" 2>&1 >/dev/null) || rc=$?
            if [ "$rc" -ne 0 ]; then
                # NotFound is the answer; anything else (Forbidden, a dark
                # API) is a read that was not made — CANNOT TELL, as the
                # secret gate treats it.
                case "$err" in
                    *NotFound*|*"not found"*) ;;
                    *) echo "instances_skipped_by_gate: cannot read ConfigMap boss-tenant in $ins_ns: $err" >&2; return 2 ;;
                esac
                skipped="${skipped:+$skipped; }$(skipped_tenant_entry "$ins_ns" "not delivered" "$trepo" "$tref")"
                continue
            fi
        fi
        rc=0
        absent=$(instance_secret_gate "$k" "$km" "$ins_ns" "$mount/$ins_ns") || rc=$?
        case "$rc" in
            0) ;;
            1) skipped="${skipped:+$skipped; }$(skipped_entry "$ins_ns" "$absent")" ;;
            *) return 2 ;;
        esac
    done <<< "$(instance_rows "$instances")"
    printf '%s\n' "$skipped"
}

# EVERY TICK OBSERVES THE CONNECTOR, and both ticks observe it HERE
# (backlog 0b7804f3; measured 2026-09-16 13:38Z). The retire verb for
# boss-gcp's hand-written connector (infra/gcp/retire-cloudflared.sh)
# proves the hand-over from the newest converge whose packet carries
# `cloudflared` + `tunnel_ingress`, and refuses one older than 120 min.
# Those fields were written only by a DEPLOYING converge; the no-op
# tick recorded `unchanged` and nothing about the connector, so on a
# quiet morning David's `--for-real` was refused — 'converge 2afe48e8
# completed 10:23:56 — 194 min ago, older than the 120 min ceiling'
# (ops-request 7d05cb04) — with no way to refresh the evidence except
# landing a car. Evidence that exists only when something ships is the
# wrong shape for a liveness fact. So the observation is ONE function,
# called by the deploying tick after its roll and by the unchanged
# tick before its exit 0, and `observed_on` says which.
#
# observe_connector K KM NS MANIFEST DEPLOY SKIPPED OBSERVED_ON
#   Records on the packet, through run_summary_field, and prints the
#   journal lines:
#     tunnel_ingress   render-tunnel-config.sh --summary for SKIPPED —
#                      the packet's own `instances_skipped` string, so
#                      the map names what the connector is serving (a
#                      skipped instance's hostname from the source)
#     cloudflared      connector_status for DEPLOY in NS, its Secret
#                      derived from the rendered MANIFEST (as KM sees it)
#     observed_on      `deploy` or `unchanged` — which tick looked
#   Returns 0 in every case, as connector_status does: the tunnel is
#   not what a converge delivers. The renderer is read from beside this
#   file — the checked-out tree this lib was sourced from — the idiom
#   instances-skipped.lib.sh uses for its own parser.
observe_connector() {
    local k="$1" km="$2" ns="$3" manifest="$4" deploy="$5" skipped="$6" observed_on="$7"
    local ingress connector
    ingress=$(BOSS_INSTANCES_SKIPPED="$skipped" "$(dirname "${BASH_SOURCE[0]}")/../cluster/render-tunnel-config.sh" --summary)
    connector=$(connector_status "$k" "$km" "$ns" "$manifest" "$deploy")
    run_summary_field tunnel_ingress "$ingress"
    run_summary_field cloudflared "$connector"
    run_summary_field observed_on "$observed_on"
    echo "cluster-deploy-runner: tunnel ingress: $ingress"
    echo "cluster-deploy-runner: cloudflared: $connector (observed on $observed_on)"
}

# A DECLARED BROKER SECRET IS CREATED EMPTY WHEN IT IS ABSENT (backlog
# 51c98681; David 2026-09-16: no hand work unless absolutely required,
# and an empty object declared in the registry is not a credential).
#
# The credential broker's install phase PATCHes a value into its Secret
# and is deliberately NOT granted `create` — it cannot be name-scoped in
# RBAC, so the grant would be namespace-wide (boss-credential-broker.yaml).
# That left one hand act at the head of every machine rotation: "the
# Secret is pre-created empty, out-of-band, once". For the forge token
# it was done by hand; for the tunnel credential nobody had, so the
# connector waited in ContainerCreating and every converge recorded
# `cloudflared: skipped (secret absent: cloudflare-tunnel-credentials)`
# with nothing that would ever change it. The converge holds the admin
# credential, so the empty object is its to create — and ONLY the empty
# object: a value is the broker's, and a Secret that exists is never
# touched, whatever it holds.
#
# WHICH SECRETS is the broker's own declaration, not a list here and not
# a parse of the registry row's prose: every `credential.rotate.*` rule
# under infra/dispatcher/rules/ carries `secret_namespace` / `secret_name`
# as handler args, and those args are exactly what the handler writes
# (credential_issuer.rs `write_key`). Reading them means the converge
# creates the Secret the broker will fill, by construction; a second
# copy on the registry row would be the pair §9a says drifts. The tree
# is the converging checkout, so no read of the system of record is
# needed to know what to create (an arm that needs the patient is not
# an arm).
#
# broker_secrets RULES_DIR
#   ns<TAB>name for every Secret a broker rule declares, sorted, unique.
#   Read with grep/sed, not a TOML parser (roles.toml's rule): the
#   `handler = "…"` line names the handler, the `args = { … }` line that
#   follows it carries the Secret as two string LITERALS — the expr
#   spelling is `"\"boss\""`. A broker rule whose Secret is not a literal
#   (a metadata expression, an absent arg) is a NAMED refusal on stderr
#   and return 1: the converge cannot know what to create and must not
#   guess; the readable declarations are still printed.
broker_secrets() {
    local dir="$1" f line handler="" ns name found="" bad=0
    for f in "$dir"/*.toml; do
        [ -f "$f" ] || continue
        while IFS= read -r line; do
            case "$line" in
                'handler = "'*)
                    handler=${line#handler = \"}
                    handler=${handler%%\"*} ;;
                'args = '*)
                    case "$handler" in
                        credential.rotate.*) ;;
                        *) continue ;;
                    esac
                    ns=$(sed -nE 's/.*secret_namespace = "\\"([^"\\]+)\\"".*/\1/p' <<< "$line")
                    name=$(sed -nE 's/.*secret_name = "\\"([^"\\]+)\\"".*/\1/p' <<< "$line")
                    if [ -z "$ns" ] || [ -z "$name" ]; then
                        echo "cluster-deploy-runner: $f: rule handler $handler declares no literal secret_namespace / secret_name — cannot know which Secret to create" >&2
                        bad=1
                    else
                        found="$found$ns"$'\t'"$name"$'\n'
                    fi
                    handler="" ;;
            esac
        done < "$f"
    done
    [ -z "$found" ] || printf '%s' "$found" | LC_ALL=C sort -u
    return $bad
}

# ensure_declared_secrets K RULES_DIR
#   For each Secret broker_secrets declares: absent → create it EMPTY
#   (`kubectl -n NS create secret generic NAME`, no --from-* of any kind,
#   so the object carries no data until the broker's install phase
#   fills it); present → nothing, whatever it holds; a read the
#   credential cannot make, or a create that fails → named with kubectl's
#   reason, and NOT created — a Forbidden read is a refusal, not an
#   absence. Prints exactly one line for the converge packet:
#     created NS/NAME, … | present NS/NAME, … | cannot NS/NAME (REASON), …
#   (each part only when non-empty; `none declared` when no broker rule
#   exists; `unreadable declaration` appended when broker_secrets refused
#   one — the details are on stderr). Returns 0 in every case: the line
#   is the verdict, the car's probe reads it, and a Secret the broker
#   fills is not what a train delivers, so it must not hold the converge.
ensure_declared_secrets() {
    local k="$1" dir="$2" declared ns name answer out created="" present="" cannot="" unreadable=0
    declared=$(broker_secrets "$dir") || unreadable=1
    while IFS=$'\t' read -r ns name; do
        [ -n "$name" ] || continue
        answer=$(secret_presence "$k" "$ns" "$name")
        case "$answer" in
            present) present="${present:+$present, }$ns/$name" ;;
            absent)
                if out=$($k -n "$ns" create secret generic "$name" 2>&1); then
                    created="${created:+$created, }$ns/$name"
                    echo "cluster-deploy-runner: created Secret $ns/$name, empty — the broker's install phase fills it" >&2
                else
                    cannot="${cannot:+$cannot, }$ns/$name ($(head -n 1 <<< "$out"))"
                    echo "cluster-deploy-runner: could not create Secret $ns/$name — $out" >&2
                fi ;;
            *)  cannot="${cannot:+$cannot, }$ns/$name (${answer#cannot: })"
                echo "cluster-deploy-runner: cannot read Secret $ns/$name — ${answer#cannot: }; not creating it" >&2 ;;
        esac
    done <<< "$declared"
    local line=""
    [ -z "$created" ] || line="created $created"
    [ -z "$present" ] || line="${line:+$line | }present $present"
    [ -z "$cannot" ] || line="${line:+$line | }cannot $cannot"
    [ "$unreadable" = 0 ] || line="${line:+$line | }unreadable declaration (see the journal)"
    echo "${line:-none declared}"
    return 0
}

# converge_held HOLD_FILE — an operator's hold stands: print its reason
# and return 0; no hold, return 1. The runner asks before it builds.
converge_held() {
    local f="$1"
    [ -f "$f" ] || return 1
    cat "$f"
}

# THE RUN ANSWERS THE PACKET THAT ASKED FOR IT (backlog d66f92b2).
#
# The `converge` ops verb (converge-now.sh) starts this unit with
# `systemctl start --no-block`, which returns 0 the moment systemd
# accepts the job — so the ops-request closed `answered` on 2026-09-07
# 22:01 while the run it started died a second later on a git lock.
# The verb now leaves the requesting packet's id in an inbox in the
# checkout; the runner takes the inbox when it starts and, when it
# ends, PATCHes each request with the outcome. Best-effort, never
# fatal: visibility must not block the executor (the-executor-never-
# waits-on-its-visibility), and the maintenance packet's ExecStopPost
# verdict is the record either way.

# take_converge_requests REPO — move every id in the inbox into this
# run's file. Idempotent (the stage-2 re-exec runs it again and finds
# the inbox already taken).
take_converge_requests() {
    local inbox="$1/.git/boss-converge-requests" mine="$1/.git/boss-converge-requests.run"
    [ -f "$inbox" ] || return 0
    cat "$inbox" >> "$mine" && rm -f "$inbox"
}

# answer_converge_requests REPO KEY VALUE — PATCH {KEY: VALUE} onto the
# metadata of every ops-request this run was started for, then forget
# them. Only well-formed job ids are sent (a malformed line is logged
# and dropped); a missing BOSS_JOBS_URL or an unreachable API is one
# loud line, and the run's exit status is untouched.
answer_converge_requests() {
    local repo="$1" key="$2" value="$3" mine="$1/.git/boss-converge-requests.run" id body code
    [ -f "$mine" ] || return 0
    if [ -z "${BOSS_JOBS_URL:-}" ]; then
        echo "cluster-deploy-runner: BOSS_JOBS_URL unset — cannot annotate the request(s) that started this run ($key: $value)" >&2
        rm -f "$mine"; return 0
    fi
    value=$(printf '%s' "$value" | tr -d '"\\' | tr '\n' ' ')
    body=$(printf '{"%s":"%s"}' "$key" "$value")
    while IFS= read -r id; do
        case "$id" in
            "") continue ;;
            *[!0-9a-fA-F-]*)
                echo "cluster-deploy-runner: ignoring a malformed request id in $mine" >&2; continue ;;
        esac
        code=$(printf '%s' "$body" | curl -s -o /dev/null -w '%{http_code}' --max-time 15 \
            -X PATCH -H 'content-type: application/json' \
            -H 'x-boss-user: {"id":"automation:cluster-deploy-runner","role":"platform-admin","access_tier":"operator"}' \
            --data-binary @- "$BOSS_JOBS_URL/api/jobs/$id/metadata") || code="unreachable"
        case "$code" in
            2*) echo "cluster-deploy-runner: request ${id:0:8} annotated $key: $value" ;;
            *)  echo "cluster-deploy-runner: could not annotate request ${id:0:8} ($key: $value) — API said $code; its maintenance packet still carries the verdict" >&2 ;;
        esac
    done < "$mine"
    rm -f "$mine"
}
