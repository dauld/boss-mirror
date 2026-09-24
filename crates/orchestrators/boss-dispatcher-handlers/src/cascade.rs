//! Static cascade metadata for the dispatcher-rules visualization.
//!
//! The rule registry declares the reactive layer as `trigger event → rule →
//! handler(s)`. To render the full *cascade* — the feedback loops that
//! make the state machine self-drive — the graph also needs two facts
//! that are not in the rule registry:
//!
//!   1. **What each handler causes to be emitted** downstream (by the
//!      API it calls). A loop closes wherever an emitted kind matches
//!      some rule's `on_event` (e.g. `inventory.parts.consume` emits
//!      `inventory.item.consumed`, which re-triggers `spawn-restock`).
//!   2. **The jobs-api / external consequences** that re-enter the rule
//!      set but are NOT dispatcher rules themselves (a new Job's steps
//!      becoming ready; a completed step emitting its `done` topic; an
//!      invoice later being paid by a counterparty). Rendered distinctly
//!      so dispatcher rules stay visually separable from system/external
//!      wiring.
//!
//! Both are authored here — a documented, central "what each side-effect
//! causes" — handed to core's read surface as one
//! [`Cascade`](boss_dispatcher::cascade::Cascade) by the binary that
//! registers these handlers ([`cascade`]), and served by
//! `GET /api/dispatcher/rules`. Kept in sync with the handlers in
//! [`crate::handlers`]; `cascade_handlers_match_rules` (tests) guards
//! against a rule referencing a handler this map forgot, reading the
//! authored registry directory that IS the rule definition.
//!
//! WHY IT LIVES IN THIS CRATE (backlog ec40e269, 2026-09-23). It was
//! `boss_dispatcher::cascade` — a core crate spelling company-module
//! handlers and the AR loop — while the code it describes lived here.
//! Measured on the tree that moved it: 52 handlers, 30 invoked by a
//! product rule under `infra/dispatcher/rules/`, 21 by the example
//! tenants' seeds only (every company-module handler, plus
//! `webhook.notify`), and one, `jobs.complete_step_matching`, by a rule
//! only the operator's own tenant repository declares. The map still
//! lists every handler because this binary still registers every one,
//! and a tenant's rules resolve against exactly this roster (`boss
//! tenant check`); a handler cannot yet be registered by a tenant, which
//! is what keeps the example tenants' handlers in a crate every
//! deployment builds.
//!
//! THIS IS NOT THE RULE REGISTRY'S SECOND COPY, and the question was
//! settled when the registry collapsed to one home (backlog 41ba00cd).
//! The note that used to stand here said the `dispatcher_rules`-registry
//! migration "will fold this into first-class rule/handler metadata". It
//! will not, because the fact is a different one: a rule row says which
//! handler it invokes, and this map says what that HANDLER emits, which
//! is a property of the handler's Rust and of nothing a rule author
//! writes. Moving it onto the rule row would make every rule naming the
//! same handler restate it — CLAUDE.md §9a's duplication, created rather
//! than removed. So it stays one declaration, in the crate whose code it
//! describes, pinned to the rule registry by the guard below.

use boss_dispatcher::cascade::{Cascade, SystemEdge};
use serde::Serialize;
use std::collections::BTreeMap;

