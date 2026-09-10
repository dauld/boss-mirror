//! Postgres-backed coverage for the station registry
//! (116-stations.sql, 118-watchlist-station.sql, 124-repair-station.sql):
//!
//! - the platform seed ships the SDLC batch stations and the filer's
//!   watchlist, active, with predicates the evaluator can actually
//!   parse (a seed row the Rust shape rejects would be a silent dead
//!   station);
//! - every registry write stages its event in `event_outbox` in the
//!   SAME transaction as the stations row (the workflow-registry
//!   posture; the InMemory contract is pinned in `stations::tests`);
//! - every `jobs.station.*` kind is registered in `event_kinds` so the
//!   audit trigger admits it. Deliberately an exact-set assertion
//!   rather than a contains-check: a marker emitted but never
//!   registered is rejected by the trigger at run time, which is the
//!   expensive place to find out.

use boss_core::actor::ActorId;
use boss_jobs::events::{
    STATION_DRAFT_SAVED, STATION_PUBLISHED, STATION_QUARANTINED, STATION_RETIRED,
};
use boss_jobs::registry::WorkflowStatus;
use boss_jobs::station_queue::StationPredicate;
use boss_jobs::{PgStations, StationKind, StationRegistry, StationSpec};
use boss_testing::TestDb;

fn author() -> ActorId {
    ActorId::Human("emp-cto".into())
}

fn spec(name: &str) -> StationSpec {
    StationSpec::draft(
        name,
        format!("Test {name}"),
        StationKind::Batch,
        StationPredicate {
            kind: Some("ship-a-change".into()),
            ..Default::default()
        },
        chrono::Utc::now(),
    )
}

async fn outbox_kinds(db: &TestDb) -> Vec<String> {
    sqlx::query_scalar("SELECT kind FROM event_outbox ORDER BY id")
        .fetch_all(&db.pool)
        .await
        .expect("read outbox kinds")
}

#[tokio::test(flavor = "multi_thread")]
async fn platform_seed_ships_the_sdlc_batch_stations() {
    let db = TestDb::new().await;
    let registry = PgStations::new(db.pool.clone());

    let active = registry.list_active().await.expect("list_active");
    let names: Vec<&str> = active.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["design-review", "loading-dock", "my-watchlist", "repair"],
        "the platform SDLC batch stations seed active, plus the one \
         per-actor row (per-employee stations stay tenant data; \
         `my-watchlist` needs no roster because @me binds at read time)"
    );

    // The repair queue (migration 124, David's bb86d687): red trains,
    // held on a fact that lives on the STEP. Asserting the clause
    // rather than just the name — the row is only useful if it reads
    // the conductor's verdict where the conductor writes it.
    let repair = registry.get_active("repair").await.expect("repair row");
    assert_eq!(repair.predicate.kind.as_deref(), Some("pr-train"));
    let ci = repair.predicate.step.as_ref().expect("ci-step clause");
    assert_eq!(ci.slug.as_deref(), Some("ci"));
    assert_eq!(
        ci.metadata_equals.get("result"),
        Some(&"failing".to_string())
    );
    assert_eq!(
        repair.discipline,
        vec![boss_jobs::station_queue::DisciplineKey::Age],
        "age alone — every train carries the same priority, so ordering \
         by it first would sort by nothing and then by age"
    );

    let dock = registry.get_active("loading-dock").await.expect("dock row");
    assert_eq!(dock.kind, StationKind::Batch);
    assert_eq!(dock.status, WorkflowStatus::Active);
    // The dockRows predicate, ported to data: parked cars only.
    assert_eq!(dock.predicate.kind.as_deref(), Some("ship-a-change"));
    assert_eq!(dock.predicate.metadata_present, vec!["branch".to_string()]);
    assert_eq!(dock.predicate.metadata_absent, vec!["train".to_string()]);
    assert!(dock.predicate.step.is_some(), "review-step clause present");
    assert_eq!(
        dock.discipline,
        boss_jobs::station_queue::default_discipline(),
        "priority, then age"
    );

    let review = registry
        .get_active("design-review")
        .await
        .expect("review row");
    // `design-doc`, not `design-doc-review`: a design doc IS a packet
    // now, so the queue holds the docs themselves rather than a review
    // Job opened per markdown file. The live cluster published this as
    // station v3 through the API and the tree's seed never learned;
    // 202609101900 converged it, because with the corpus panel deleted
    // this predicate is the whole page.
    assert_eq!(review.predicate.kind.as_deref(), Some("design-doc"));
}

