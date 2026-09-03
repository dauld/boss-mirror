# Runbook: adding a worker node

**What this is:** the mechanical path from a new machine on the LAN
to gates and the dev pod running off the control plane. Written
2026-08-23, ahead of the node David ordered (decision recorded on
design packet `95e9492a`: one small dedicated worker, hookup
2026-08-25). Why it matters is in
[build-capacity](../design/build-capacity.md): the cluster has no
worker nodes, so every heavy build has shared a machine with etcd and
the system of record's Longhorn replicas — the direct cause of the
2026-08-22 control-plane incidents.

## Before the node arrives

Nothing here is blocked on hardware:

- `~/talos-homelab/v2/base-worker.yaml` already exists — the worker
  machine-config template, out-of-tree with the rest of the Talos
  material (`infra/cluster/manifests/README.md` explains why).
- Client `talosctl` is v1.13.8; keep it within one minor of the
  cluster.
- Decide the hostname now. This runbook assumes **`w-1`**, and the
  manifest edits below name it.

## Joining the node

1. Boot the machine on the Talos image matching the cluster's
   version, on the LAN, with a static or reserved lease.
2. Apply the worker config — the template carries the cluster CA and
   join token, so this is the whole handshake:

   ```
   talosctl --talosconfig ~/talos-homelab/v2/talosconfig \
     apply-config --insecure -n <new-node-ip> \
     -f ~/talos-homelab/v2/base-worker.yaml
   ```

   (`--insecure` is correct exactly once: the node has no PKI yet.)
3. Watch it register against the VIP (<api-vip>) and go Ready:

   ```
   kubectl --kubeconfig ~/talos-homelab/v2/kubeconfig get nodes -w
   ```
4. The role label is declared in the machine config, not applied by
   hand. The patch carries:

   ```
   machine:
     nodeLabels:
       boss.dev/purpose: build
   ```

   so the label survives a wipe-and-reinstall and arrives with the
   node rather than after it. `kubectl label node ...` would work and
   would be gone the next time the machine is rebuilt.
## Two things that make a node look joined when it is not

Both of these produce a node that reports `Ready`, passes every
Kubernetes health check, and is broken. Neither is visible to
`kubectl`. Check both before you move work onto it.

### The registry mirror

The forge registry speaks plain **HTTP**. A node without a mirror
declaration tries HTTPS, gets a plaintext response, and fails every
internal image pull with `server gave HTTP response to HTTPS client`.
The node still joins and still says `Ready`. w-1 sat like that for two
hours on 2026-08-25 while its `ImagePullBackOff` pods were read as
unrelated noise.

The declaration belongs in the node patch so one apply is a complete
node:

```yaml
machine:
  registries:
    mirrors:
      10.20.0.15:3000:
        endpoints:
          - http://10.20.0.15:3000
```

### The GPU driver, if the machine has one

**Installing the NVIDIA extensions is not enough. They ship the driver;
nothing loads it.** The machine config must declare the kernel modules:

```yaml
machine:
  kernel:
    modules:
      - name: nvidia
      - name: nvidia_uvm
      - name: nvidia_drm
      - name: nvidia_modeset
```

Without them `ext-nvidia-persistenced` waits forever on
`/sys/bus/pci/drivers/nvidia`, `ext-nvidia-cdi-gen` waits on that, and
**the machine never finishes booting** — `MachineStatus` stays
`STAGE=booting READY=false` for the life of the node.

A node that never completes boot **is reset on a timer**. w-1 reset
three times at 69-70 minutes of uptime, once while completely idle
(load 0.36, CPU 40C). That cost most of a day chasing a thermal fault
that did not exist, because the resets happened to land inside 40-minute
gates and looked like load. The tell was in the logs the whole time: the
uptime at each reset was identical.

Applying the modules needs a reboot. Afterwards the services read
`Finished` and `Running`, and the stage reads `running`.

Making the GPU **schedulable** is a further step and not covered here:
Kubernetes also needs the NVIDIA device plugin DaemonSet and a
`RuntimeClass`. Neither existed in this cluster as of 2026-08-26, and
nothing in BOSS uses a GPU yet.

## Moving the work onto it

Two manifest edits, each a `nodeSelector` that currently pins to a
control-plane node:

| file | today | after |
|---|---|---|
| `infra/gate-runner/gate-runner.yaml` | `kubernetes.io/hostname: cp-3` | `boss.dev/purpose: build` |
| `infra/cluster/manifests/boss-dev.yaml` | `kubernetes.io/hostname: cp-2` | `boss.dev/purpose: build` |

Both were made on 2026-08-25 when w-1 joined; the table stays because
the next worker inherits the role by wearing the label, and these two
files should not need editing again.

Ship them as a car (the deploy runner converges manifests; do not
hand-apply). Two consequences worth knowing before you do:

- **The dev pod's workspace PVC is `longhorn-dev-disposable` with
  `dataLocality: best-effort`, single replica.** Moving the pod to a
  new node means Longhorn rebuilds that replica there — the clone and
  cargo cache are reproducible, but the first gate after the move is
  cold. Expect one slow run, not a broken one.
- **`strategy: Recreate` plus a ReadWriteOnce volume** means the old
  pod must fully terminate before the new one attaches. If it hangs,
  the old node is still holding the volume; check
  `kubectl get volumeattachment`.

## Proving it worked

The point of the node is that heavy work stops touching the control
plane. Verify exactly that, not just that pods moved:

0. **Check the machine actually finished booting**, before anything
   else and again an hour later:

   ```
   talosctl -n <node> get machinestatus     # must read STAGE=running
   talosctl -n <node> services              # nothing left in Waiting
   talosctl -n <node> read /proc/uptime     # must exceed ~70 minutes
   ```

   `Ready` in `kubectl` does not imply this. Compare against a node
   known good: on 2026-08-26, w-2 read `running` with 401 minutes while
   w-1 read `booting` and had never survived 70.
1. Start a full gate on the new node (`infra/gate-runner/`, with its
   `gate-run` packet registered first).
2. While it runs, poll `kubectl get --raw /readyz?verbose` on a
   loop. Before the node, this reported `etcd failed` under gate
   load (2026-08-22 18:10Z). It must stay `readyz check passed`
   throughout.
3. Confirm placement: `kubectl -n boss-dev get pods -o wide` shows
   the gate pod on `w-1`, and `kubectl describe node cp-2 | grep -A4
   'Allocated resources'` shows the build requests gone.

## What this does not do

It does not make the cluster highly available for builds — one worker
is one failure domain, and a gate that dies with the node still dies.
It leaves cp-1/2/3 carrying the control plane, etcd, and the SoR
replicas, which is what those machines are for. And it does not
change the gate-runner's disk discipline: one job, one branch, one
private per-run workspace (an emptyDir seeded from the shared warm
target on the seed PVC), because ~74 GB per cold build is a property
of the workspace, not of where it runs. Concurrent gates all co-mount
the seed PVC, so its RWO attach herds them onto the node that holds
it — moving the build label moves the whole pack once the volume
follows.
