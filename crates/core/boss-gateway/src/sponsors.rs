//! The sponsor roll — `GET /site/sponsors.json` on the tenant site
//! host: a projection of closed `receive-a-sponsorship` packets to
//! the names their sponsors consented to, by month, and a count of
//! the rest (design "The sponsor roll", David 2026-09-17; backlog
//! f200f3d2).
//!
//! WHAT IS LISTED. A closed sponsorship packet whose `decide` step
//! completed with `public_thanks = yes` AND whose packet metadata
//! carries a non-blank `sponsor_roll_name` (the Checkout custom field
//! the sponsor typed — the consent rides the payment, see
//! boss-dispatcher-handlers' stripe_charges) → `{name, month}`, the
//! month being the packet's `opened_on`. Every other closed
//! sponsorship packet is one of the `anonymous_count`. Ordered by
//! month, then name — a roll, not a ledger.
//!
//! WHAT IS NEVER IN THE DOCUMENT. The packets carry `amount_cents`,
//! `currency`, `customer_email`, `customer_name`, `stripe_charge_id`,
//! `stripe_customer`, ids and step metadata. None of that leaves this
//! module: the output type has exactly the fields `name` and `month`,
//! so a field the packet grows tomorrow cannot leak by default. The
//! test below asserts the serialized text against a fixture carrying
//! every one of those fields. The site host is behind Access today;
//! the endpoint is public by design once Access comes down, because
//! it exposes only what sponsors consented to.
//!
//! HOW IT READS. Through the jobs API, paged, with the gateway's own
//! service identity (`automation:gateway`, the actor passkey.rs signs
//! its own reads as) — never a visitor's session, of which the site
//! has none. The computed document is cached for [`TTL`]; one fetch
//! at a time (the cache lock is held across it), so a burst of site
//! visitors is one upstream read.
//!
//! WHEN THE JOBS API FAILS. No cached copy → 503 with a plain
//! sentence: an empty roll would read as "no sponsors", which is a
//! confident wrong answer. A copy older than the TTL → it is served,
//! but marked: `Cache-Control: no-store` so no edge holds it, and
//! `X-Roll-Stale: <age in seconds>` so a reader can tell a served
//! stale copy from a fresh one. Neither is a stale copy pretending
//! to be current.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::Value;

/// The path on the site host. Named once, here; site.rs routes it.
pub const PATH: &str = "/site/sponsors.json";

/// How long a computed document is current.
pub const TTL: Duration = Duration::from_secs(60);

/// The workflow kind the roll projects.
pub const KIND: &str = "receive-a-sponsorship";

/// The step whose completion carries the public-thanks decision, and
/// the field it decides.
const DECIDE_STEP: &str = "decide";
const PUBLIC_THANKS: &str = "public_thanks";

/// The packet metadata key the consented name rides under — the same
/// spelling the Stripe reading carries it in.
const ROLL_NAME: &str = "sponsor_roll_name";

/// Page size for the jobs read, and a ceiling on pages so a runaway
/// `total` cannot turn one site request into an unbounded walk.
const PAGE: usize = 200;
const MAX_PAGES: usize = 50;

/// One page of closed sponsorship packets as the jobs API lists them:
/// the rows (each with its `steps` inline) and the filter-wide total.
#[derive(Debug, Clone, Default)]
pub struct Page {
    pub data: Vec<Value>,
    pub total: usize,
}

/// The port: closed sponsorship packets, one page at a time.
#[async_trait::async_trait]
pub trait ClosedSponsorships: Send + Sync {
    async fn page(&self, offset: usize, limit: usize) -> Result<Page, String>;
}

/// One listed sponsor. Exactly these two fields — the shape is the
/// privacy boundary.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Entry {
    pub month: String,
    pub name: String,
}

/// The document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Roll {
    pub sponsors: Vec<Entry>,
    pub anonymous_count: usize,
}

/// Pure: the roll a set of closed sponsorship packets projects to.
/// Listed entries sorted by month then name; everything else counted.
pub fn project(jobs: &[Value]) -> Roll {
    let mut sponsors: Vec<Entry> = jobs.iter().filter_map(listed).collect();
    sponsors.sort();
    Roll {
        anonymous_count: jobs.len() - sponsors.len(),
        sponsors,
    }
}