/// The seeded upstream pointers survive the round trip through the
/// `stations.upstream` column, and they point at routes that exist
/// (119-station-upstream.sql).
///
/// A seed whose href is a typo is a dead button, and a dead
/// navigational aid is worse than none — it sends the operator
/// somewhere blank at exactly the moment they are diagnosing. The
/// route strings are pinned here; `apps/web/src/shell/nav-catalog.ts`
/// is where they resolve.
#[tokio::test(flavor = "multi_thread")]
async fn the_seeded_upstream_pointers_round_trip() {
    let db = TestDb::new().await;
    let registry = PgStations::new(db.pool.clone());

    // The dock holds parked ship-a-change cars; a car names its
    // motivating user-feedback packet through the `backlog_item` edge,
    // and the triage board is where those packets are worked.
    let dock = registry.get_active("loading-dock").await.expect("dock row");
    let up = dock.upstream.expect("the dock declares its upstream");
    assert_eq!(up.label, "FEEDBACK");
    assert_eq!(up.href, "/system/feedback");

    // Not every station has an upstream, and one that doesn't must
    // read back as "none declared" rather than as an empty pointer.
    //
    // `design-review` is one of them since 2026-09-10. Its pointer read
    // `DESIGN DOCS -> /system/design`, which named the corpus page as
    // where the packets in this queue come FROM — a human clicking a row
    // in a table of markdown files. They come from `boss design` now, a
    // verb rather than a page, and the href had been dead since
    // `/system/*` folded into `/it/*` on 2026-08-31. This test exists to
    // catch a pointer at a route that does not resolve; the honest fix
    // for one was to remove it (202609101900).
    let review = registry
        .get_active("design-review")
        .await
        .expect("review row");
    assert_eq!(review.upstream, None);

    let watchlist = registry
        .get_active("my-watchlist")
        .await
        .expect("watchlist row");
    assert_eq!(watchlist.upstream, None);
}

/// An authored station carries its upstream through the write path
/// too — the seed is data, and so is every row an operator publishes
/// after it.
#[tokio::test(flavor = "multi_thread")]
async fn an_authored_upstream_survives_draft_and_publish() {
    let db = TestDb::new().await;
    let registry = PgStations::new(db.pool.clone());
    let now = chrono::Utc::now();

    let mut authored = spec("night-dock");
    authored.upstream = Some(boss_jobs::StationUpstream {
        label: "FEEDBACK".into(),
        href: "/system/feedback".into(),
    });
    registry
        .create_draft(authored, &author(), now)
        .await
        .expect("draft");
    let published = registry
        .publish("night-dock", &author(), now)
        .await
        .expect("publish");

    let expected = boss_jobs::StationUpstream {
        label: "FEEDBACK".into(),
        href: "/system/feedback".into(),
    };
    assert_eq!(published.upstream.as_ref(), Some(&expected));
    let read_back = registry.get_active("night-dock").await.expect("active");
    assert_eq!(read_back.upstream.as_ref(), Some(&expected));
}

