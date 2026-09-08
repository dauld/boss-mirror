# Design: the IT surface consolidates

**Status**: settled — all four questions answered by David 2026-08-31 (packet `1f6d55e0`); carried to this file by the car that executed them.

**Origin**: David, 2026-08-31: *"Don't put it under /system, we put pages in the department they are for"* and, minutes later, *"We do have too many IT pages though. We should consolidate."*

## The measured problem

17 Page components across 14 directories, and the /it-vs-/system split never finished: four surfaces (yard, incidents, dispatcher rules, design) routed under **both** prefixes, with a long /system tail (workflows, feedback, monitoring ×3, kb, step-plugins, subjects, policy). The router's own comment promised /system would answer "permanently and deliberately" — a promise made for legacy users that turned out not to exist.

## The shape

Six surfaces, families as tabs, one department home:

| surface | carries |
|---|---|
| `/it` | the Train Yard — delivery truth is the landing |
| `/it/operate` | Incidents · Audit Log · Performance · Atlas · Bottlenecks |
| `/it/registry` | Workflows · Dispatcher · Step plugins · Policy · Subjects |
| `/it/design` | Reviews · Experiments · Feedback |
| `/it/estate` | the hardware registry + the dev-workspace door |
| `/it/kb` | knowledge base |

`/it/auth-admin` stays as an unlisted door. The tab strips live in one data-driven component (`ItTabs.svelte`); the sidebar holds exactly six rows and a test pins that number.

## Decision history

- **Q1 — retire /system entirely?** *"Yes."* Every /system route now falls through to the catch-all like any unknown path. No redirects, per Q4.
- **Q2 — 17 → 6 with families as tabs?** *"Yes."* Registry was the biggest fold: five pages onto one surface.
- **Q3 — map / flow / fleet?** *"Fold them."* Read against the ratified proposal text this means: the duplicate **renderings** die (StationMapPage, FlowPage/FlowNetwork, and the SystemModelPage wrapper — its live view moved to the landing page that also used it), their unique **series** fold into Atlas (the station registry table with the one-pass load read, and the stage-runs card), and Fleet — a genuinely distinct per-kind dashboard, not a duplicate — survives as Operate's Bottlenecks tab. Page usage remains unmeasurable; nothing else was deleted on a guess.
- **Q4 — sequencing?** *"We don't need to worry about legacy users. It is just me, and I know all the changes, so sequence as quickly as possible."* One car, no redirect ceremony. This reverses the router's earlier kept-permanently promise, knowingly.

## What enforces it

- `nav-catalog.test.ts`: the sidebar holds exactly six IT rows; every other IT surface must appear in `ItTabs.svelte` or on the short documented-doors list — the executable form of this document.
- `router.test.ts`: the /it table is the routing spec; /system paths assert nothing because they no longer exist to assert about.
- The permission vocabulary shrank with the pages: `system-map`, `system-flow`, `system-fleet`, `system-model` left `RouteName` entirely.

## Residue

The station registry's `upstream` hrefs and any docs still naming /system paths are registry/doc data, not code — they resolve to the catch-all until touched, and each is a one-line fix at next contact. Deliberately not swept here.