/// The entry a packet lists as, or `None` when it is anonymous: the
/// decide step must be COMPLETED (an active step's metadata is not a
/// decision) with `public_thanks = yes`, and the consented name must
/// be present and non-blank.
fn listed(job: &Value) -> Option<Entry> {
    let decide = job
        .get("steps")?
        .as_array()?
        .iter()
        .find(|s| step_slug(s) == Some(DECIDE_STEP))?;
    if decide.get("status").and_then(Value::as_str) != Some("completed") {
        return None;
    }
    let said_yes = decide
        .pointer(&format!("/metadata/{PUBLIC_THANKS}"))
        .and_then(Value::as_str)
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("yes"));
    if !said_yes {
        return None;
    }
    let name = job
        .pointer(&format!("/metadata/{ROLL_NAME}"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|n| !n.is_empty())?;
    // `opened_on` is a NaiveDate, `YYYY-MM-DD`; the month is its
    // first seven characters. A packet without one is not listable —
    // a name without a month has nowhere on the roll to go.
    let month = job
        .get("opened_on")
        .and_then(Value::as_str)
        .and_then(|d| d.get(..7))
        .map(str::to_string)?;
    Some(Entry {
        month,
        name: name.to_string(),
    })
}

/// A step's stable name: `spec_slug`, else its title — the same
/// fallback the step API renders (steps.rs).
fn step_slug(step: &Value) -> Option<&str> {
    step.get("spec_slug")
        .and_then(Value::as_str)
        .or_else(|| step.get("title").and_then(Value::as_str))
}

/// Every closed sponsorship packet, walked page by page until the
/// listing's own `total` is reached (or a page comes back short, or
/// the page ceiling — whichever first).
pub async fn fetch_all(reader: &dyn ClosedSponsorships) -> Result<Vec<Value>, String> {
    let mut all: Vec<Value> = Vec::new();
    for _ in 0..MAX_PAGES {
        let page = reader.page(all.len(), PAGE).await?;
        let got = page.data.len();
        all.extend(page.data);
        if got < PAGE || all.len() >= page.total {
            break;
        }
    }
    Ok(all)
}

/// The jobs API as the port: `GET /api/jobs?kind=…&status=closed`,
/// signed as the gateway's own actor.
pub struct JobsApi {
    http: reqwest::Client,
    base: String,
}

impl JobsApi {
    /// `BOSS_JOBS_UPSTREAM`, or the port registry's jobs URL — the
    /// same resolution passkey.rs uses for its own jobs reads.
    pub fn from_env() -> Self {
        Self::new(std::env::var("BOSS_JOBS_UPSTREAM").unwrap_or_else(|_| boss_ports::url("jobs")))
    }

    pub fn new(base: String) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
            base,
        }
    }
}

#[async_trait::async_trait]
impl ClosedSponsorships for JobsApi {
    /// The listing the jobs API answers: `{data: [job + steps],
    /// total, limit, offset}` (boss-jobs http/jobs.rs `list_jobs`).
    /// Signed as the gateway's own actor — the identity passkey.rs
    /// uses for its server-side reads — plus the machine token when
    /// the process has one, so the read is admitted once the door
    /// requires it on reads too.
    async fn page(&self, offset: usize, limit: usize) -> Result<Page, String> {
        let url = format!(
            "{}/api/jobs?kind={KIND}&status=closed&limit={limit}&offset={offset}",
            self.base
        );
        let mut rb = self.http.get(&url).header(
            "x-boss-user",
            serde_json::json!({
                "id": "automation:gateway",
                "role": "platform-admin",
                "access_tier": "operator",
            })
            .to_string(),
        );
        if let Some(token) = boss_core::machine_token::from_env() {
            rb = rb.header(boss_core::machine_token::HEADER, token);
        }
        let resp = rb
            .send()
            .await
            .map_err(|e| format!("jobs unreachable: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("jobs answered {}", resp.status()));
        }
        let body: Value = resp
            .json()
            .await
            .map_err(|e| format!("jobs listing malformed: {e}"))?;
        let data = body
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| "jobs listing has no data array".to_string())?;
        let total = body
            .get("total")
            .and_then(Value::as_u64)
            .ok_or_else(|| "jobs listing has no total".to_string())?;
        Ok(Page {
            data,
            total: usize::try_from(total).unwrap_or(usize::MAX),
        })
    }
}