/// The design-review station's page context round-trips through the
/// `stations.lens` column (138-station-lens.sql).
///
/// Pinned here for the same reason the upstream hrefs are: this row is
/// what `/it/design` draws its header and panel set from, so a seed
/// that fails to parse is not a missing button but a page with no
/// name. The panel keys are the renderers `DesignReviewPage` ships —
/// a key nothing renders is a declared panel that never appears, which
/// is exactly what this assertion caught on 2026-09-10: the seed said
/// `["rejections", "corpus"]` and the bundle shipped neither any
/// longer, both panels having gone with the markdown corpus they
/// rendered.
#[tokio::test(flavor = "multi_thread")]
async fn the_design_review_lens_round_trips() {
    let db = TestDb::new().await;
    let registry = PgStations::new(db.pool.clone());

    let review = registry
        .get_active("design-review")
        .await
        .expect("review row");
    let lens = review.lens.expect("the review queue declares a lens");
    assert_eq!(lens.title, "Design review");
    assert_eq!(
        lens.eyebrow.as_deref(),
        Some("System Model · Design review")
    );
    assert_eq!(
        lens.panels,
        vec!["queue".to_string()],
        "one panel, and it renders this station's own packets"
    );

    // A station no page renders reads back as "none declared" rather
    // than as an empty page.
    let dock = registry.get_active("loading-dock").await.expect("dock row");
    assert_eq!(dock.lens, None);
}

/// An authored lens carries through the write path, like `upstream`:
/// the seed is data, and so is every row published after it.
#[tokio::test(flavor = "multi_thread")]
async fn an_authored_lens_survives_draft_and_publish() {
    let db = TestDb::new().await;
    let registry = PgStations::new(db.pool.clone());
    let now = chrono::Utc::now();

    let mut authored = spec("night-review");
    authored.lens = Some(boss_jobs::StationLens {
        eyebrow: None,
        title: "Night review".into(),
        subtitle: Some("What came in after hours".into()),
        panels: vec!["queue".into()],
        with_steps: false,
    });
    let expected = authored.lens.clone();
    registry
        .create_draft(authored, &author(), now)
        .await
        .expect("draft");
    let published = registry
        .publish("night-review", &author(), now)
        .await
        .expect("publish");
    assert_eq!(published.lens, expected);
    let read_back = registry.get_active("night-review").await.expect("active");
    assert_eq!(read_back.lens, expected);
}

/// The seeded watchlist row survives the round trip through the
/// `stations` columns AND evaluates — the seed is only as good as the
/// Rust shape's ability to read it back, and this row is the first to
/// use both new pieces (the `@me` placeholder in the predicate JSONB,
/// the `terminal_window_days` column).
#[tokio::test(flavor = "multi_thread")]
async fn the_watchlist_row_round_trips_and_binds() {
    let db = TestDb::new().await;
    let registry = PgStations::new(db.pool.clone());

    let row = registry
        .get_active("my-watchlist")
        .await
        .expect("watchlist row");
    assert_eq!(row.kind, StationKind::Actor);
    assert_eq!(row.terminal_window_days, Some(14));
    assert_eq!(
        row.discipline,
        vec![boss_jobs::station_queue::DisciplineKey::Recency],
        "newest activity first — a watchlist is read, not pulled from"
    );
    assert!(
        row.predicate.binds_self(),
        "the stored predicate still carries the placeholder; binding \
         happens at the read edge, per request"
    );

    // And it binds: one row, a concrete queue per actor.
    let bound = row.bind_self(Some("emp-r")).expect("an actor binds");
    assert_eq!(
        bound.predicate.metadata_equals.get("submitted_by"),
        Some(&"emp-r".to_string())
    );
    assert_eq!(row.bind_self(None), None, "no actor, no queue");
}

