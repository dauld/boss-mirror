# A probe shape follows the car

On 2026-09-11 seventeen cars were proven by hand in one session. The
bottleneck was never `boss prove` — that verb takes a probe and runs it.
It was not knowing, for a given car, **what shape of evidence its claim
admits**. Every one of those seventeen answers was re-derived from
scratch, and several were re-derived wrongly first.

This is that knowledge. It is keyed to **what the car changed**, because
that is the question a builder actually has in front of them; it is not
a catalogue of techniques looking for an application.

The rules in §Three rules hold for every shape. The mechanics in
§Where a probe runs are host facts, not preferences. The table is
judgement — see §Judgement, not fact.

## What the car changed, and what will prove it

| the car changed | the honest probe | earned on |
|---|---|---|
| a **lint** or a test | a NAMED check in a green gate receipt. The receipt carries one object per check with its own `name` and `result` (77 on a recent run, 69 of them lints individually), so assert the name **and** the result — an absent name must FAIL, not pass silently | the receipt writer in `infra/gate.sh` |
| a **forge verb** | ops-request evidence: requests for that verb exist and reached a completed `execute` step | the ops-runner door |
| behaviour with a **before/after in the SoR** | an **outcome series**. The strongest shape there is: it shows the defect happening, then not | *A gate verdict survives a SoR roll* (`51d493f6`) — `decided_after=102 lost_after=0 lost_before=36` |
| a **gateway route** | 401-vs-404. Routing happens before auth, so a declared `/api` route answers `401 authentication required` and an undeclared one gets the catch-all `404 {"error":"no such API route","path":…}` | measured live 2026-09-11 against `http://10.20.0.30` |
| a surface **behind the gateway's session** — a page, an API answer, or a **static asset** the gateway serves | POST `/api/auth/guest`, send the returned `boss_session` cookie, assert on the bytes | *The sign-off signs after it saves* (`e6cfc706`) — fetched `/plugins/sign-off.js` (24065 bytes), asserted the `v3` self-declaration and the guard `if (stamps.length > 0 && unchanged)` |
| a **CLI verb** | a git read of `main`. **No `/api/` probe is honest here** — the SoR holds packets, and no row changes when a verb lands, so an HTTP read would only restate the packet's own step: a belief dressed as an artifact | the shape of the SoR |
| a **refactor with no observable delta** | none can fingerprint it. Assert the INVARIANT it installs still holds in production, and say in the proof that that is what you are asserting | — |
| something only an **event** can show | `--park-proof-event`. The car stays hand-proven and the yard counts it waiting, which is the truth | — |

A static asset is **not** a row of its own, and the reason is worth
keeping: `/plugins/sign-off.js` answers **401** unauthenticated. The
sign-off car's probe was legal because it carried a gateway session, not
because its target needed no identity. A carve-out keyed on "it's only a
static file" would have exempted something that is not readable
unidentified anyway.

Two sharper notes on those two rows, read out of the authorities rather
than out of experience:

- **Name and result are necessary and not sufficient.** `write_receipt`
  emits the `checks` array on a REFUSAL too (`verdict: "refused"`,
  `refused_because` naming the unfit host), and on a receipt it has
  flagged `unverifiable` — so a name-plus-result grep can be satisfied
  by a run that never judged the branch. Assert `"verdict": "green"`
  and an empty `"unverifiable": []` alongside the named check.
- **401-vs-404 is a GATEWAY fact, not a system-of-record fact.** The
  same two requests against the internal jobs door at `:7900` answer
  `200` with a narrowed world and a body-less `404` — no catch-all JSON,
  no 401, nothing that distinguishes a routed path from an unrouted one.
  A route probe that points at `:7900` proves nothing and says it
  passed. Point it at the gateway.

Why a server-issued session is admissible where a self-asserted header
is not: a forged cookie (`-b 'boss_session=iamtheoperator'`) answers
**401 with a zero-byte body, byte-identical to sending no cookie at
all** (measured 2026-09-11). A forged header is accepted by a trusting
upstream; a forged session is refused at the door.

## Three rules

### 1. The reader does the read; anything may parse its stdout.

`boss_jobs::probe::reads_the_sor_unidentified` refuses a probe that
reads the system of record without saying who it is, because an
unidentified reader is answered with a narrower world **in silence** — a
presence assertion then fails for the wrong reason, and an absence
assertion passes falsely and closes a car on a proof of nothing. The
measured evidence lives in one place, `probe::UNIDENTIFIED_READ_EVIDENCE`,
and every door quotes it.

What trips the rule is an HTTP client in **command position**. The list
is `HTTP_CLIENTS` = `curl`, `wget`, `python`, `python3` — python
correctly, since the first measured instance was `urllib.request.urlopen`.

`jq` is not on that list and **must never be added**: it cannot make a
network request, so it is a parser, not a client. Three consequences,
and they are not the same consequence:

- `curl $BOSS_JOBS_URL/api/… | jq` is still refused, on the `curl`.
  Piping into jq launders nothing.
- `boss-sor-read /api/… | jq` is clean. The identified reader did the
  read; jq only parsed its stdout.
