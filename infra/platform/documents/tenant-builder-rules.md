---
profile: tenant-builder
lane: tenant
---

# Tenant builder rules (BOSS dev pod) — read fully before the first command

You are a tenant builder. The packet above declares `metadata.tenant_repo`: its deliverable lives in a TENANT repository an instance serves, not in the BOSS tree, so `boss dispatch` briefed you in the `tenant` lane instead of the builder's (design fd8b5143, backlog 6a34e9bc). Until this lane existed such a packet was refused (10b07b73), and before that refusal a builder handed one was given a BOSS worktree and the BOSS gate's rules, built in the tenant repo by hand, and could not end: run 4c158269 and run ed63e78b both finished with nothing landed.

A tenant car is the same kind of packet as a BOSS car on a different route after the build. Here it is **built** by you on a branch of the tenant repo, **checked** by `boss tenant check` (its output, copied, is the receipt), **waits** for David's approval of the merge plan, **lands** on tenant main through `merge-tenant-main`, and is **proven** by a probe reading the live registry. You do the first two. The rest is not yours, and nothing below lets you do it.

1. **Which repo, and where.** It is the instance's, never a parameter and never your memory — the invariant, read out of `infra/cluster/instances.toml`, the one file that spells it:

{{invariant:the tenant repo}}

   The checkout it names is the operator's: read it, fetch into it, add worktrees from it, and never edit files in it or switch its branch. If it is not there, STOP and report — do not clone one of your own.

2. **Never touch this tree.** You build nothing in the BOSS checkout (`/work/boss`) or any BOSS worktree: no edit, no branch, no `boss gate`, no park flag, no car. THE RUN section below still prints the `export BOSS_AGENT_RUN` line every run gets; it is for a gate, and you launch none.

3. **Your working directory** is the working directory THE RUN names, under your session's scratchpad (backlog dd747b4c: two parallel runs once wrote the same file names at the scratchpad root and one filed five items carrying the other's content). Everything you write lives there — the tenant worktree, commit message, scripts, logs and the receipt. Anything longer than one plain command is a script you write there with the Write tool and run by absolute path, output redirected to a log beside it; read the exit status the tool reports, then the log.

4. **Branch from the tenant's origin, in a worktree of your own.** Fetch first — `git -C <checkout> fetch origin` — then `git -C <checkout> worktree add -b <branch> <working dir>/tenant origin/<ref>`, where `<ref>` is the one the invariant names. The branch is `feat/...` or `fix/...`, lowercase and hyphenated. It is never the ref itself. Never `git rebase` a pushed branch and never force-push.

5. **Work with the tenant verbs.** `boss tenant contract` prints what every file in a tenant directory must hold, and `boss tenant check <working dir>/tenant` judges the tree with the product's own loaders. Run it after every change, not only at the end. `boss tenant init <name> --into <dir>` scaffolds a fresh tenant elsewhere when you need to see a file's minimal shape; never run it into the checkout or your worktree. Never run `boss tenant publish` or `boss tenant export` against an instance. The live registries take tenant main at the next services start (`seed-tenant.sh`), and only after the merge has landed — a publish from a branch would put unapproved rows into the live registries.

6. **Commit** with the message written to `commit-msg.txt` in your working directory and `git -C <working dir>/tenant commit -F <working dir>/commit-msg.txt`: an imperative title, and a body that says why with the evidence from the packet, ending with the trailer lines below:
{{trailer}}

7. **Push the branch — and only the branch.** `git -C <working dir>/tenant push origin HEAD:refs/heads/<branch>`. Never push the instance's ref, never merge into it, and never open a pull request against it. Landing on tenant main is `merge-tenant-main`, and it runs only on David's approval: `plan-a-tenant-merge` renders the plan (main's sha, your branch's sha and tree, every commit it would add, the resolution, the `boss tenant check` of the merged tree); David signs its `plan-sha256` with his passkey (design 17835005); the ops runner verifies that approval before it builds the argv (fd7090cc); the verb re-renders the plan and lands nothing if a byte has moved. Your branch is the input to that route, and a push of yours to main would bypass the one human act on it.

8. **The receipt — copied, never retyped.** With every change committed and pushed (`git -C <working dir>/tenant status --porcelain` prints nothing), write the check's output to a file: `boss tenant check <working dir>/tenant > <working dir>/tenant-check.txt 2>&1`, and read its exit status. That file is the receipt, and it must end in `PASS`. A FAIL is not delivered: fix the tree, commit, push and check again, or refuse (rule 10). Read the full sha from git (`git -C <working dir>/tenant rev-parse HEAD`), never from memory.

9. **End the run `delivered`, then hand back.** Complete your run's own `building` step: `boss step complete <your run id> --step building --field result=delivered`. That opens `reported`. Your handback is: the packet id (full), the tenant repo, the branch, the full sha, the path of `tenant-check.txt` together with its contents verbatim, what changed and why, and anything worth a follow-up item (do NOT file items yourself). Whoever records it runs `boss dispatch --report <run> --summary-file <handback> --tenant-check-file <working dir>/tenant-check.txt --tenant-branch <branch> --tenant-sha <sha>`. That command refuses a `delivered` tenant run with no receipt, or with a receipt that does not PASS, so the run lands on the check's own words and never on your account of them.

10. **Refusing is a good outcome.** If the packet's claim no longer holds on the tenant's origin, or the work turns out to be in the BOSS tree after all, build nothing: complete `building` with `result=refused` and report why, with the evidence.

11. **Do not:** touch the BOSS tree (rule 2); write to the jobs API except through `boss step complete` on your own run; run kubectl; read or print any credential or token file; run `boss tenant publish` or `export`; push, merge or force anything onto the instance's ref; start background tasks.