/// The push-down the watchlist reads through — `metadata @> $1` in
/// `list_jobs`. Without it a per-actor station pages through the whole
/// company's newest packets to find one person's, so this is a
/// correctness test, not an optimization one.
#[tokio::test(flavor = "multi_thread")]
async fn metadata_containment_narrows_in_sql() {
    use boss_core::job::{Job, JobStatus, Priority, Subject};
    use boss_jobs::port::{JobFilter, JobsRepository};

    let db = TestDb::new().await;
    let jobs = boss_jobs::PgJobs::new(db.pool.clone());
    let today = chrono::Utc::now().date_naive();

    for who in ["emp-r", "emp-r", "emp-s"] {
        let mut j = Job::new(
            "user-feedback",
            Subject::new("custom", "/ux/jobs"),
            "filed",
            who,
            Priority::Standard,
            today,
        );
        j.status = JobStatus::Open;
        j.metadata = serde_json::json!({ "submitted_by": who });
        jobs.create_job(&j).await.expect("create");
    }

    let filter = JobFilter {
        metadata_contains: Some(serde_json::json!({ "submitted_by": "emp-r" })),
        ..Default::default()
    };
    let (rows, total) = jobs.list_jobs(&filter, 100, 0).await.expect("list");
    assert_eq!(rows.len(), 2);
    assert_eq!(total, 2, "the count query filters identically");
    assert!(rows.iter().all(|j| j.metadata["submitted_by"] == "emp-r"));

    // Absent filter is not a filter — the existing callers are
    // untouched by the new bind.
    let (all, _) = jobs
        .list_jobs(&JobFilter::default(), 100, 0)
        .await
        .expect("list all");
    assert_eq!(all.len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_writes_stage_their_events_in_the_outbox() {
    let db = TestDb::new().await;
    let registry = PgStations::new(db.pool.clone());
    let now = chrono::Utc::now();

    registry
        .create_draft(spec("night-dock"), &author(), now)
        .await
        .expect("draft");
    registry
        .publish("night-dock", &author(), now)
        .await
        .expect("publish");
    registry
        .retire("night-dock", &author(), now)
        .await
        .expect("retire");
    // Idempotent second retire: no row touched, nothing staged.
    registry
        .retire("night-dock", &author(), now)
        .await
        .expect("retire again");

    assert_eq!(
        outbox_kinds(&db).await,
        vec![
            STATION_DRAFT_SAVED.to_string(),
            STATION_PUBLISHED.to_string(),
            STATION_RETIRED.to_string(),
        ],
        "each registry write stages exactly one outbox row, in write order"
    );

    // Versioning against the seeded loading-dock row: a new draft
    // appends the NEXT version and publish demotes the one before it.
    //
    // The numbers are 3 and 2, not 2 and 1, because
    // 133-dock-wip-limit.sql seeds an active v2 carrying the WIP limit.
    // Written as next/previous relative to what the seed leaves active
    // so the next migration that versions this station does not have to
    // come back and edit arithmetic — the only thing this test is about
    // is that a draft appends and a publish demotes.
    let seeded = registry
        .get_active("loading-dock")
        .await
        .expect("seeded active dock");
    let next = seeded.version + 1;
    let draft = registry
        .create_draft(spec("loading-dock"), &author(), now)
        .await
        .expect("draft the next version");
    assert_eq!(draft.version, next);
    registry
        .publish("loading-dock", &author(), now)
        .await
        .expect("publish the next version");
    let previous = registry
        .get_version("loading-dock", seeded.version)
        .await
        .expect("previously active row");
    assert_eq!(previous.status, WorkflowStatus::Retired);
    let active = registry.get_active("loading-dock").await.expect("active");
    assert_eq!(active.version, next);
}

#[tokio::test(flavor = "multi_thread")]
async fn station_event_kinds_are_registered() {
    let db = TestDb::new().await;
    let kinds: Vec<String> = sqlx::query_scalar(
        "SELECT kind_pattern FROM event_kinds WHERE kind_pattern LIKE 'jobs.station.%' ORDER BY kind_pattern",
    )
    .fetch_all(&db.pool)
    .await
    .expect("read event_kinds");
    assert_eq!(
        kinds,
        // Alphabetical, because the query is ORDER BY kind_pattern —
        // `quarantined` sorts between `published` and `retired`.
        //
        // This list is the second home of a fact that lives in the
        // event_kinds INSERTs of migrations 116 and 120 (§9a's equality
        // test), and it did exactly its job: adding
        // `jobs.station.quarantined` in 120 reddened here, which is how
        // an unregistered marker gets caught before it reaches
        // production rather than after.
        vec![
            STATION_DRAFT_SAVED.to_string(),
            STATION_PUBLISHED.to_string(),
            STATION_QUARANTINED.to_string(),
            STATION_RETIRED.to_string(),
        ]
    );
}