- `boss-api … | python3` is refused although `boss-api` **is** an
  identified door — the detector sees `python3` in command position and
  `/api/` in the text, and cannot see that python3 is being fed stdout.
  That is a **false positive in the predicate, not a rule to argue
  with.** Rewriting the parse to `jq` sidesteps it and usually produces
  a better probe; an override with a stated reason is the other correct
  answer.

Corpus, measured 2026-09-11: **532 probe texts across 451 cars** (80
parked `proof_probe` values plus 452 probe strings inside recorded proof
records), and **10** cars ever carried an override on this rule. **Five
of the ten are the `boss-api … | python3` shape** — the dominant case by
count as well as the sharpest.

One more parsing trap, paid for once already: **`grep` is line-based.**
An expect phrase that wraps across lines in the source can never match,
and the probe reports a false negative rather than an error.

### 2. Pair every absence with a presence control on the same connection, and make the control abort FIRST.

An absence assertion cannot distinguish a fixed system from an
unreadable one. The control is what makes the difference visible, and it
has to run before the claim is evaluated, or a failed read reads as a
clean result.

*A gate verdict survives a SoR roll* (`51d493f6`) is the worked example.
Before it evaluates anything it refuses on two controls: fewer than 100
gate-runs in the window ("too small a window to judge; NOT a verdict on
the claim"), and **zero `lost` verdicts even in the period before the
fix** — "this probe cannot distinguish a fixed system from one whose
lost rows it cannot see". It found 36 historical `lost` rows, so the
reader was demonstrably able to see the thing claimed gone, and only
then asserted `lost_after=0`.

The same shape applies to routes: proving a set of endpoints was deleted
works only if an undeclared control path still produces the catch-all.
If it does not, the probe must say *"this probe cannot tell a routed path
from an unrouted one, so it proves nothing"* — not pass.

### 3. Exercise the failure paths before recording.

*A maintenance protocol states its category* (`a0ab90a5`) sat unproven
for 18 hours on a probe that **could not pass**. Its success branch
emitted jq `empty`, and **`jq -e` exits 4 when its filter produces no
output** — so the claim holding made jq exit nonzero, and the trailing
`|| exit 1` fired precisely when the claim was true. Re-run against
correct live data the next day, it exited 4 on every row it checked.

The shape that cannot invert asserts positively and prints a token:

```sh
jq -e '<claim> or error("…")' >/dev/null && echo claim:ok
```

So: mutate the assertion against real data and watch it fail; remove the
reader and watch it report *reachability* rather than a verdict. And do
not write a bare `|| exit <n>` — it replaces the status that would have
explained the failure (`probe::rewrites_its_exit_status` warns on it).
`|| { echo "…$?"; exit 1; }` keeps the evidence.

## Where a probe runs

The one thing about a probe that is easy to get wrong. A probe is
**authored** on the dev pod, where the cluster is one hop away, and
**run** on the **forge host** — as `david`, in `/home/david/boss`, with
that host's tools, by `infra/forge/run-car-probe.sh`. Two machines. The
forge is outside the cluster and holds no kubeconfig, so a probe that
reaches for `kubectl` is correct and unrunnable.

**`infra/forge/host-absent-tools.txt` is the authority**, and it says
this three ways deliberately. Read it rather than a restatement of it:
it carries every tool measured absent (`kubectl`, `boss`), the
distinction from the CI runner image's `required-tools.txt`, the reason a
guessed entry is worse than the failure it prevents, and the identified
reader (`boss-sor-read`, already in the converged checkout and first on
the probe's PATH) that a probe reads the SoR with there.

`boss gate --park-probe` checks that file at gate time, on the builder's
terminal, so the common case never reaches the forge as an exit code
hours later on a car.

A probe run by hand with `boss prove` runs wherever you are, which is
usually the pod — so `kubectl` and `boss` are available to a hand proof
and not to a parked one. That asymmetry is the single most common reason
a probe that worked yesterday fails at arrival.

## Judgement, not fact

**The table is judgement.** Which shape of evidence a claim admits is a
call, informed by the cars above and by nothing stronger. No test holds
it, and a weak assertion pretending otherwise would be a worse lie than
this paragraph. Read it as argued, and argue with it.

These four are facts, and
`crates/core/boss-jobs/tests/a_probe_shape_follows_the_car.rs` holds
each against its own authority — failing with the line of this document
to fix if an authority moves:

1. `jq` is not in `HTTP_CLIENTS` while `python3` is, and `curl … | jq`
   is still refused (asserted through `reads_the_sor_unidentified`, the
   predicate every door shares).
2. `jq -e` exits 4 on empty output (asserted by running `jq`, not by
   citing it).
3. The forge lacks `kubectl`, per `infra/forge/host-absent-tools.txt`.
4. A gate receipt names its checks individually (asserted against
   `write_receipt` in `infra/gate.sh`, including its `"checks"` array).

The test also pins the two pointers that make this document findable —
that label above, and its own path — because a reference whose facts
have gone quietly false is worse than no reference. That is not
hypothetical: on 2026-09-11 ten builder briefs carried a probe rule that
had stopped being true, and every one of them was believed.
