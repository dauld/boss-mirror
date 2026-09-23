# Design: the human-powered state machine — the execution lens

**Status**: approved — framing doc; no implementation work.
**This is a lens, not the foundation.** The primary frame is
[the-three-layers.md](./the-three-layers.md): *the network is the
substrate, the fat protocols dictate the current operating model, the
actors run it.* This doc is the **execution lens** over those three
layers — it answers "how does a single packet get executed", and it is
a good frame for that: the log as memory, the StepType registry as the
instruction alphabet, a step's `status` as a program counter, policy as
the privilege model. It is not the substrate; the network is. **When
the two framings disagree in a design argument, the network framing
wins**, because it is the one that survives changing the operating
model. Read everything below with that pointer in mind: where this
doc says "the machine", the three-layer frame says "the substrate plus
the protocol a packet is pinned to"; where it says "CPU", that is an
**actor** (layer 3), unchanged.
**Related**: [the-three-layers.md](./the-three-layers.md) — the primary
frame this is a lens over ·
[architecture-diagram.md](../architecture-diagram.md) ·
[CLAUDE.md](../../CLAUDE.md) ·
[extending-boss.md](./extending-boss.md)

---

## The framing (as a lens)

**BOSS is the software layer of a state machine whose executors are
humans (and, increasingly, agents).** The software does not *run* the
business; it *describes* the business as a state machine and gives the
humans instrumentation to execute it safely. When we build BOSS we
are not building an "app" in the conventional sense — we are building
the memory, the display, and the program counter for a machine whose
CPUs are the people sitting at computers, clipboards, and benches.

This framing is not metaphorical. Every decision below has a literal
mapping:

| State-machine concept | BOSS artifact |
|---|---|
| Persistent state | `audit_log` + projections (Assets, Accounts, Ledger, …) |
| Alphabet of transitions | StepType registry (~43 opcodes) |
| Program | Workflow registry (StepTypes wired into an implicit DAG, versioned) |
| Program counter | A Step's `status` + the Job's open step set |
| Dispatcher / scheduler | Messages inbox + My Day |
| Preconditions on a transition | each Step's `ready_when` predicate + policy rules |
| Postconditions on a transition | Metadata schema validated at `status=completed` |
| Durable transition record | Events on NATS → `audit_log` → projections |
| CPU | A human (primarily) or an agent |
| Instrumentation | The web SPA, the `boss` CLI, the ICS feed |

---

## Mapping to State · Surfaces · Work

The `/architecture` page already organizes BOSS into **State**,
**Surfaces**, and **Work**. That framing stays, but re-read through
the state-machine lens it becomes more precise:

- **State** is the machine's memory. Facts are immutable (the
  `audit_log` is append-only); projections are the "efficient reads"
  of that memory; the ledger is a specialized projection with its own
  posting rules. The KB is compile-time-ish constants — the values
  that transitions consume but don't usually mutate.
- **Surfaces** are how CPUs observe memory. A page, a panel, an inbox
  message, a calendar event, a CLI table — these are all reads against
  State with one job: let the human form an accurate enough model of
  State to choose the next action.
- **Work** is what happens when a CPU fires an instruction. A Step
  flipping to `status=completed` with valid metadata is a single transition; a
  Job closing is a sequence of transitions terminating at its sink
  state. Every transition emits one or more Events, which mutate
  State, which changes what every other CPU sees.

The loop is `State → Surfaces → CPUs (humans) → Work → State`. It
is a closed feedback system, not a pipeline. If you can't draw the
cycle for a feature you're designing, the feature is probably
incomplete.

---

## Where humans fit, explicitly

Humans are a named component of the machine, not implicit "users of
the app":

- **Every Step has an assignee.** That assignee is the CPU executing
  this transition. Unassigned Steps are pending instructions with no
  CPU currently scheduled on them.
- **"My Day" is the CPU's instruction queue.** Per-employee, it ranks
  the Steps that human should execute next. Without it a human has
  no program counter.
- **Policy is the privilege model for CPUs.** A rule says "this CPU
  may execute this transition in this scope". Every write passes
  through it because state machines need hard contracts on who may
  transition what.
- **Messages are inter-CPU signalling.** DMs, system signals, and
  escalations are all messages on a shared bus that route attention
  to the CPU that needs to act next.
- **Sign-off gates name a required second CPU.** Steps with a
  non-empty `sign_offs_required` encode a two-phase-commit transition:
  one CPU proposes, another CPU confirms.

---

## Primitives through this lens

Four primitives — Subjects, Jobs, Steps, Events — take on sharper
roles:

### Subjects
The nouns the machine tracks across time. Each Subject (`Account`,
`Employee`, `Vendor`, `PurchaseOrder`, `Campaign`, … — see the
`subject_kinds` registry for the full roster) is a named cell of
memory with its own event log. A Step on a Job always
acts on exactly one Subject, which is how the machine knows which
cell a transition writes into.

