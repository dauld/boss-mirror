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
    local required name answer absent="" keys
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
    [ -n "$absent" ] || return 0
    absent="${absent# }"
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

# instances_skipped_by_gate K KM SOURCE_NS INSTANCES MOUNT
#   The packet's `instances_skipped` string as the secret gate would
#   decide it NOW: instance_secret_gate asked for every instance but
#   the source, WRITING NOTHING — the reads the apply loop makes,
#   without the apply. INSTANCES is render-instance.sh --instances
#   (name<TAB>namespace<TAB>…); MOUNT is the rendered directory as KM
#   sees it. Prints the string (empty when nothing is skipped) and
#   returns 0; returns 2 when a read could not be made — the string is
#   then not knowable, and the caller must not guess one.
instances_skipped_by_gate() {
    local k="$1" km="$2" source_ns="$3" instances="$4" mount="$5"
    local iname ins_ns _t _s _h absent rc skipped=""
    while IFS=$'\t' read -r iname ins_ns _t _s _h; do
        [ -n "$ins_ns" ] || continue
        [ "$ins_ns" = "$source_ns" ] && continue
        rc=0
        absent=$(instance_secret_gate "$k" "$km" "$ins_ns" "$mount/$ins_ns") || rc=$?
        case "$rc" in
            0) ;;
            1) skipped="${skipped:+$skipped; }$(skipped_entry "$ins_ns" "$absent")" ;;
            *) return 2 ;;
        esac
    done <<< "$instances"
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
