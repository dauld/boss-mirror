# Platform document bundle — what an agent profile is told, as DATA

One file per profile: `<profile>-rules.md` holds the rules document a
run under that profile is briefed with, and its front-matter names the
profile. **Adding a profile's rules is dropping a file in.** Nothing is
appended anywhere; two cars adding profiles touch no shared line.

`boss brief <packet>` prints the document for the profile the packet's
step declares (its projected `agent_profile`, `builder` when it
declares none) after the packet and the invariants; `boss dispatch`
prints the same as the last part of the prompt it hands over. The
reader is `boss_cli::documents`, which reads THIS directory in the
checkout the verb is standing in — the same tree the invariants are
derived from, so a builder in a worktree reads the rules of the tree
it builds and an edit here reaches the next dispatch with no seed and
no deploy.

TWO PROFILES, TWO DOCUMENTS. `builder-rules.md` is about shipping a
car — the base, the gate, the push. `analyst-rules.md` (backlog
8d32cc88, 2026-09-19) is about the profile that ships none: eleven
steps in the Workflow bundle declared `analyst` and every dispatch
carried the one-line note naming the file that would have served it,
with the page march about to add two more at 47 packets of volume. An
analyst's output is metadata on a step and packets filed, so its rules
are about EVIDENCE and REFUSAL — measuring now rather than quoting the
brief, an empty read as a wrong read until a control says otherwise, a
gap list that matches the ids filed — not about cargo.

WHICH PROFILES NEED A DOCUMENT IS NOT A LIST HERE (CLAUDE.md 9a): the
tests in `boss_cli::documents` read the profiles off the platform
Workflow bundle's `agent` blocks and require a document for each, so a
step that starts declaring a third profile fails until one exists
rather than dispatching an agent with no rules. Each document is
pinned on its own content besides — the builder's on the gate's checks
and the doors a red gate taught it, the analyst's on its doors (each
checked against the CLI's own verb roster) and on NOT carrying the
builder's.

WHY A DIRECTORY AND NOT A REGISTRY ROW (design c87fb59b car 2, backlog
39d0b528, measured 2026-09-18). Three bundles under `infra/platform/`
seed a registry — workflows, stations, step-plugins — each through its
own insert-if-missing seed binary, and the fourth home a rules document
could take, the company manual in `boss-content`, is HR-authored tenant
content behind an HTTP surface a CLI verb with only `BOSS_JOBS_URL`
cannot reach. A registry row would buy live editing at the cost of a
seed, a door and a version column for one Markdown file that changes
when the pipeline changes — which is to say, with a car. The rules
were a scratchpad file until this directory existed
(`/tmp/.../builders/RULES.md`): unversioned, untested and one pod
restart from gone.
