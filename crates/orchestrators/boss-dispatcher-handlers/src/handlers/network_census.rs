//! `network.census` — count what the network is carrying, on a clock.
//!
//! THE INVARIANT THIS MEASURES (packet-loss.md Q1, decided in review
//! `9fb9904f`): every admitted packet reaches a terminal, and every
//! non-terminal packet is visible at >= 1 station. The first half is
//! conservation over TIME, the second over SPACE — and the space half
//! became checkable the day stations became data, because "which
//! queues hold this packet" turned from a convention into a query. A
//! packet matching zero stations is orphaned by definition: nobody's
//! lens will ever show it, so no actor will ever work it.
//!
//! REPORT FIRST, RAISE LATER (Q2). This handler writes the measured
//! series and nothing else: no raiser, no threshold, no catch-all
//! orphan station. We do not yet know the base rate, and a noisy
//! raiser trains people to ignore it — the raiser comes later,
//! calibrated against the numbers this series accumulates. A cadence
//! rule firing daily (Q3) is what turns loss from a spot check into a
//! trend, the same move that turned train timings into the retro's
//! evidence. Destroyed-content detection is explicitly out of scope
//! (Q4) — named here so it is not mistaken for covered.
//!
//! WHAT ONE FIRING DOES. Everything is read through the jobs API — the
//! same public surface any caller gets, no private view:
//!
//! 1. Status totals: `GET /api/jobs?status=<s>&simulated=<b>&limit=1`
//!    per status per partition, reading `total` (a DB-side COUNT, not
//!    a page length). Simulated packets are counted SEPARATELY
//!    throughout — 87% of packets are the demo tenant's, and a
//!    headline number that mixes them measures the demo, not the
//!    company.
//! 2. The workable set: every open job, paged through
//!    `GET /api/jobs?status=open` (the list endpoint returns each
//!    job with its steps). A job with >= 1 ready/active step is
//!    WORKABLE — it is asking for an actor right now.
//! 3. Station coverage: `GET /api/stations`, then each station's
//!    `GET /api/stations/{name}/queue`, unioning member job ids. A
//!    workable job in no queue is ORPHANED — Q1's space half, the
//!    checkable one.
//! 4. One POST to `/api/network/census`, which records the counts as
//!    a single `jobs.network.census` event on the audit log.
//!
//! THE TIME HALF is measured as `closed_on_census_day`: the count of
//! packets that reached a terminal (closed or cancelled) today, by
//! the authoritative clock. The list endpoint has no direct
//! closed-on-day filter, but `closed_within=0` returns exactly
//! "non-terminal OR reached a terminal today" as a DB-side total, so
//! `closed_on_census_day = total(closed_within=0) - sum(non-terminal
//! status totals)` — arithmetic over exact counts, no scan. Clamped
//! at zero because the reads are not one transaction (see LIMITS).
//!
//! HONEST LIMITS, stated rather than implied:
//!
//! - **Read skew.** The census is assembled from many API reads, not
//!   one snapshot; packets that move between reads can shift a count
//!   by a few. Acceptable for a daily trend, and stated here so
//!   nobody mistakes single-digit jitter for loss.
//! - **Station pages clip at the server's MAX_LIMIT (1000).** A
//!   station's queue is evaluated over at most 1000 candidate
//!   packets; with more open packets than that, a true member can be
//!   missed and read as orphaned. `station_page_clipped: true` in
//!   the payload flags exactly this condition (open total across
//!   both partitions > 1000), so a reader can tell a measured zero
//!   from a maybe-clipped one.
//! - **Per-actor stations are not globally evaluable.** A station
//!   whose predicate carries the `@me` placeholder binds to whoever
//!   asks; the census (asking as the dispatcher rule) would only see
//!   its own empty view, so those stations are skipped and counted
//!   in `per_actor_stations_unevaluated`. A packet whose ONLY
//!   visibility is somebody's personal station therefore counts as
//!   orphaned here; `orphaned_with_assignee` (orphans that carry an
//!   assignee on a workable step, and so at least appear in that
//!   person's assignment pull) is reported so the raiser can be
//!   calibrated with that class in view.
//! - **A failed read fails the firing.** Any error aborts the census
//!   with no event: a partial count landing in the series would
//!   poison the trend it exists to build, and the schedule runner
//!   logs the failure loudly. The missing datapoint IS the signal.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use boss_core::partition::Partition;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};

