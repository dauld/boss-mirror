# BOSS ops-request verb allowlist

THE authority for what a host runner will execute. In-tree, reviewed,
versioned: changing what a host runs for a packet is a PR, never a
packet.

## One file per verb

**This directory is the allowlist.** A verb is `<name>.json` here, and
the verb's name — the `metadata.verb` a packet carries — IS the file
name. Adding a verb is dropping a file in; it touches no shared line.

It used to be one file, `infra/ops/verbs.json`, one JSON object — and a
JSON object has no uncontended insertion point: appending contends on
the previous entry's trailing comma and the closing brace, inserting
alphabetically contends with a neighbour. On 2026-09-12 three cars each
added a verb, two inserted before the same key, and the conductor left
one behind (`conflict: infra/ops/verbs.json`) — the shape CLAUDE.md §9a
records for `rules.toml` before dispatcher rules became one file each
under `infra/dispatcher/rules/`. Backlog 5086842d collapsed this one the
same way.

Every reader DERIVES the allowlist from this directory; none holds a
list of verbs:

- `infra/ops/verbs-allowlist.sh` assembles `{"verbs": {<name>: <file>}}`
  from `*.json` here — the one derivation that `infra/ops/ops-runner.sh`
  (the runner on the forge and boss-gcp) and the two allowlist lints
  (`infra/lint/a-verb-declares-the-hosts-it-serves.sh`,
  `infra/lint/the-controls-are-bounded-verbs.sh`) all call. It refuses
  (exit 78, naming the fault) on a missing or empty directory, a file
  that is not one verb object, or a name the runner could not match.
- `boss ops` (`crates/orchestrators/boss-cli`) compiles the verbs in, so
  its `build.rs` lists this directory at build time and generates the
  `include_str!` table; a test pins that set to the directory.
- `infra/oss-quickstart/Dockerfile` COPYs the directory into the image's
  build stage, because boss-cli reads it while compiling there.

A `README.md` beside the verbs is prose, not a verb — only `*.json` is
read.

## Authorization

Phase 1 was READ-ONLY. `reclaim-disk` is the FIRST MUTATING verb —
authorized by David, 2026-09-03, after the forge disk filled and blocked
CI for every train. Its bound is by construction of the ONE script it
calls (`infra/forge/disk-floor-sweep.sh`, the same definition the hourly
timer runs — §9a): regenerable docker caches only, in a fixed order,
stopping at the floor, never volumes or non-docker paths, loud non-zero
exit when the floor stays unmet rather than deleting harder. Any further
mutating verb needs the same explicit authorization — that entry is a
precedent for the PROCESS, not a loosened default. A mutating verb says
`MUTATING` in its `about` and names who authorized it; the lint
`the-controls-are-bounded-verbs.sh` derives its roster from that word.

## Shape of a verb file

```json
{
  "about": "what it does, MUTATING if it is, who authorized it",
  "hosts": ["forge"],
  "argv": ["infra/forge/some-script.sh", "{1}"],
  "params": [{"name": "sha", "pattern": "^[0-9a-f]{7,40}$"}],
  "timeout": 120
}
```