/// Event kind(s) each handler causes to be emitted downstream, keyed by
/// the handler's registered name (the `handler = "..."` in a rule file).
/// An empty list is a pure sink (notifier / webhook — emits nothing).
pub fn handler_emits() -> BTreeMap<&'static str, Vec<&'static str>> {
    BTreeMap::from([
        ("inventory.parts.consume", vec!["inventory.item.consumed"]),
        (
            "inventory.overhead.absorb",
            vec!["inventory.overhead.absorbed"],
        ),
        ("commerce.invoice.issue", vec!["commerce.invoice.created"]),
        (
            "products.produce",
            vec!["products.produced", "products.inventory.upserted"],
        ),
        (
            "products.consume",
            vec!["products.consumed", "products.inventory.upserted"],
        ),
        (
            "products.consume_from_invoice",
            vec!["products.consumed", "products.inventory.upserted"],
        ),
        (
            "inventory.po.place",
            vec!["inventory.purchase_order.upserted"],
        ),
        (
            "inventory.receive",
            vec!["inventory.item.received", "inventory.po.status_changed"],
        ),
        (
            "inventory.bill.approve",
            vec!["inventory.vendor_invoice.approved"],
        ),
        ("ledger.bill.approve", vec!["ledger.bill.approved"]),
        (
            "inventory.bill.payment_batch",
            vec!["inventory.vendor_invoice.paid"],
        ),
        ("ledger.bill.payment_batch", vec!["ledger.bill.paid"]),
        ("ledger.tax.accrue", vec!["ledger.tax.accrued"]),
        // One POST, two facts: the settlement endpoint records the
        // charge and release legs in one transaction, each with its
        // own audit event for the rebuild bridge (93f936b9).
        (
            "ledger.keg_deposit.settle",
            vec!["ledger.keg_deposit.charged", "ledger.keg_deposit.released"],
        ),
        // Two emits: the handler POSTs the filing (which records
        // `ledger.tax.filing.created` — the event `tax_filings` is
        // projected from) and then, when `remit=true`, follows with the
        // remit POST.
        (
            "ledger.tax.remit",
            vec!["ledger.tax.filing.created", "ledger.tax.remitted"],
        ),
        ("ledger.payroll.run.submit", vec!["ledger.payroll.run"]),
        ("people.hire", vec!["people.employee.created"]),
        ("people.terminate", vec!["people.employee.updated"]),
        ("shipping.create", vec!["shipping.shipment.created"]),
        ("jobs.spawn", vec!["jobs.job.created"]),
        // The week's retros (1dffde5d): one department-retro per
        // department the classes registry holds, plus the platform's
        // protocol-retro, each a POST /api/jobs — so the only emit is
        // the packet it creates, the same as jobs.spawn. The dedup
        // reads are GETs and a skip is a no-op.
        ("retro.open", vec!["jobs.job.created"]),
        // Files one ops-request per probed car aboard an arrived
        // train (28ac45ab); the probe itself runs on the forge and
        // writes back through the jobs API as its own actor, so the
        // only emit this handler owns is the packet it creates.
        ("jobs.run-car-probes", vec!["jobs.job.created"]),
        // Auto-park (898e41b1): on a gate-run's GREEN gate-verdict step
        // it files the ship-a-change car (`jobs.job.created`), completes
        // that car's three receipt steps (`jobs.step.completed`), and —
        // on the skip/re-gate branches, and when `--park-backlog-item`
        // routes the linked item to `build` — PATCHes metadata
        // (`jobs.job.updated`). Every one is a car or backlog-item write,
        // so the only rule that can re-enter on them is the backlog-item
        // advance rule; nothing it writes is a gate-run, so the park
        // cannot trigger its own trigger.
        (
            "jobs.auto-park",
            vec![
                "jobs.job.created",
                "jobs.step.completed",
                "jobs.job.updated",
            ],
        ),
        ("jobs.complete_step", vec!["jobs.step.completed"]),
        // Clears waiting_on via PUT /api/jobs — the update emits
        // jobs.job.updated (and wakes metadata-gated steps in the
        // same write, aa9980c8).
        ("jobs.clear_waiting", vec!["jobs.job.updated"]),
        (
            "maintenance.sweep.inspect",
            vec!["jobs.step.completed", "jobs.job.updated"],
        ),
        // An answered sweep measurement judges the sweep that asked
        // for it (970c0c94): a clean `verdict:` line routes the sweep
        // (`jobs.job.updated`, action_needed) and completes its inspect
        // checklist (`jobs.step.completed`); an unclean one merges the
        // reading onto the still-open step (`jobs.step.updated`). Every
        // write is on a maintenance-sweep, never an ops-request, so the
        // close it fires on cannot re-enter it.
        (
            "maintenance.sweep.judge",
            vec![
                "jobs.step.completed",
                "jobs.job.updated",
                "jobs.step.updated",
            ],
        ),
        // An answered ops-request's verdict line files the next verb
        // (a1d3c762): a follow-on ops-request (`jobs.job.created`) and
        // a `judged` note on the request it read (`jobs.job.updated`).
        // The follow-on closes `answered` through the same trigger, but
        // a rule never judges the packet it files (same verb, same
        // args), and its answer is a different line, so the chain ends
        // at the follow-on by construction.
        ("ops.judge", vec!["jobs.job.created", "jobs.job.updated"]),
        // A release packet's `tag` step going ready files the
        // tag-release ops-request for the forge (89c95245): one
        // `jobs.job.created`, an ops-request, never a task step, so
        // the shared step.ready.task topic it fires on cannot re-enter
        // it; the request's answer completes the step through
        // complete-release-tag-on-tag-release-answered.
        ("ops.file_tag_release", vec!["jobs.job.created"]),
        // A chore that closed red opens one backlog-item per RED route
        // on its recorded step (ac3270c7): items (`jobs.job.created`)
        // and a `judged` note on the chore it read (`jobs.job.updated`).
        // Every item is a backlog-item, never the chore kind the close
        // fires on, so the chain ends at the operator's queue the way
        // estate.alarm's does.
        (
            "maintenance.chore.file_reds",
            vec!["jobs.job.created", "jobs.job.updated"],
        ),
        ("jobs.subjob_resolve", vec!["jobs.step.completed"]),
        // Completes the open branch on the Job a declared edge names
        // (a merged car answering its feedback packet). The completion
        // is what closes the loop: jobs.step.completed → step.done.* →
        // the packet's own `closed` terminal → jobs.job.closed, which
        // re-enters the rule set at notify-filer-on-feedback-terminal.
        // Under `on_failure = "annotate-and-alert"` (f47861a5) a FAILED
        // verb files one urgent backlog-item instead (`jobs.job.created`)
        // and annotates the open step, which is not a completion; the
        // item is the operator's queue, where the chain ends.
        (
            "jobs.complete_linked_step",
            vec!["jobs.step.completed", "jobs.job.created"],
        ),
        // Completes a step on every open packet whose recorded step
        // metadata matches a value the closing Job carries — the
        // converge that records a site's hash making that site's
        // packet `live` (c34583cb). No rule this tree ships names it;
        // the rule is the tenant's.
        ("jobs.complete_step_matching", vec!["jobs.step.completed"]),
        // A clock rule completes an open step on every packet of a
        // kind that has gone silent past a bound (c87fb59b car 2: an
        // agent-run whose builder died). The completion carries the
        // packet to its own terminal; nothing listens for a run's
        // close, so the loop ends at the packet.
        ("jobs.age_out_step", vec!["jobs.step.completed"]),
        // The real-work watch (078ddcb0): an hourly clock rule that
        // files (`jobs.job.created`) one backlog-item alarm per step
        // held by an agent past its workflow's declared bound, and
        // withdraws it through the step the alarm waits on
        // (`jobs.step.completed`) or a note (`jobs.job.updated`) once
        // the step no longer waits. Every write is to its OWN alarm; it
        // never touches the late step, so it cannot move what it watches.
        (
            "jobs.agent_step_overdue",
            vec![
                "jobs.job.created",
                "jobs.job.updated",
                "jobs.step.completed",
            ],
        ),
        // The stale-flight watch (design c4c2a607, 73c31776): an hourly
        // clock rule that files (`jobs.job.created`) one backlog-item
        // alarm per flight past its observe period or decided and not
        // cleaned up, and withdraws it through the step the alarm waits
        // on (`jobs.step.completed`) or a note (`jobs.job.updated`).
        // Every write is to its OWN alarm; it never touches the flight.
        (
            "jobs.flight_overdue",
            vec![
                "jobs.job.created",
                "jobs.job.updated",
                "jobs.step.completed",
            ],
        ),
        // The routing half of the same judgement (a3397b01): a step
        // held by a run that DIED is released — `ready`, unassigned,
        // the dead run's edge cleared. A step UPDATE, never a
        // completion: releasing work is not doing it. Nothing listens
        // on `jobs.step.updated`, and the released step is handed out
        // again by the durable inbox's queue READ, not by an event, so
        // the loop ends at the board.
        ("jobs.reclaim_abandoned_step", vec!["jobs.step.updated"]),
        ("gate.resolve", vec!["jobs.step.completed"]),
        ("packaging.allocate", vec!["jobs.step.completed"]),
        // The packet-loss census (migration 152): reads the whole
        // board through the jobs API and lands one
        // `jobs.network.census` event per firing via the census door.
        // No rule listens on that topic — the series is for lenses,
        // not for the cascade — so the loop terminates here by design
        // (packet-loss.md Q2: report first, raise later).
        ("network.census", vec!["jobs.network.census"]),
        // The estate comparison (59ef456a): fires on each
        // `jobs.estate.observed`, reads the registry through the jobs
        // API, and lands one `jobs.estate.compared` event via the
        // comparison door. No rule listens on that topic — the series
        // is for lenses and for calibrating the eventual raiser, so
        // the loop terminates here by design, same as the census.
        ("estate.compare", vec!["jobs.estate.compared"]),
        // The raiser on that series (a5adfb99): it POSTs an urgent
        // backlog-item when a hard finding persists or a watched series
        // goes stale, and nothing else — the dedup read is a GET and a
        // non-raise is a no-op. A backlog-item write, so it cannot make
        // the estate series it watches look any different, and the loop
        // terminates at the operator's queue by design (delivery beyond
        // the queue is channel work, not this handler's).
        ("estate.alarm", vec!["jobs.job.created"]),
        // The raiser's closing half (ef421cd3): when a finding has
        // been absent N consecutive comparisons, it completes the
        // alarm packet's triage step (`jobs.step.completed`, which
        // carries the backlog-item to its `stale` terminal) and
        // merges `recovered_at` onto the packet (`jobs.job.updated`).
        // Both are backlog-item writes; neither can make the estate
        // series it reads look any different, so the loop ends at
        // the packet the same way the raiser's does.
        (
            "estate.recover",
            vec!["jobs.step.completed", "jobs.job.updated"],
        ),
        // The cadence silence sweep (ecca2f43): a daily clock rule
        // that reconciles each DECLARED cadence against the newest
        // ACTUAL packet of that kind. Three writes, all through the
        // jobs API — it FILES an alarm (`jobs.job.created`), REFRESHES
        // a standing one's metadata (`jobs.job.updated`), and CLOSES
        // its own alarm when the kind comes back by completing the
        // packet's triage step (`jobs.step.completed`, which carries
        // the backlog-item to its `stale` terminal). Every one of them
        // is a backlog-item write, so the only rule that can re-enter
        // on them is the backlog-item advance rule, and none of that
        // reaches a maintenance kind: the sweep cannot make the
        // cadences it watches look busier.
        (
            "cadence.silence.sweep",
            vec![
                "jobs.job.created",
                "jobs.job.updated",
                "jobs.step.completed",
            ],
        ),
        // The credential broker (7ee101aa): fires on a rotation
        // packet's scope step, speaks to the forge admin API + the
        // k8s Secret store, and records issue/install/verify/revoke
        // by completing that same packet's steps — so its only
        // in-system emission is `jobs.step.completed`, same as the
        // other step-completing executors above. Deliberately NOT
        // `step.done.credential-rotation`: the steps it completes are
        // `task` kind, so the loop cannot re-enter its own trigger.
        ("credential.rotate.forgejo", vec!["jobs.step.completed"]),
        // Same shape, second issuer (04e5f833): completes `task`
        // steps of the rotation packet, never a credential-rotation
        // step, so it cannot re-enter its own trigger.
        (
            "credential.rotate.cloudflare-tunnel",
            vec!["jobs.step.completed"],
        ),
        // The zone observer (5e58922c): fires on a dns-zone-observation
        // packet's `observe` step, reads the zone with the broker's
        // Cloudflare root token, runs the tree's comparator, and
        // completes that same `task` step with the verdicts — so it
        // cannot re-enter its own trigger. On DRIFT/ABSENT it also files
        // (`jobs.job.created`) or refreshes (`jobs.job.updated`) the
        // `dns_drift:<zone>` estate alarm, a backlog-item write that ends
        // at the operator's queue the way estate.alarm's does.
        (
            "dns.observe",
            vec![
                "jobs.step.completed",
                "jobs.job.created",
                "jobs.job.updated",
            ],
        ),
        // The sensor poll (14c9b2ad): a five-minute clock rule that reads
        // the sensor registry and each due sensor's SOURCE outside BOSS
        // (Stripe's charges), records readings outside the audit log,
        // and OPENS one packet per new reading of the kind the sensor
        // row declares (`jobs.job.created`) — the audit-log fact. An
        // unreadable sensor files (`jobs.job.created`) or refreshes
        // (`jobs.job.updated`) the `sensor_unreadable:<id>` estate alarm
        // and the next good read closes it through its triage step
        // (`jobs.step.completed`). Nothing it emits reaches a clock, so
        // it cannot re-enter its own trigger; the opened packet's own
        // protocol carries on from there.
        (
            "sensor.poll",
            vec![
                "jobs.job.created",
                "jobs.job.updated",
                "jobs.step.completed",
            ],
        ),
        // The ops-runner queue watch (a45b38c1): a five-minute clock rule
        // that reads the open ops-request queue and, per host whose
        // oldest waiting request is past the bound, files
        // (`jobs.job.created`) or refreshes (`jobs.job.updated`) the
        // `ops_queue:<host>` estate alarm, withdrawing it through the
        // step it waits on (`jobs.step.completed`) once the queue
        // drains. Every write is a backlog-item write; it never writes
        // an ops-request, so it cannot make the queue it watches move.
        (
            "ops.queue.alarm",
            vec![
                "jobs.job.created",
                "jobs.job.updated",
                "jobs.step.completed",
            ],
        ),
        ("messages.notify", vec![]),
        // Tells the filer how their packet ended. A sink, like every
        // other notifier — the message is the end of the cascade, not
        // a new branch of it.
        ("messages.notify_job_terminal", vec![]),
        // Archives the unread SIGNALS about a job when it closes
        // (rule expire-signals-on-job-closed, migration 128). A sink:
        // it records messages.message.archived per row it touches, and
        // no dispatcher rule listens on that topic — archiving a stale
        // notification must not wake anything up, which is the whole
        // point of archiving it.
        ("messages.expire_for_job", vec![]),
        // Archives the step notifier's unread notices — directs
        // included — when their step ends or their job closes (rules
        // expire-notices-on-step-ended / -on-job-closed, backlog
        // 0b2bac00). A sink for the same reason as the one above.
        ("messages.expire_notices", vec![]),
        ("webhook.notify", vec![]),
    ])
}