/// The cached document and the reader behind it.
pub struct SponsorRoll {
    reader: Arc<dyn ClosedSponsorships>,
    cache: tokio::sync::Mutex<Option<(Instant, String)>>,
}

impl std::fmt::Debug for SponsorRoll {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SponsorRoll")
    }
}

impl SponsorRoll {
    pub fn new(reader: Arc<dyn ClosedSponsorships>) -> Self {
        Self {
            reader,
            cache: tokio::sync::Mutex::new(None),
        }
    }

    /// The response for `GET /site/sponsors.json`: the cached
    /// document while it is current; else a fresh one; else — the
    /// upstream failing — the stale copy marked stale, or a 503.
    pub async fn respond(&self) -> Response {
        // The lock is held across the fetch on purpose: concurrent
        // misses wait for one read rather than each making their own.
        let mut cache = self.cache.lock().await;
        if let Some((at, doc)) = cache.as_ref()
            && at.elapsed() < TTL
        {
            return fresh(doc);
        }
        match fetch_all(self.reader.as_ref()).await {
            Ok(jobs) => {
                let doc = match serde_json::to_string(&project(&jobs)) {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::warn!(error = %e, "sponsor roll: document did not serialize");
                        return unavailable();
                    }
                };
                *cache = Some((Instant::now(), doc.clone()));
                fresh(&doc)
            }
            Err(why) => {
                tracing::warn!(why, "sponsor roll: jobs read failed");
                match cache.as_ref() {
                    Some((at, doc)) => stale(doc, at.elapsed()),
                    None => unavailable(),
                }
            }
        }
    }
}

fn json_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers
}

/// A current document: cacheable at the edge for the same TTL.
fn fresh(doc: &str) -> Response {
    let mut headers = json_headers();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=60"),
    );
    (StatusCode::OK, headers, doc.to_string()).into_response()
}

/// The last good document while the upstream is down: served, not
/// cached beyond this response, and marked with its age in seconds.
fn stale(doc: &str, age: Duration) -> Response {
    let mut headers = json_headers();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Ok(v) = HeaderValue::from_str(&age.as_secs().to_string()) {
        headers.insert("x-roll-stale", v);
    }
    (StatusCode::OK, headers, doc.to_string()).into_response()
}