use super::common::{api_client, get_json, post_json};

/// One page more than the server will ever hand back per request.
const PAGE: i64 = 1000;
/// Refuse to scan an absurd universe: at 50 pages the census is no
/// longer a cheap daily read and the design needs revisiting, not a
/// longer loop.
const MAX_PAGES: usize = 50;
/// Cap on orphan ids carried in the payload; the count is always
/// exact, the id list is a sample for a person to pull on.
const ORPHAN_ID_CAP: usize = 20;

/// The non-terminal statuses — a packet in one of these has been
/// admitted and not reached a terminal, so Q1 says it must be visible
/// somewhere. Kebab-case as the API speaks it.
const NON_TERMINAL: [&str; 4] = ["draft", "open", "blocked", "pending-sign-off"];
const TERMINAL: [&str; 2] = ["closed", "cancelled"];

pub struct NetworkCensus {
    client: reqwest::Client,
    jobs_base: String,
}

impl NetworkCensus {
    pub fn new(jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
        })
    }

    pub fn with_client(client: reqwest::Client, jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client,
            jobs_base: jobs_base.into(),
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }

    /// A DB-side count: `total` from a limit=1 page. The row itself is
    /// discarded — the query exists for the COUNT the adapter runs.
    async fn total(&self, query: &str, rule: &str) -> Result<i64, HandlerError> {
        let url = format!("{}/api/jobs?{query}&limit=1", self.base());
        let page = get_json(&self.client, &url, rule).await?;
        page.get("total").and_then(|v| v.as_i64()).ok_or_else(|| {
            HandlerError::Downstream(format!("GET {url}: response carries no numeric `total`"))
        })
    }

    /// The status totals for one partition — real or simulated; the
    /// shadow lane (508cc38c) is in neither tally, since a candidate's
    /// dry run is not the company's work and not the sim's traffic —
    /// in `NON_TERMINAL ++ TERMINAL` order, plus the closed-today
    /// derivation input (`closed_within=0`).
    ///
    /// Spelled as the N-1 `simulated=<bool>` on the wire, deliberately:
    /// on a server that knows the word it means exactly `partition=real`
    /// / `partition=simulated` (http/jobs.rs `partition_from_query`), and
    /// on one that does not it means the same two lanes — whereas an
    /// unknown `partition=` param would be IGNORED there and both
    /// tallies would silently read the unfiltered count. This binary
    /// and the jobs API roll separately, so that window exists.
    async fn partition_totals(
        &self,
        partition: Partition,
        rule: &str,
    ) -> Result<PartitionTotals, HandlerError> {
        let lane = format!("simulated={}", partition == Partition::Simulated);
        let mut by_status = serde_json::Map::new();
        let mut non_terminal_sum = 0i64;
        for status in NON_TERMINAL {
            let n = self.total(&format!("status={status}&{lane}"), rule).await?;
            non_terminal_sum += n;
            by_status.insert(status.to_string(), json!(n));
        }
        for status in TERMINAL {
            let n = self.total(&format!("status={status}&{lane}"), rule).await?;
            by_status.insert(status.to_string(), json!(n));
        }
        let with_today_window = self.total(&format!("closed_within=0&{lane}"), rule).await?;
        Ok(PartitionTotals {
            by_status: serde_json::Value::Object(by_status),
            open_total: non_terminal_sum,
            closed_on_census_day: derive_closed_today(with_today_window, non_terminal_sum),
        })
    }

    /// Every open packet with its steps, paged to completion.
    async fn open_packets(&self, rule: &str) -> Result<Vec<PacketView>, HandlerError> {
        let mut packets: Vec<PacketView> = Vec::new();
        let mut offset = 0i64;
        for _ in 0..MAX_PAGES {
            let url = format!(
                "{}/api/jobs?status=open&limit={PAGE}&offset={offset}",
                self.base()
            );
            let page = get_json(&self.client, &url, rule).await?;
            let rows = page
                .get("data")
                .and_then(|v| v.as_array())
                .map(Vec::len)
                .unwrap_or(0);
            packets.extend(packet_views(&page));
            offset += rows as i64;
            let total = page.get("total").and_then(|v| v.as_i64()).unwrap_or(0);
            if rows == 0 || offset >= total {
                return Ok(packets);
            }
        }
        Err(HandlerError::Downstream(format!(
            "census refuses to scan more than {} open packets — this is no longer a cheap \
             daily read; revisit the design",
            PAGE as usize * MAX_PAGES
        )))
    }
}