/// The non-rule edges that complete the cascade: jobs-api step-lifecycle
/// mechanics + the AR collections loop.
///
/// Every jobs-API topic here is spelled through `boss_jobs::events`, so a
/// renamed kind fails to compile rather than draw an edge nothing travels.
/// The `step.<family>.*` topics have no constant — boss-jobs formats them
/// per step kind — so the test
/// `the_step_families_the_jobs_api_publishes_are_the_system_edges_heads`
/// holds them equal to the families its source publishes, and
/// `every_shipped_trigger_has_something_upstream` holds the whole list to
/// the rule directory (CLAUDE.md §9a). Until backlog cf13bd12 this was
/// five hand rows pinned only by `!is_empty()`, and it missed
/// `jobs.job.closed` and `step.assigned.*`: 23 of 46 event-triggered
/// rules drew on /it/registry/dispatcher with nothing upstream, the
/// largest trigger class among them, so no loop that closes through a
/// packet's closure was drawn or lit as a cycle.
pub fn system_edges() -> Vec<SystemEdge> {
    use boss_jobs::events::{JOB_CLOSED, JOB_CREATED, JOB_UPDATED, STEP_COMPLETED};
    vec![
        SystemEdge {
            from: JOB_CREATED,
            to: "step.ready.*",
            kind: "jobs-api",
            label: "a new Job's entry steps become ready",
        },
        SystemEdge {
            from: STEP_COMPLETED,
            to: "step.done.*",
            kind: "jobs-api",
            label: "a completed step emits its done topic",
        },
        SystemEdge {
            from: STEP_COMPLETED,
            to: "step.ready.*",
            kind: "jobs-api",
            label: "completing a step readies its dependents",
        },
        // A Job PUT re-evaluates readiness against the pinned Workflow,
        // so a metadata write wakes a metadata-gated step in the same
        // transaction (`reevaluate_and_persist`, aa9980c8) — the edge
        // `jobs.clear_waiting` exists to travel.
        SystemEdge {
            from: JOB_UPDATED,
            to: "step.ready.*",
            kind: "jobs-api",
            label: "a metadata write wakes a metadata-gated step",
        },
        // Completing a terminal step closes its packet, and so does the
        // all-steps-terminal catch-all (both in boss-jobs http/steps.rs).
        // No handler closes a Job by a status PUT — the third close site
        // is an operator's — so the step completion is the only in-system
        // cause, and it is the one every rule on the close travels.
        SystemEdge {
            from: STEP_COMPLETED,
            to: JOB_CLOSED,
            kind: "jobs-api",
            label: "completing a terminal step closes its packet",
        },
        // The dispatcher's own assignment loop (dispatcher.rs, not a
        // rule) places a ready, unassigned step with an executor through
        // a step PUT, and the jobs API marks the assignee change. A claim
        // by an actor is the same marker from outside the rule set.
        SystemEdge {
            from: "step.ready.*",
            to: "step.assigned.*",
            kind: "jobs-api",
            label: "the assignment loop places a ready step with an executor",
        },
        SystemEdge {
            from: "commerce.invoice.created",
            to: "commerce.invoice.paid",
            kind: "external",
            label: "the counterparty settles the invoice",
        },
        SystemEdge {
            from: "commerce.invoice.created",
            to: "commerce.invoice.past_due",
            kind: "external",
            label: "the invoice goes unpaid past terms",
        },
    ]
}

