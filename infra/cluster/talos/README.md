# infra/cluster/talos — the machine-config entries the pipeline depends on

`patches/<node>.yaml` is the **declaration** of every non-secret Talos
machine-config entry a BOSS node carries that the pipeline relies on.
`check-declared.sh` reads a node's LIVE config against it. Nothing here
holds a credential, and nothing here runs `talosctl`.

## Why this directory exists

CLAUDE.md's rule: *an imperative cluster change has an expiry*, and
substrate the pipeline relies on is declared where the pipeline can
read it. Until 2026-09-15 (backlog `08430090`) the entries below lived
only in `~/talos-homelab/v2` on a laptop, applied by hand with
`talosctl patch machineconfig`:

- **w-1** — `machine.files` creating `/var/local/gate-seed` and
  `machine.kubelet.extraMounts` binding it into the kubelet's mount
  namespace. The gate's warm seed is a `local` PersistentVolume on that
  directory (`infra/cluster/manifests/gate-seed-local.yaml`); without
  the bind a pod mounting it sat nine hours at ContainerCreating
  (`d42d4967`). A rebuilt or re-imaged w-1 without these entries hangs
  every gate, and nothing would have said why.
- **cp-1, cp-2, cp-3** — `machine.kubelet.extraConfig` image-GC
  thresholds, one of the levers behind the 2026-09-11 outage of the
  system of record (design `16115a17`).
- **every node** — `machine.registries.mirrors` for `10.20.0.15:3000`,
  the forge's registry every image in the cluster is pulled through.

The values are each node's live config as David read it on 2026-09-15
(`talosctl -n <ip> get machineconfig -o yaml`), declared **as found,
per node**. Two things are worth knowing about what was found:

- w-1's gate-seed `files` entry is present **twice** live — two
  identical hand patches on 2026-09-12. It is declared once; the check
  reports the double as `DOUBLED` until it is removed live.
- The image-GC pair is **40/30 on cp-2 and 50/40 on cp-1 and cp-3**.
  Which pair is intended is an open question on `08430090`; the
  declaration records each node as it is, so the check reads `MATCH`
  today and the decision, when made, is one edit per node here.

PKI, tokens, the cluster secret, and anything else `talosctl gen
secrets` produces **never live here**. A patch is a fragment; the
secrets stay under `/etc/boss-ops` on the cluster-operator host
(design `1bc4b4ed`) and in the operator's own config until then.

## Reading a node against its declaration

The check takes the live config as **input** — a file, or stdin — so it
needs no talosconfig and runs anywhere python3 does (the pod, the gate
image, a Mac; no PyYAML needed):

```
talosctl -n 10.20.0.14 get machineconfig -o yaml | infra/cluster/talos/check-declared.sh w-1
infra/cluster/talos/check-declared.sh cp-2 cp-2.live.yaml
```

Node addresses come from the estate registry (`boss-api GET
/api/estate/nodes`, field `address`), not from a table here — the
registry is the one place that answers hardware questions. The check
itself needs no address: the live document names its own hostname, and
the check **refuses** to compare a document whose hostname is not the
node asked for (a wrong target answers instead of erroring).

One line per declared entry, in the vocabulary design `16115a17`
decided, then a summary:

| verdict | meaning | exit |
|---|---|---|
| `MATCH` | present live with the declared value | 0 |
| `DRIFT` | present live with a different value — both printed | 1 |
| `ABSENT` | declared, not live | 1 |
| `DOUBLED` | a declared `files`/`extraMounts` entry appears more than once live | 1 |
| `UNDECLARED` | live, in a class the check reads, named by no declaration | 0 (reported) |

Exit 2 is a usage refusal (no such node, unreadable input, wrong
hostname); 78 is no python3. The classes read for `UNDECLARED` are
`machine.files` under `/var/local`, every `extraMounts` entry, every
`imageGC*` key of `extraConfig`, and every registry mirror — so the
first run on a control plane will most likely list a mount the grep of
2026-09-15 showed but did not name. That is the check working: declare
it here, or leave it listed.

The check is a pure comparator. When the forge's `cluster-operator`
role carries a talosconfig, the `talos-get` ops verb (the next car on
`1bc4b4ed`) feeds it the same way the pipe above does; the comparison
does not change.

## Applying a declaration

A patch is a Talos strategic-merge fragment, exactly the shape
`talosctl patch machineconfig --patch @file` accepts:

```
talosctl -n <address> patch machineconfig --patch @infra/cluster/talos/patches/<node>.yaml
```

The kubelet restarts; running pods survive; **no reboot** (`files`,
`extraMounts`, `extraConfig` and `registries` all apply live). Then
read the node back through the check — verify is always the next
comparison, never the applier's exit code.

Two things the merge will not do for you:

- It **appends** list entries it cannot key — that is how w-1's seed
  file entry came to be doubled. Re-applying `w-1.yaml` does not repair
  a `DOUBLED` finding; remove the duplicate with `talosctl -n <address>
  edit machineconfig` and run the check again.
- It does not remove an entry you delete from a declaration. A removed
  entry reads as `UNDECLARED` live until it is removed live too.

`talosctl` is a human's door today (design `16115a17`: "a named human
step for Talos"). The bounded `node-converge <node>` verb — `apply-config
--dry-run` against the per-node file here, the diff on the packet, then
apply — is design `1bc4b4ed`'s third car and is not built yet.