/// Nothing to serve: a sentence, never an empty roll.
fn unavailable() -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    (
        StatusCode::SERVICE_UNAVAILABLE,
        headers,
        "The sponsor roll is not available right now.\n",
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    //! Through the gateway's own router with a site mounted, the
    //! jobs API stubbed in memory: every answer below is one the
    //! mounted router gave for the site host.

    use super::*;
    use crate::AppState;
    use crate::perf::PerfCollector;
    use crate::site::{self, Site};
    use axum::body::Body;
    use axum::extract::Request;
    use http_body_util::BodyExt;
    use serde_json::json;
    use std::sync::Mutex;
    use tower::ServiceExt;

    const HOST: &str = "www.site.test";

    /// The in-memory jobs API: the closed sponsorship packets it
    /// holds, served in pages; or a failure, when told to fail. Counts
    /// its page reads so a cache hit is observable.
    struct Stub {
        jobs: Vec<Value>,
        fail: Mutex<bool>,
        reads: Mutex<usize>,
    }

    impl Stub {
        fn holding(jobs: Vec<Value>) -> Arc<Self> {
            Arc::new(Self {
                jobs,
                fail: Mutex::new(false),
                reads: Mutex::new(0),
            })
        }
        fn set_failing(&self, yes: bool) {
            *self.fail.lock().unwrap() = yes;
        }
        fn reads(&self) -> usize {
            *self.reads.lock().unwrap()
        }
    }

    #[async_trait::async_trait]
    impl ClosedSponsorships for Stub {
        async fn page(&self, offset: usize, limit: usize) -> Result<Page, String> {
            *self.reads.lock().unwrap() += 1;
            if *self.fail.lock().unwrap() {
                return Err("connection refused".into());
            }
            Ok(Page {
                data: self.jobs.iter().skip(offset).take(limit).cloned().collect(),
                total: self.jobs.len(),
            })
        }
    }

    /// A closed sponsorship packet as the jobs API lists it, carrying
    /// EVERY field the projection must not repeat: amounts, currency,
    /// the customer's email and name, Stripe ids, packet and step
    /// ids. `decided` is the decide step's `public_thanks`; `roll`
    /// the packet's `sponsor_roll_name` (absent when `None`).
    fn packet(id: &str, opened_on: &str, decided: &str, roll: Option<&str>) -> Value {
        let mut metadata = json!({
            "amount_cents": "2500",
            "currency": "usd",
            "customer_email": format!("{id}@sponsor.example"),
            "customer_name": format!("Customer {id}"),
            "stripe_charge_id": format!("ch_{id}"),
            "stripe_customer": format!("cus_{id}"),
            "receipt_url": format!("https://pay.stripe.com/receipts/{id}"),
        });
        if let Some(r) = roll {
            metadata[ROLL_NAME] = json!(r);
        }
        json!({
            "id": format!("job-{id}"),
            "kind": KIND,
            "status": "closed",
            "title": format!("Stripe reported a payment — acct-{id}"),
            "owner_id": "emp-david",
            "subject": {"id": format!("acct-{id}"), "subject_kind": "account"},
            "opened_on": opened_on,
            "closed_on": opened_on,
            "metadata": metadata,
            "steps": [
                {"id": format!("step-{id}-1"), "spec_slug": "reconcile", "title": "reconcile",
                 "status": "completed",
                 "metadata": {"amount_cents": "2500", "stripe_event_id": format!("evt_{id}"),
                              "sponsor_account_id": format!("acct-{id}"), "new_sponsor": "yes"}},
                {"id": format!("step-{id}-2"), "spec_slug": "decide", "title": "decide",
                 "status": "completed",
                 "metadata": {PUBLIC_THANKS: decided}},
                {"id": format!("step-{id}-3"), "spec_slug": "thank", "title": "thank",
                 "status": "completed", "metadata": {"message_id": format!("msg-{id}")}},
            ],
        })
    }

    fn app(stub: Arc<Stub>) -> axum::Router {
        let state = Arc::new(AppState {
            session_key: vec![0u8; 32],
            proxy_client: reqwest::Client::new(),
            perf: Arc::new(PerfCollector::new()),
        });
        let root = boss_testing::scratch_dir("gateway-sponsors");
        boss_testing::create_dir(&root);
        let site = Site::from_values(HOST, root.to_str().unwrap())
            .map(|s| s.with_sponsors(SponsorRoll::new(stub)));
        site::mount(crate::build_router(None).with_state(state), site)
    }

    async fn get(app: &axum::Router, host: &str) -> (StatusCode, HeaderMap, String) {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(PATH)
                    .header(header::HOST, host)
                    .header(header::COOKIE, "boss_session=forged")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("router responds");
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = resp.into_body().collect().await.expect("body").to_bytes();
        (
            status,
            headers,
            String::from_utf8_lossy(&bytes).into_owned(),
        )
    }

    fn header<'a>(h: &'a HeaderMap, name: &str) -> &'a str {
        h.get(name).and_then(|v| v.to_str().ok()).unwrap_or("")
    }

    /// The fixture: three listed (out of month and name order), one
    /// who said no, one who said yes but typed no name, one whose
    /// name is blank.
    fn fixture() -> Vec<Value> {
        vec![
            packet("b", "2026-10-03", "yes", Some("Zed Corp")),
            packet("a", "2026-09-17", "yes", Some("Ada Lovelace")),
            packet("c", "2026-10-01", "yes", Some("  Bob & Co  ")),
            packet("d", "2026-09-20", "no", Some("Not Listed")),
            packet("e", "2026-09-21", "yes", None),
            packet("f", "2026-09-22", "yes", Some("   ")),
        ]
    }

    #[tokio::test]
    async fn listed_names_by_month_then_name_and_the_rest_counted() {
        let (status, h, body) = get(&app(Stub::holding(fixture())), HOST).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(header(&h, "content-type"), "application/json");
        assert_eq!(header(&h, "cache-control"), "public, max-age=60");
        assert!(h.get("x-roll-stale").is_none(), "a fresh roll is not stale");
        let doc: Value = serde_json::from_str(&body).expect("json");
        assert_eq!(
            doc,
            json!({
                "sponsors": [
                    {"name": "Ada Lovelace", "month": "2026-09"},
                    {"name": "Bob & Co", "month": "2026-10"},
                    {"name": "Zed Corp", "month": "2026-10"},
                ],
                "anonymous_count": 3,
            })
        );
    }

    #[test]
    fn the_projection_reads_only_a_completed_decide_step_that_said_yes() {
        // A decide step that is not completed does not list, however
        // its metadata reads; a name on a packet with no decide step
        // does not list either.
        let mut open_decide = packet("g", "2026-09-01", "yes", Some("Gus"));
        open_decide["steps"][1]["status"] = json!("active");
        let mut no_decide = packet("h", "2026-09-01", "yes", Some("Hal"));
        no_decide["steps"] = json!([]);
        // Only the spelling the registry admits: `yes`, any case,
        // trimmed. A step named by title alone (no spec_slug) counts.
        let mut by_title = packet("i", "2026-09-01", " YES ", Some("Ivy"));
        by_title["steps"][1]
            .as_object_mut()
            .unwrap()
            .remove("spec_slug");
        let roll = project(&[open_decide, no_decide, by_title]);
        assert_eq!(
            roll,
            Roll {
                sponsors: vec![Entry {
                    month: "2026-09".into(),
                    name: "Ivy".into()
                }],
                anonymous_count: 2,
            }
        );
        assert_eq!(
            project(&[]),
            Roll {
                sponsors: vec![],
                anonymous_count: 0
            }
        );
    }

    #[tokio::test]
    async fn the_document_carries_nothing_but_names_months_and_the_count() {
        let (_, _, body) = get(&app(Stub::holding(fixture())), HOST).await;
        for forbidden in [
            "amount_cents",
            "currency",
            "customer_email",
            "customer_name",
            "stripe_charge_id",
            "stripe_customer",
            "receipt_url",
            "@sponsor.example",
            "ch_",
            "cus_",
            "job-",
            "step-",
            "acct-",
            "emp-david",
            "message_id",
            "\"id\"",
            "opened_on",
            "status",
            "steps",
            "metadata",
            "Not Listed",
            "Customer",
        ] {
            assert!(
                !body.contains(forbidden),
                "the roll repeats {forbidden:?}: {body}"
            );
        }
        let doc: Value = serde_json::from_str(&body).unwrap();
        let keys: Vec<&str> = doc
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["anonymous_count", "sponsors"]);
        for entry in doc["sponsors"].as_array().unwrap() {
            let keys: Vec<&str> = entry
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(keys, ["month", "name"]);
        }
    }

    #[tokio::test]
    async fn the_roll_is_paged_from_the_jobs_api() {
        // More packets than one page: every one must be read, and the
        // walk must stop when the total is reached.
        let jobs: Vec<Value> = (0..(PAGE * 2 + 7))
            .map(|i| {
                packet(
                    &format!("p{i}"),
                    "2026-09-01",
                    "yes",
                    Some(&format!("S{i:04}")),
                )
            })
            .collect();
        let stub = Stub::holding(jobs);
        let all = fetch_all(stub.as_ref()).await.unwrap();
        assert_eq!(all.len(), PAGE * 2 + 7);
        assert_eq!(stub.reads(), 3, "three pages for 407 packets");
        let roll = project(&all);
        assert_eq!(roll.sponsors.len(), PAGE * 2 + 7);
        assert_eq!(roll.anonymous_count, 0);
        // An empty listing is a roll with nobody on it — a real
        // answer, not a failure.
        let (status, _, body) = get(&app(Stub::holding(vec![])), HOST).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, r#"{"sponsors":[],"anonymous_count":0}"#);
    }

    #[tokio::test]
    async fn a_second_request_within_the_ttl_is_answered_from_the_cache() {
        let stub = Stub::holding(fixture());
        let app = app(stub.clone());
        let (status, _, first) = get(&app, HOST).await;
        assert_eq!(status, StatusCode::OK);
        let reads = stub.reads();
        assert!(reads >= 1);
        // The upstream now fails; the cached copy is current, so the
        // failure is never even seen.
        stub.set_failing(true);
        let (status, h, second) = get(&app, HOST).await;
        assert_eq!(status, StatusCode::OK, "{second}");
        assert_eq!(second, first);
        assert_eq!(stub.reads(), reads, "no upstream read on a cache hit");
        assert!(h.get("x-roll-stale").is_none());
    }

    #[tokio::test]
    async fn an_upstream_failure_with_nothing_cached_is_a_503_not_an_empty_roll() {
        let stub = Stub::holding(fixture());
        stub.set_failing(true);
        let (status, h, body) = get(&app(stub), HOST).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert_eq!(header(&h, "cache-control"), "no-store");
        assert!(
            !body.contains("sponsors") && !body.contains('{'),
            "a refusal is a sentence, not a document: {body}"
        );
        assert!(body.ends_with('\n') && body.len() > 20, "{body}");
    }

    #[tokio::test]
    async fn an_upstream_failure_with_a_stale_copy_serves_it_marked_stale() {
        let stub = Stub::holding(fixture());
        let roll = SponsorRoll::new(stub.clone());
        let (status, _, fresh) = split(roll.respond().await).await;
        assert_eq!(status, StatusCode::OK);
        // Age the cache past the TTL by hand, then fail the upstream.
        {
            let mut c = roll.cache.lock().await;
            let (_, doc) = c.take().unwrap();
            *c = Some((Instant::now() - TTL - Duration::from_secs(90), doc));
        }
        stub.set_failing(true);
        let (status, h, body) = split(roll.respond().await).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, fresh);
        assert_eq!(header(&h, "cache-control"), "no-store");
        let age: u64 = header(&h, "x-roll-stale")
            .parse()
            .expect("an age in seconds");
        assert!((150..200).contains(&age), "age {age}");
        assert_eq!(header(&h, "content-type"), "application/json");
        // The upstream returns: the next read is fresh again.
        stub.set_failing(false);
        let (status, h, _) = split(roll.respond().await).await;
        assert_eq!(status, StatusCode::OK);
        assert!(h.get("x-roll-stale").is_none());
    }

    async fn split(resp: Response) -> (StatusCode, HeaderMap, String) {
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = resp.into_body().collect().await.expect("body").to_bytes();
        (
            status,
            headers,
            String::from_utf8_lossy(&bytes).into_owned(),
        )
    }

    #[tokio::test]
    async fn the_roll_answers_on_the_site_host_only() {
        // On the application host the same path is the SPA's, which
        // gates on the session: never the roll.
        let (status, _, body) = get(&app(Stub::holding(fixture())), "boss.site.test").await;
        assert_ne!(status, StatusCode::OK, "{body}");
        assert!(!body.contains("Ada Lovelace"), "the roll leaked: {body}");
        // A site without the roll wired answers the path from the
        // directory, as any other path: a 404 here.
        let state = Arc::new(AppState {
            session_key: vec![0u8; 32],
            proxy_client: reqwest::Client::new(),
            perf: Arc::new(PerfCollector::new()),
        });
        let root = boss_testing::scratch_dir("gateway-sponsors-unwired");
        boss_testing::create_dir(&root);
        let site = Site::from_values(HOST, root.to_str().unwrap());
        let bare = site::mount(crate::build_router(None).with_state(state), site);
        let (status, _, _) = get(&bare, HOST).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
