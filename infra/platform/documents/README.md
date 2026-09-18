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
no deploy. Pinned by the test in that module: the builder document
exists, names the profile it is for, and names the gate's own checks.

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
