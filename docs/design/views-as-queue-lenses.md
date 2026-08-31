# Design: views as queue lenses — the view layer joins the network

**Status**: decided — every question answered by David in review `6f33df26`, 2026-08-19; see Decision history.
**Origin:** David, 2026-08-12 (verbatim): "I think we need to rebuild
some of our views to reflect the nature of the network, job packets
operating under specific protocols, and heavy use of data-defined
queues to hold work in progress." And (`3f5f7f63`): "the Design
Review page should really just be a custom view onto a particular
queue or set of queues. That is what many of our pages fundamentally
devolve into. Here is what is in queue, presented usefully, with
context about how that queue has been flowing recently."
**Related**: [job-packet-network.md](./job-packet-network.md) ·
[queue-visibility.md](./queue-visibility.md) ·
[requirements-based-addressing.md](./requirements-based-addressing.md) ·
[departure-board.md](./departure-board.md) — the worked example ·
[workflow-ux-as-data.md](./workflow-ux-as-data.md) — the StepPlugin
precedent this generalizes.

## The claim

A view is three declarations, not a page of code:

- **a queue set** — predicates over packets (kind, station,
  protocol, requirement), the same predicate language the network
  routes by;
- **a presentation** — how a packet renders in this lens (board row,
  card, kanban column, canvas overlay), the way StepPlugins already
  declare step UX as data;
- **a flow window** — the context strip every lens carries: depth
  now, arrivals and drains over the window, oldest waiting — the
  algedonic telemetry queue-visibility Q4 named.

The hand-built pages are each a frozen instance of this triple.
My Day was converted by hand (`354469c8`); the departure board is
spec'd as one; TriageBoard's columns, Fleet's depths, Design
Review's list, and JobsList's filters are all lens triples wearing
bespoke Svelte. Rebuilding the views means extracting the triple
into registry data and letting one renderer serve them — CLAUDE.md
§9's registries-over-hardcoded-paths applied to the read side, the
exact move StepPlugins made for step UX.

## What every rebuilt view shows, uniformly

- **The packet, enveloped**: title, subject, and its **protocol set
  as chips** — `(kind, version)` visible on every rendering, because
  a packet's governance should never be invisible (packet Q6's
  presentation vocabulary starts here, not on a dedicated page).
- **The queue as the container**: WIP is shown *where it sits* — a
  station holding packets — never as a bare status column. Status is
  derived from position (packet Q2); the views stop pretending
  otherwise.
- **The flow strip**: every lens header carries its window — depth,
  in-rate, out-rate, oldest-waiting — computed from the marker
  events the correlation car (`27341d5d`) just made joinable.

## The rebuild map, in car-sized pieces

| Surface | Its lens triple today (implicit) | Rebuild |
|---|---|---|
| My Day | assignments lens; personal + role queues | done (`354469c8`); retrofit the flow strip |
| Departure board | pipeline queues; board idiom | new (departure-board.md) |
| TriageBoard | user-feedback at fork stations; kanban | columns become queue declarations |
| Fleet | per-kind step depths; DAG overlay | depths become flow strips per station |
| Design Review | design-doc-review at review; doc cards | a lens row + doc-card presentation |
| JobsList | ad-hoc filters | a lens *builder* — saved filters are lens rows |
| Canvas | every station; the network itself | the approved build; lenses are its zoomed regions |

Each conversion deletes bespoke fetch/filter code and registers a
lens row; the renderer and the flow strip are built once. The
canvas stays the one wall (`../architecture-decisions.md` §Step UX &
frontend); every lens is a scoped region of the same picture, which
is what makes the company recursion — the scope parameter every layer
endpoint carries from its first version — hold for views too.

## Open questions

None — every question was answered in review `6f33df26` on 2026-08-19; see Decision history.

## Decision history

**Q1 — What is the lens registry's row shape (decided by David in review `6f33df26`, 2026-08-19).**
`lenses(id, title, queue_predicates jsonb, presentation, flow_window, scope, owner_role)` — predicates in the same `boss-expr` surface the admission edge evaluates (PPP's one-evaluator rule), presentation naming a registered renderer the way `step_plugins` rows name bundles, scope as the canvas layers carry it. Append-only, versioned, seeded from the table above.

**Q2 — Are presentations code or data (decided by David in review `6f33df26`, 2026-08-19).**
registered renderers (board, cards, kanban, list) ship in core as the vocabulary; a lens row picks one plus options. Bespoke presentations follow the StepPlugin escape hatch — a plugin bundle named by the row — so a new view idiom never requires a core change. Board and kanban land first (departure board, TriageBoard).

**Q3 — What does the flow strip compute, exactly (decided by David in review `6f33df26`, 2026-08-19).**
depth (now), arrivals/drains over the window, oldest waiting, and a small trend — all from marker events joined on `step_id`, windowed like `/api/views/stages`. One shared endpoint: `/api/views/flow-strip?lens=<id>`, cached at the os-map cadence. The strip is the algedonic surface: a queue nobody drains shows itself on every page that renders it.

**Q4 — Do the bespoke pages get deleted as they convert (decided by David in review `6f33df26`, 2026-08-19).**
yes, ruthlessly (§1, "delete code freely") — each conversion car deletes the page's fetch/filter/render code in the same diff, the way os-map retires into the canvas. A page that survives conversion is a lens row plus at most a thin route alias.

**Q5 — In what order do the cars run (decided by David in review `6f33df26`, 2026-08-19).**
(1) the lens registry + renderer + flow strip, proven by the departure board as first tenant; (2) TriageBoard (kanban renderer, columns-as-queues); (3) Design Review + Fleet; (4) JobsList as the lens builder; (5) My Day retrofit onto the registry it predates. The canvas proceeds independently on its approved design and adopts lens scoping when both exist.