`argv` is the exact command, literal words plus `{N}` placeholders;
`{N}` takes param N (1-based) AFTER pattern validation. A param without
`default` is required; `max` is a numeric ceiling applied after the
pattern has proven the value is digits. A param may instead carry
`one_of`: an exact list of reviewed literal WORDS the packet selects
among (no pattern — equality only). Because the word comes from this
file and never from the packet, a literal may lead with `-`
(`publish-github-pr`'s `--check`). With `optional: true` an absent arg
drops its `{N}` word from the argv instead of failing. `timeout`
(seconds) overrides the runner's default for that verb.

`argv[0]` names a script RELATIVE to the repo (`infra/forge/disk-report.sh`);
the runner resolves it against its own checkout, so every managed host
can carry every script, and a bare command (`systemctl`, `df`) stays a
bare command resolved on PATH. An absolute path is refused by the lint
(66077f9c: eleven verbs once baked the forge checkout's path in and
could run nowhere else).

**The tree's own CLI is a bare command too.** Since backlog 9f00a805
(consolidation H8) every managed host installs `/usr/local/bin/boss`
from the converged image (`infra/estate/install-cli-from-image.sh`),
so a verb whose behaviour already exists as a `boss` verb names it
directly — `run-car-probe` is `["boss", "prove", "{1}", "--from-car",
"--unattended"]` — and the shell twin that re-implemented it on the
host is deleted with its pin. The runner hands such a verb its own
account as `BOSS_ACTOR` (the CLI refuses an unnamed write) and the
system of record from its unit's `EnvironmentFile`; the verb's exit
code is the packet's `exit_code`, so a CLI verb run this way must make
its exit the verdict. The remaining twins retire the same way, one
verb per car, each measured first (which of the script's behaviours
the CLI verb lacks — the argument shape, the refusals, the output a
reader of the packet expects, the host-side actions a CLI verb cannot
do from the system of record alone).

`hosts` is WHICH HOSTS THE VERB SERVES — the estate node ids (`nodes.id`
in `infra/postgres/schema`, the same string a packet's `metadata.host`
carries) whose runner will execute it. REQUIRED, and ABSENT MEANS
REFUSE: the runner refuses a verb whose `hosts` does not list its own
`HOST_ID`, so a new verb cannot reach a host by forgetting to say. It is
not a privilege boundary — every runner reads this same directory — it
is the door telling the truth about what it opens. Before 2026-09-11 it
could not: boss-gcp got its runner that day and 11 of the 16 verbs named
a script under the FORGE's checkout (`infra/forge`), which does not
exist there, so standing the runner up advertised a vocabulary of which
11 could only fail on ENOENT. An exec failure is not a verdict
(CLAUDE.md §Diagnosis); a refusal that names the verb, this host and the
hosts that verb does serve is. Widening a verb to another host is a
reviewed change to that verb's file — boss-gcp's set is deliberately the
read-only host-agnostic reads, plus exactly the MUTATING verbs
`infra/lint/a-verb-declares-the-hosts-it-serves.sh` admits BY NAME with
their authorization. The first is `retire-second-stack` (David
2026-09-11, design 9e3e093f): bounded to the unit list the tree carries
at `infra/gcp/second-stack-units.txt`, capture-before-stop, `--dry-run`
exercisable without acting, `--for-real` a human's decision to file.
The second, `uninstall-not-in-role` (same authorization, car 4 of
d5941ef3), carries no list at all: its set is what
`install-units.sh roster` says the host's LIVE roles do not name —
the installer's own derivation — and an empty set is a refusal. The
third, `retire-cloudflared` (David 2026-09-16, design 4c565f8c; backlog
0b7804f3 car 4), retires the host's hand-written tunnel connector —
the unit whose inline token unit-cat once leaked (9c760dd7) — and is
bounded to that ONE unit, named in the script and never a param: it
verifies the hand-over through the system of record BEFORE anything
stops (the newest converge that observed the in-cluster connector must
be under two hours old, `connected`, and routing every hostname
`infra/cluster/instances.toml` declares), prints the unit through
unit-cat's mask, and refuses success while systemd still holds the
unit. It never touches the tunnel in Cloudflare: that is the broker's
revoke phase, which completes on its own once the old tunnel shows
zero connections. The fifth, `publish-drift` (retro 27fad542 approved by
David 2026-09-18; backlog a2f97942), publishes every platform workflow
kind the tree moved ahead of as ONE act — 23 `publish-workflow`
requests were one hand loop on 2026-09-18 — and is bounded by
composition: the drift set is `publish-workflow.sh <kind> --check` per
kind, the publish is that verb per tree-ahead kind, a live row the tree
never said is listed field by field and never published (there is no
`--force-tree`; that stays the Drift tab's approve, one kind at a
time). Its `mode` DEFAULTS to `--check`, which is what lets a
dispatcher rule file it on boss-gcp's checkout moving without any
authority widening: a `jobs.spawn` packet carries no args, so it can
only ask; `--for-real` is a word a packet carries on purpose.

## Approval verbs

A verb that declares `requires_approval` runs only under a passkey
approval of a rendered plan (design 17835005; the runner half is backlog
fd7090cc). Its file carries three more keys, each checked by the runner
and by `boss ops` before anything is filed or rendered:

- `plan_verb` — a read-only verb here, serving the same hosts, taking
  exactly this verb's params less the last, which prints the plan on
  stdout and `plan-sha256:` on stderr;
- a last param named `plan_sha256`, required, `^[0-9a-f]{64}$` — the
  runner appends sha256 of the SIGNED plan, and the script re-renders
  and refuses bytes that no longer hash to it;
- `approvers` — employee ids, e.g. `["emp-david"]`: only a presence
  stamp whose `authority_id` is on this list approves (design 03451237
  q2, David 2026-09-22: a named list, never a role, because a role is
  registry data and a role gate hangs the approval on whoever can write
  a policy row). Adding an approver is a reviewed change to this file.

A completed approve step is not by itself an approval: Reject runs the
same passkey ceremony and completes it too. The runner runs the write
only when the step's `decision` — saved before the stamp, so inside the
signed shape — is exactly `approved`; ops-request routes any other
decision to `refused` (adversarial re-review of fd7090cc, 2026-09-25).

## Defense

Stated once and relied on by `ops-runner.sh`: the runner never executes
a packet-supplied string. It builds an argv ARRAY from the verb file —
no `sh -c`, no `eval`, no interpolation into program text. The patterns
admit no whitespace and no leading `-`, so a validated arg can neither
split into extra words nor be parsed as an option; `unit-status` also
passes `--` so even a future pattern loosening cannot turn an arg into a
flag there. An arg carrying any control character is refused before its
pattern is consulted: jq's `$` also matches before a trailing newline,
so `"word\n"` passed `^[a-z]+$` and grew the argv an empty word
(security review of fd7090cc, 2026-09-24).

JSON rather than TOML because the runner is sh + jq (directive 26d61c97:
no python) and jq reads JSON natively — a hand-rolled TOML parser in sh
is exactly the fragile string handling this protocol exists to ban.