/// A topic that re-enters the rule set from OUTSIDE it — no handler and
/// no jobs-API mechanic causes it — so the graph draws it as a root, and
/// that root is true. Declared rather than left implicit so
/// `every_shipped_trigger_has_something_upstream` can tell a real root
/// from a missing edge (backlog cf13bd12).
#[derive(Debug, Clone, Serialize)]
pub struct OutsideOrigin {
    pub topic: &'static str,
    /// Who records it, in words.
    pub label: &'static str,
}

pub const OUTSIDE_ORIGINS: &[OutsideOrigin] = &[OutsideOrigin {
    // Each host's observer (infra/estate/observe-lib.sh) POSTs what it
    // found to /api/estate/observation on its own timer; nothing a rule
    // does causes an observation (59ef456a).
    topic: boss_jobs::events::ESTATE_OBSERVED,
    label: "a host's estate observer records what machines it found",
}];

/// Everything this build declares about the handlers it registers, in the
/// one shape core's read surface serves ([`boss_dispatcher::http::HttpState`]).
pub fn cascade() -> Cascade {
    Cascade {
        handler_emits: handler_emits(),
        system_edges: system_edges(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_dispatcher::rules::registry::{TopicPattern, parse_raw_path};
    use std::collections::BTreeSet;
    use std::path::Path;

    /// Either side may be the wildcard, exactly as the page's
    /// `topicMatch(p, trg) || topicMatch(trg, p)` joins them.
    fn topics_meet(a: &str, b: &str) -> bool {
        let one_way = |p: &str, t: &str| TopicPattern::parse(p).is_ok_and(|p| p.matches(t));
        a == b || one_way(a, b) || one_way(b, a)
    }

    /// The event kinds boss-jobs declares, read from the one file that
    /// declares them (`pub const NAME: &str = "..."` in events.rs).
    fn jobs_declared_kinds() -> BTreeSet<String> {
        let path = boss_testing::repo_root().join("crates/core/boss-jobs/src/events.rs");
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let re = regex::Regex::new(r#"(?m)^pub const [A-Z_]+: &str = "([^"]+)";"#).expect("regex");
        let kinds: BTreeSet<String> = re.captures_iter(&src).map(|c| c[1].to_string()).collect();
        assert!(
            kinds.contains("jobs.job.closed"),
            "events.rs parse found {kinds:?}"
        );
        kinds
    }

    /// The per-step-kind topic families the jobs API publishes
    /// (`format!("step.<family>.{}", kind)`), as the wildcard a rule
    /// subscribes with. No constant names them, so the publishing
    /// source is the definition.
    fn jobs_step_families() -> BTreeSet<String> {
        fn walk(dir: &Path, re: &regex::Regex, out: &mut BTreeSet<String>) {
            let entries =
                std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
            for entry in entries {
                let path = entry.expect("a dir entry").path();
                if path.is_dir() {
                    walk(&path, re, out);
                } else if path.extension().is_some_and(|x| x == "rs") {
                    let src = std::fs::read_to_string(&path)
                        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
                    out.extend(re.captures_iter(&src).map(|c| format!("step.{}.*", &c[1])));
                }
            }
        }
        let re = regex::Regex::new(r#"format!\(\s*"step\.([a-z_-]+)\.\{"#).expect("regex");
        let mut out = BTreeSet::new();
        walk(
            &boss_testing::repo_root().join("crates/core/boss-jobs/src"),
            &re,
            &mut out,
        );
        assert!(out.contains("step.done.*"), "boss-jobs scan found {out:?}");
        out
    }

    /// Backlog cf13bd12: /it/registry/dispatcher drew 23 of 46
    /// event-triggered rules with nothing upstream, because
    /// `system_edges` was a hand list that missed `jobs.job.closed` and
    /// `step.assigned.*`, and its only test was `!is_empty()`. Every
    /// topic a shipped rule listens on must now be caused by something
    /// the graph draws — a handler's emit, a system edge — or be named
    /// an outside origin, so a root on the page is a true root.
    #[test]
    fn every_shipped_trigger_has_something_upstream() {
        let path = boss_testing::dispatcher_rules_dir();
        let raw = parse_raw_path(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let produced: Vec<&str> = handler_emits()
            .into_values()
            .flatten()
            .chain(system_edges().into_iter().map(|e| e.to))
            .chain(OUTSIDE_ORIGINS.iter().map(|o| o.topic))
            .collect();
        let orphans: BTreeSet<String> = raw
            .rules
            .iter()
            .filter_map(|r| r.on_event.as_deref().map(|t| (t, r.name.as_str())))
            .filter(|(t, _)| !produced.iter().any(|p| topics_meet(p, t)))
            .map(|(t, name)| format!("{t} (rule {name})"))
            .collect();
        assert!(
            orphans.is_empty(),
            "these triggers have nothing upstream — add the system edge that causes each, \
             or name it in OUTSIDE_ORIGINS: {orphans:#?}"
        );
    }

    /// The step-topic families are the half of the jobs API's output no
    /// constant names; each one it publishes must be the head of a
    /// jobs-api system edge, and every such edge must name one it
    /// publishes — an equality, so a new family reds here by name.
    #[test]
    fn the_step_families_the_jobs_api_publishes_are_the_system_edges_heads() {
        let published = jobs_step_families();
        let drawn: BTreeSet<String> = system_edges()
            .into_iter()
            .filter(|e| e.kind == "jobs-api" && e.to.starts_with("step."))
            .map(|e| e.to.to_string())
            .collect();
        let undrawn: Vec<_> = published.difference(&drawn).collect();
        let stale: Vec<_> = drawn.difference(&published).collect();
        assert!(
            undrawn.is_empty() && stale.is_empty(),
            "published by boss-jobs but no system edge leads to it: {undrawn:?}; \
             a system edge leads to it but boss-jobs never publishes it: {stale:?}"
        );
    }

    /// Every jobs-API topic the cascade names — either end of a
    /// jobs-api edge, a handler's `jobs.*` emit, an outside origin —
    /// is a kind boss-jobs declares, so a renamed or invented topic
    /// cannot draw an edge nothing travels.
    #[test]
    fn every_jobs_topic_the_cascade_names_is_one_boss_jobs_publishes() {
        let declared = jobs_declared_kinds();
        let families = jobs_step_families();
        let is_published = |t: &str| declared.contains(t) || families.contains(t);
        let named: BTreeSet<&str> = system_edges()
            .into_iter()
            .filter(|e| e.kind == "jobs-api")
            .flat_map(|e| [e.from, e.to])
            .chain(
                handler_emits()
                    .into_values()
                    .flatten()
                    .filter(|t| t.starts_with("jobs.")),
            )
            .chain(OUTSIDE_ORIGINS.iter().map(|o| o.topic))
            .collect();
        let unknown: Vec<_> = named.into_iter().filter(|t| !is_published(t)).collect();
        assert!(
            unknown.is_empty(),
            "named in the cascade, published by no boss-jobs site: {unknown:?}"
        );
    }

    #[test]
    fn the_restock_loop_edge_and_a_sink_are_present() {
        let emits = handler_emits();
        // The load-bearing loop: parts.consume → inventory.item.consumed.
        assert!(
            emits["inventory.parts.consume"].contains(&"inventory.item.consumed"),
            "restock loop edge must be present"
        );
        assert!(emits["messages.notify"].is_empty(), "notifier is a sink");
    }

    /// Drift guard: every handler the shipped registry references must
    /// have a `handler_emits` entry, so the cascade graph never silently
    /// drops a handler. Reads the real rule directory (its one
    /// definition, `boss_testing::dispatcher_rules_dir`) so it tracks
    /// the deployed registry, not a fixture.
    #[test]
    fn cascade_handlers_match_rules() {
        let path = boss_testing::dispatcher_rules_dir();
        let raw = parse_raw_path(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let emits = handler_emits();
        for rule in &raw.rules {
            for step in &rule.do_steps {
                assert!(
                    emits.contains_key(step.handler.as_str()),
                    "handler {:?} (rule {:?}) is missing from handler_emits()",
                    step.handler,
                    rule.name
                );
            }
        }
    }
}
