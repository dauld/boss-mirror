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

`argv[0]` names a script RELATIVE to the repo (`infra/forge/reach.sh`);
the runner resolves it against its own checkout, so every managed host
can carry every script, and a bare command (`systemctl`, `df`) stays a
bare command resolved on PATH. An absolute path is refused by the lint
(66077f9c: eleven verbs once baked the forge checkout's path in and
could run nowhere else).

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
read-only host-agnostic reads and nothing mutating.

## Defense

Stated once and relied on by `ops-runner.sh`: the runner never executes
a packet-supplied string. It builds an argv ARRAY from the verb file —
no `sh -c`, no `eval`, no interpolation into program text. The patterns
admit no whitespace and no leading `-`, so a validated arg can neither
split into extra words nor be parsed as an option; `unit-status` also
passes `--` so even a future pattern loosening cannot turn an arg into a
flag there.

JSON rather than TOML because the runner is sh + jq (directive 26d61c97:
no python) and jq reads JSON natively — a hand-rolled TOML parser in sh
is exactly the fragile string handling this protocol exists to ban.