### Jobs
A Job is a **program invocation**. Its Workflow is the program; the
versioned `steps` set (the DAG implicit in each step's `ready_when`)
declares the call shape. A Job opened under
`v3` of `field-service` executes against `v3`'s semantics for the
rest of its life, even if `v4` ships midway. This is exactly how
ISAs version.

### Steps
Steps are the opcodes currently loaded into the running program.
`status` is the program counter; each Step's `ready_when` predicate
declares its data dependencies on other opcodes (the `blocked_by`
edge list is recovered from those predicates for rendering); metadata
fields are the opcodes' typed arguments. The "required at done, not required at create" rule is
the classic compiler distinction between declared variables and
defined ones — the argument must be bound before the opcode can
retire.

### Events
The durable transition log — `audit_log` is its physical home.
Every state change emits one immutable fact. The log is the source
of truth; everything else is a projection. Replay reconstructs the
machine's history from t=0.

## Supporting concepts through this lens

- **Class registry** — the machine's typed reference data. Subject
  classes (roles, account types, system models) are constants and
  type declarations, rarely mutated by Steps. When a Class row is
  edited it's closer to a schema migration than a transition.
- **StepPlugins** — the microcode layer. When a StepType's default
  rendering isn't rich enough for a particular opcode, a plugin
  overrides it with custom instrumentation — but the opcode's
  contract (fields, validation) is unchanged. Plugins do not extend
  the alphabet; they just render it better.
- **Policy** — privilege model on CPUs. Every write passes through.
- **Messages** — inter-CPU signal bus.

---

## Agents as additional executors

Agents (registered in the `agents` registry and dispatched onto
steps by `boss dispatch`) are **not a separate
system that talks to the state machine from outside**. They are
additional CPUs inside the same machine. They read the same State
through the same ports, they execute the same StepTypes, they pass
through the same policy gate, they emit the same events.

The only thing that differs between a human CPU and an agent CPU is
**latency, throughput, and legal scope** — all of which are policy
knobs, not architectural ones. This is why the agent plumbing (the
claim door's budget and concurrency bounds, the `agent-run` packet)
is explicitly *not* business-event handlers: it is "how we mount
additional CPUs into the machine", not a place where business logic
lives. (It used to be a crate of its own, `boss-cybernetics`, retired
2026-09-23 as superseded-by — see docs/architecture-decisions.md.) Business
event handlers live next to the publishers that emit the events
they react to (the escalation router, the bank-sweep handler —
all in their domain services).

When we eventually auto-execute routine Steps (e.g. "schedule a preventive maintenance
visit off an agreement's cadence"), that is literally assigning an
agent CPU to an opcode that used to be hand-dispatched. No new
architectural shape is needed.

---

## Invariants this framing gives us

A design is aligned with the framing when it preserves these
invariants. A design that violates one is suspect and probably
hiding a modeling error.

### I-1. Every business state change is a transition in the log.
If State changes without an event, we have leaked state. A surface
that renders numbers the event log doesn't explain is a bug.
Projections are deterministic functions of the log.

### I-2. Every transition has a named CPU and a legal scope.
`actor_id` in event payloads, `assignee_id` on Steps, `owner_id` on Jobs.
If we cannot name who executed a transition, we cannot audit it and
we probably shouldn't be allowing it.

### I-3. The alphabet is closed; the programs are open.
New StepTypes land rarely, and when they do it is a deliberate
extension of the machine's alphabet. New Workflows land often, as
data. If a proposed feature requires a new StepType and a new core
code path to implement it, we are probably modeling it at the wrong
layer.

### I-4. Preconditions are declarative, postconditions are enforced.
Each Step's `ready_when` predicate declares what must have happened
upstream (the `blocked_by` edge list is derived from it).
Metadata validators enforce that the transition actually completed
before `status=completed`. The distinction is load-bearing: a Step can
exist (and be visible to the CPU) without being done; it can't be
claimed done without meeting its postconditions.

### I-5. Humans can always find their next opcode.
"My Day", the unified inbox, the Step assignee field, and the
deep-link-to-the-step URL together give every CPU the answer to
"what do I do next?" If a role in the org can't reliably answer
that question using BOSS alone, we have a dispatcher gap.

### I-6. The machine describes real operations, not aspirational ones.
If the tenant actually performs some business transition, it must have a
StepType. If a StepType exists but no Workflow uses it, it's dead
metal. Both directions matter: documentation-only constructs are
smells.

---

## What this changes in practice

Not much in the code — the framing codifies what's already there. The
payoff is in **how we talk about new work and review new designs**:

1. **When adding a new workflow**, the first question is "which
   existing StepTypes does it need and what's the Workflow's DAG?"
   Not "which crate owns this?" If a new StepType is unavoidable,
   that's a deliberate extension of the machine, not a detail.

2. **When adding a new page**, the first question is "what State is
   it projecting, and which CPU is it instrumenting?" Surfaces
   without a named audience are usually re-creating data that
   belongs somewhere else.

3. **When a bug shows up**, ask "which invariant did it violate?"
   "The commerce service silently dropped agreements" is I-1 — a
   transition happened on the sim side but never landed in the log.
   A stale-binary 404 at the API boundary is the same invariant
   failing: the sim emitted transitions the state machine couldn't
   commit.

4. **When onboarding a new person (or an LLM)**, lead with the
   state-machine framing instead of the "four primitives" slide.
   The primitives make sense much faster when the reader already
   knows what kind of machine they're building.

5. **When deciding whether a change is "feature" vs "architecture"**,
   ask whether it extends the alphabet (StepType), the program
   library (Workflow), the rendering layer (StepPlugin), or just
   adds a row to the program library. Only the first is architecture;
   the rest are data changes with matching UI.

---

## What this framing is *not*

- It is **not a claim that humans are replaceable by agents**. It's a
  claim that they both occupy the same slot in the machine, which
  means both benefit from the same rails (policy, events, Steps) and
  both are audited the same way.
- It is **not a call for a DSL, workflow engine, or BPMN rewrite**.
  We already have the machine; the framing just names it. Adding a
  purpose-built DSL would be reinventing what Workflow + StepType
  already do.
- It is **not a permission to over-model**. Not every entity is a
  Subject, not every action is a Step, and not every screen is a
  Surface. The framing sharpens decisions we were already making; it
  does not demand we shoehorn everything in.

---

## Open questions

None at the moment — this is a framing doc, not a proposal. If the
vocabulary doesn't land for people using it in conversation or
design review, that's the signal to revise it.