/// One partition's headline totals.
struct PartitionTotals {
    by_status: serde_json::Value,
    /// Non-terminal count — draft + open + blocked + pending-sign-off.
    open_total: i64,
    closed_on_census_day: i64,
}

/// The census's view of one open packet, extracted from a jobs-list
/// page row: identity, which partition it belongs to, and whether it
/// is asking for an actor right now.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PacketView {
    pub id: String,
    /// Read through the shared wire reader (`partition`, else the
    /// legacy `simulated` bool, else real), so a shadow packet is
    /// neither a real one nor the sim's here (508cc38c).
    pub partition: Partition,
    /// >= 1 step in ready/active — the packet wants an actor.
    pub workable: bool,
    /// A workable step carries an assignee — the packet is at least
    /// visible in that person's assignment pull, even if no global
    /// station holds it.
    pub assigned: bool,
}

/// Parse one `GET /api/jobs` page into [`PacketView`]s. Rows without
/// an id are dropped rather than invented; a missing `steps` array
/// reads as "no workable step", which errs toward orphaned — the
/// loud direction, and the read failing outright is already a hard
/// error upstream.
pub(crate) fn packet_views(page: &serde_json::Value) -> Vec<PacketView> {
    page.get("data")
        .and_then(|v| v.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|job| {
                    let id = job.get("id")?.as_str()?.to_string();
                    let partition = job
                        .get("partition")
                        .and_then(|v| v.as_str())
                        .and_then(|w| w.parse::<Partition>().ok())
                        .unwrap_or_else(|| {
                            Partition::from_legacy_simulated(
                                job.get("simulated").and_then(|v| v.as_bool()) == Some(true),
                            )
                        });
                    let empty = Vec::new();
                    let steps = job
                        .get("steps")
                        .and_then(|v| v.as_array())
                        .unwrap_or(&empty);
                    let workable_step = |s: &&serde_json::Value| {
                        matches!(
                            s.get("status").and_then(|v| v.as_str()),
                            Some("ready") | Some("active")
                        )
                    };
                    let workable = steps.iter().any(|s| workable_step(&s));
                    let assigned = steps.iter().filter(workable_step).any(|s| {
                        s.get("assignee_id")
                            .and_then(|v| v.as_str())
                            .is_some_and(|a| !a.is_empty())
                    });
                    Some(PacketView {
                        id,
                        partition,
                        workable,
                        assigned,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The member job ids of one station-queue envelope (`data: [Job]`).
/// An unrecognised body claims no members — safe here because a
/// failed queue READ is a hard error upstream; this only shapes a
/// 2xx body.
pub(crate) fn queue_member_ids(queue: &serde_json::Value) -> Vec<String> {
    queue
        .get("data")
        .and_then(|v| v.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|j| j.get("id").and_then(|v| v.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Q1's space half, computed: workable packets vs the union of every
/// station queue. Real and simulated are tallied separately, and a
/// shadow packet (508cc38c) is in NEITHER tally — it fails closed out
/// of the real headline exactly as a simulated one does, and the sim's
/// tally is the sim's alone (Q5); the orphan id sample carries REAL
/// packets only (a demo orphan is a count, not a work item for a
/// person).
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct SpaceHalf {
    pub workable_total: i64,
    pub stationed_count: i64,
    pub orphaned_count: i64,
    pub orphaned_with_assignee: i64,
    pub orphaned_job_ids: Vec<String>,
    pub orphans_truncated: bool,
    pub sim_workable_total: i64,
    pub sim_stationed_count: i64,
    pub sim_orphaned_count: i64,
}

pub(crate) fn space_half(
    packets: &[PacketView],
    stationed: &HashSet<String>,
    id_cap: usize,
) -> SpaceHalf {
    let mut out = SpaceHalf::default();
    for p in packets {
        if !p.workable {
            continue;
        }
        let is_stationed = stationed.contains(&p.id);
        match p.partition {
            Partition::Simulated => {
                out.sim_workable_total += 1;
                if is_stationed {
                    out.sim_stationed_count += 1;
                } else {
                    out.sim_orphaned_count += 1;
                }
                continue;
            }
            Partition::Shadow => continue,
            Partition::Real => {}
        }
        out.workable_total += 1;
        if is_stationed {
            out.stationed_count += 1;
            continue;
        }
        out.orphaned_count += 1;
        if p.assigned {
            out.orphaned_with_assignee += 1;
        }
        if out.orphaned_job_ids.len() < id_cap {
            out.orphaned_job_ids.push(p.id.clone());
        } else {
            out.orphans_truncated = true;
        }
    }
    out
}

/// `closed_within=0` returns "non-terminal OR reached a terminal
/// today" as one exact count; subtracting the non-terminal sum leaves
/// today's terminals. Clamped at zero: the two counts come from
/// separate reads, and a packet closing between them must read as
/// jitter, not as a negative day.
pub(crate) fn derive_closed_today(with_today_window: i64, non_terminal_sum: i64) -> i64 {
    (with_today_window - non_terminal_sum).max(0)
}

/// Whether a station row can be evaluated globally: a predicate
/// carrying the `@me` placeholder binds per-caller and cannot be —
/// the census would only see its own empty view of it.
pub(crate) fn is_per_actor(station_row: &serde_json::Value) -> bool {
    station_row
        .get("predicate")
        .map(|p| p.to_string().contains("\"@me\""))
        .unwrap_or(false)
}

#[async_trait]
impl Handler for NetworkCensus {
    fn name(&self) -> &'static str {
        "network.census"
    }

    async fn invoke(
        &self,
        _args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let rule = &ctx.rule_name;
        // The schedule runner's synthetic clock-day payload. Absent on
        // a hand-fired invocation; the event's own timestamp remains
        // the authoritative axis either way.
        let census_day = ctx.event_payload.get("_day").and_then(|v| v.as_str());

        // 1. Status totals, per partition.
        let real = self.partition_totals(Partition::Real, rule).await?;
        let sim = self.partition_totals(Partition::Simulated, rule).await?;

        // 2. The workable set: every open packet with its steps.
        let packets = self.open_packets(rule).await?;

        // 3. Station coverage.
        let stations =
            get_json(&self.client, &format!("{}/api/stations", self.base()), rule).await?;
        // A station list with no `data` array is NO ANSWER. Read as zero
        // stations it recorded a census that evaluated nothing as a
        // measured datapoint — the partial count LIMITS says must fail
        // the firing instead (backlog 37fc5837).
        let rows: Vec<serde_json::Value> =
            super::common::rows_or_refuse(&stations, "GET /api/stations")
                .map_err(HandlerError::Downstream)?;
        let mut per_actor_skipped = 0i64;
        let mut evaluated = 0i64;
        let mut stationed: HashSet<String> = HashSet::new();
        for row in &rows {
            let Some(name) = row.get("name").and_then(|v| v.as_str()) else {
                continue;
            };
            if is_per_actor(row) {
                per_actor_skipped += 1;
                continue;
            }
            let queue = get_json(
                &self.client,
                &format!("{}/api/stations/{name}/queue", self.base()),
                rule,
            )
            .await?;
            stationed.extend(queue_member_ids(&queue));
            evaluated += 1;
        }

        // 4. Compute and land the counts as one event.
        let space = space_half(&packets, &stationed, ORPHAN_ID_CAP);
        let open_all = real.by_status["open"].as_i64().unwrap_or(0)
            + sim.by_status["open"].as_i64().unwrap_or(0);
        let payload = json!({
            "census_day": census_day,
            // Headline numbers are the REAL partition throughout.
            "open_total": real.open_total,
            "by_status": real.by_status,
            "workable_total": space.workable_total,
            "stationed_count": space.stationed_count,
            "orphaned_count": space.orphaned_count,
            "orphaned_with_assignee": space.orphaned_with_assignee,
            "orphaned_job_ids": space.orphaned_job_ids,
            "orphans_truncated": space.orphans_truncated,
            "closed_on_census_day": real.closed_on_census_day,
            "time_half": "measured",
            // The demo tenant, separately — never mixed in.
            "sim_open_total": sim.open_total,
            "sim_by_status": sim.by_status,
            "sim_workable_total": space.sim_workable_total,
            "sim_stationed_count": space.sim_stationed_count,
            "sim_orphaned_count": space.sim_orphaned_count,
            "sim_closed_on_census_day": sim.closed_on_census_day,
            // Instrument honesty (see module doc LIMITS).
            "stations_evaluated": evaluated,
            "per_actor_stations_unevaluated": per_actor_skipped,
            "station_page_clipped": open_all > PAGE,
        });
        post_json(
            &self.client,
            &format!("{}/api/network/census", self.base()),
            &payload,
            rule,
        )
        .await?;
        tracing::info!(
            rule = %rule,
            open_total = real.open_total,
            workable = space.workable_total,
            orphaned = space.orphaned_count,
            sim_orphaned = space.sim_orphaned_count,
            "network census recorded"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(jobs: serde_json::Value) -> serde_json::Value {
        let total = jobs.as_array().map(Vec::len).unwrap_or(0);
        json!({"data": jobs, "total": total})
    }

    #[test]
    fn a_ready_or_active_step_makes_a_packet_workable() {
        let views = packet_views(&page(json!([
            {"id": "j1", "simulated": false, "steps": [{"status": "ready"}]},
            {"id": "j2", "simulated": false, "steps": [{"status": "active"}]},
            {"id": "j3", "simulated": false, "steps": [{"status": "pending"}, {"status": "completed"}]},
            {"id": "j4", "simulated": false, "steps": []},
        ])));
        let workable: Vec<&str> = views
            .iter()
            .filter(|p| p.workable)
            .map(|p| p.id.as_str())
            .collect();
        assert_eq!(workable, vec!["j1", "j2"]);
    }

    #[test]
    fn assignment_is_read_off_workable_steps_only() {
        let views = packet_views(&page(json!([
            // The assignee sits on a COMPLETED step: the packet is
            // workable via the ready step, but nobody's assignment
            // pull will surface it.
            {"id": "j1", "steps": [
                {"status": "completed", "assignee_id": "emp-1"},
                {"status": "ready"},
            ]},
            {"id": "j2", "steps": [{"status": "ready", "assignee_id": "emp-2"}]},
            {"id": "j3", "steps": [{"status": "ready", "assignee_id": ""}]},
        ])));
        assert!(!views[0].assigned);
        assert!(views[1].assigned);
        assert!(!views[2].assigned, "an empty assignee is no assignee");
    }

    #[test]
    fn partition_defaults_real_for_pre_flag_rows_and_reads_the_word_first() {
        let views = packet_views(&page(json!([
            {"id": "j1", "steps": []},
            {"id": "j2", "simulated": true, "steps": []},
            {"id": "j3", "partition": "shadow", "simulated": true, "steps": []},
        ])));
        assert_eq!(views[0].partition, Partition::Real);
        assert_eq!(
            views[1].partition,
            Partition::Simulated,
            "an N-1 row: the bool"
        );
        assert_eq!(
            views[2].partition,
            Partition::Shadow,
            "the word wins over the derived bool"
        );
    }

    #[test]
    fn queue_member_ids_come_from_the_envelope_data() {
        let ids = queue_member_ids(&json!({
            "station": "q.brewer.task", "total": 2,
            "data": [{"id": "j1"}, {"id": "j2"}],
        }));
        assert_eq!(ids, vec!["j1", "j2"]);
        assert!(queue_member_ids(&json!({"error": "nope"})).is_empty());
    }

    fn pv(id: &str, partition: Partition, workable: bool, assigned: bool) -> PacketView {
        PacketView {
            id: id.into(),
            partition,
            workable,
            assigned,
        }
    }

    #[test]
    fn stationed_and_orphaned_partition_the_workable_set() {
        let packets = vec![
            pv("j1", Partition::Real, true, false),  // stationed
            pv("j2", Partition::Real, true, true),   // orphaned, assigned
            pv("j3", Partition::Real, true, false),  // orphaned
            pv("j4", Partition::Real, false, false), // not workable: not counted
        ];
        let stationed: HashSet<String> = ["j1".to_string()].into();
        let s = space_half(&packets, &stationed, ORPHAN_ID_CAP);
        assert_eq!(s.workable_total, 3);
        assert_eq!(s.stationed_count, 1);
        assert_eq!(s.orphaned_count, 2);
        assert_eq!(s.orphaned_with_assignee, 1);
        assert_eq!(s.orphaned_job_ids, vec!["j2", "j3"]);
        assert!(!s.orphans_truncated);
    }

    #[test]
    fn simulated_packets_never_touch_the_headline_numbers() {
        let packets = vec![
            pv("j1", Partition::Real, true, false),
            pv("s1", Partition::Simulated, true, false), // sim, stationed
            pv("s2", Partition::Simulated, true, false), // sim, orphaned
            // A shadow packet (508cc38c): in neither tally — excluded
            // from the headline as a simulated one is, and not the
            // sim's either.
            pv("x1", Partition::Shadow, true, false),
        ];
        let stationed: HashSet<String> = ["s1".to_string()].into();
        let s = space_half(&packets, &stationed, ORPHAN_ID_CAP);
        assert_eq!(s.workable_total, 1);
        assert_eq!(s.orphaned_count, 1);
        assert_eq!(s.sim_workable_total, 2);
        assert_eq!(s.sim_stationed_count, 1);
        assert_eq!(s.sim_orphaned_count, 1);
        assert_eq!(
            s.orphaned_job_ids,
            vec!["j1"],
            "the id sample is real packets only — a demo orphan is a count, not a work item"
        );
    }

    #[test]
    fn the_orphan_id_sample_caps_and_says_so_while_the_count_stays_exact() {
        let packets: Vec<PacketView> = (0..25)
            .map(|i| pv(&format!("j{i}"), Partition::Real, true, false))
            .collect();
        let s = space_half(&packets, &HashSet::new(), ORPHAN_ID_CAP);
        assert_eq!(s.orphaned_count, 25);
        assert_eq!(s.orphaned_job_ids.len(), ORPHAN_ID_CAP);
        assert!(s.orphans_truncated);
    }

    #[test]
    fn closed_today_is_the_window_minus_the_open_set_clamped_at_zero() {
        assert_eq!(derive_closed_today(45, 40), 5);
        // A packet closed between the two reads: jitter, not a
        // negative day.
        assert_eq!(derive_closed_today(39, 40), 0);
    }

    #[test]
    fn a_self_placeholder_marks_a_station_per_actor() {
        assert!(is_per_actor(&json!({
            "name": "my-watchlist",
            "predicate": {"metadata_equals": {"submitted_by": "@me"}},
        })));
        assert!(!is_per_actor(&json!({
            "name": "q.brewer.task",
            "predicate": {"kind": "brew-batch"},
        })));
    }

    /// A station list with no `data` array is no answer (backlog
    /// 37fc5837). Read as zero stations, the census recorded a day on
    /// which nothing was evaluated as a measured datapoint — the
    /// partial count this handler's own LIMITS refuse to land. It now
    /// fails the firing, and no census event is written.
    #[tokio::test]
    async fn a_station_list_with_no_data_array_refuses_and_records_nothing() {
        use crate::handlers::listing_stub::{
            assert_refused_by_name, empty_listing, no_data_array, serve,
        };
        let stub = serve(vec![
            ("/api/jobs", empty_listing()),
            ("/api/stations", no_data_array()),
        ])
        .await;
        let ctx = InvocationContext {
            rule_name: "network-census-daily".into(),
            triggering_event_id: "clock-2026-09-23".into(),
            triggering_topic: "schedule".into(),
            event_payload: json!({ "_day": "2026-09-23" }),
        };
        let res = NetworkCensus::new(stub.base.clone())
            .invoke(&[], &ctx)
            .await;
        assert_eq!(stub.writes(), Vec::<String>::new(), "no census recorded");
        assert_refused_by_name(res, "GET /api/stations");
    }
}
