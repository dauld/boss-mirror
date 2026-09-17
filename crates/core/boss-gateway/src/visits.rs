//! Page views as telemetry — one `www-visits` reading per HTML page the
//! site surface serves (backlog 0b5c5081; the readings are the sensors
//! surface of design 14c9b2ad, David 2026-09-16: "sensors record data
//! that don't flow into the audit_log").
//!
//! WHAT ONE READING HOLDS, and nothing else: the path (no query), the
//! referrer's HOST (no path, no query), the country Cloudflare stamps
//! on the request (`CF-IPCountry`, absent when the tunnel does not),
//! a three-way user-agent class (`phone` | `desktop` | `bot`), and the
//! instant. No IP, no cookie, no fingerprint, no user agent string:
//! the payload type below has exactly these fields, so nothing the
//! request carries can leak into a reading by default. The reading's
//! `external_id` is a fresh UUIDv4 per view — the sensors door dedups
//! by it, and a page view has no natural id — so a retried batch
//! inserts nothing twice.
//!
//! A SLOW OR DARK JOBS API NEVER SLOWS A PAGE. The site handler does
//! one non-blocking `try_send` into a bounded channel ([`CAPACITY`])
//! and answers; one task drains the channel every [`EVERY`] and posts
//! what it holds in batches of [`BATCH`] through the gateway's jobs
//! upstream, signed as the gateway. A full channel DROPS the view and
//! counts it; the count is logged once a minute and reset, so a burst
//! is visible in the log as a number, not as latency. A batch the API
//! refuses is logged with the answer and let go — a reading is
//! telemetry, and retrying forever would only pin memory against the
//! next batch.
//!
//! ON BY DEFAULT WITH A SITE. The sensor id is `BOSS_SITE_VISITS_SENSOR`
//! or [`DEFAULT_SENSOR`]; without a site (site.rs' inert spelling)
//! there is no page to count, so nothing is recorded. The sensor row
//! itself is the tenant's (`seeds/sensors.toml`, `source = "site"`);
//! until the tenant declares it every batch is a logged 404, which is
//! the honest reading of "the company has not asked for this".

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::http::{HeaderMap, header};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;

/// The sensor a page view is recorded on unless
/// `BOSS_SITE_VISITS_SENSOR` names another.
pub const DEFAULT_SENSOR: &str = "www-visits";

/// Views held between drains. Ten seconds of a busy site is tens of
/// views; a thousand is a burst, and past it views are dropped and
/// counted rather than queued into memory.
pub const CAPACITY: usize = 1000;

/// Readings per POST.
pub const BATCH: usize = 50;

/// How often the channel is drained.
pub const EVERY: Duration = Duration::from_secs(10);

/// Drains between one report of the drop count — six is a minute.
const REPORT_EVERY_DRAINS: u32 = 6;

/// One page view as it is recorded. Exactly these fields — the shape
/// is the privacy boundary (the module doc says what is NOT here).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct View {
    pub path: String,
    pub referrer_host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    pub ua_class: &'static str,
    pub observed_at: DateTime<Utc>,
}

impl View {
    /// The view a request was: its path and the three headers read.
    pub fn of(path: &str, headers: &HeaderMap, observed_at: DateTime<Utc>) -> Self {
        let text = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .unwrap_or("")
        };
        let country = text("cf-ipcountry");
        Self {
            path: path.to_string(),
            referrer_host: referrer_host(text(header::REFERER.as_str())),
            country: (!country.is_empty()).then(|| country.to_ascii_uppercase()),
            ua_class: ua_class(text(header::USER_AGENT.as_str())),
            observed_at,
        }
    }

    /// The reading the sensors door takes: a fresh external id, the
    /// instant, and this view as the payload.
    pub fn reading(&self) -> Value {
        json!({
            "external_id": uuid::Uuid::new_v4().to_string(),
            "observed_at": self.observed_at,
            "payload": self,
        })
    }
}

/// `phone`, `desktop` or `bot` from a user-agent string. A small test,
/// not a database: a crawler names itself (or sends nothing), a phone
/// says `Mobile`/`Android`/`iPhone`, and the rest is a desktop. An
/// iPad on iPadOS sends a desktop string and is counted as one.
pub fn ua_class(ua: &str) -> &'static str {
    let ua = ua.trim().to_ascii_lowercase();
    if ua.is_empty() {
        return "bot";
    }
    const BOTS: [&str; 12] = [
        "bot",
        "crawl",
        "spider",
        "slurp",
        "curl/",
        "wget/",
        "python-requests",
        "python-urllib",
        "go-http-client",
        "headless",
        "lighthouse",
        "facebookexternalhit",
    ];
    if BOTS.iter().any(|b| ua.contains(b)) {
        return "bot";
    }
    if ["mobi", "android", "iphone", "ipod"]
        .iter()
        .any(|p| ua.contains(p))
    {
        return "phone";
    }
    "desktop"
}

/// The host of a `Referer` value and nothing else: no scheme, no
/// port, no path, no query, folded to lowercase. Empty when the
/// header is absent or is not a URL with a host.
pub fn referrer_host(referer: &str) -> String {
    let Some((_, rest)) = referer.trim().split_once("://") else {
        return String::new();
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // Drop any userinfo, then the port (a bracketed IPv6 literal keeps
    // its brackets).
    let host = authority.rsplit('@').next().unwrap_or("");
    let host = if host.starts_with('[') {
        host.find(']').map_or(host, |end| &host[..=end])
    } else {
        host.rsplit_once(':').map_or(host, |(h, _)| h)
    };
    host.to_ascii_lowercase()
}

/// Pure: the sensor id `BOSS_SITE_VISITS_SENSOR` names, or
/// [`DEFAULT_SENSOR`] when it is unset or blank.
pub fn sensor_named(named: &str) -> String {
    let named = named.trim();
    if named.is_empty() {
        DEFAULT_SENSOR.to_string()
    } else {
        named.to_string()
    }
}

/// The port: where a batch of readings goes.
#[async_trait::async_trait]
pub trait Upstream: Send + Sync {
    async fn record(&self, sensor: &str, readings: &[Value]) -> Result<(), String>;
}

/// The site handler's end of the channel: one non-blocking send per
/// view, and the count of views a full channel dropped.
#[derive(Clone)]
pub struct Recorder {
    tx: mpsc::Sender<Value>,
    dropped: Arc<AtomicU64>,
}

impl std::fmt::Debug for Recorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Recorder")
    }
}

impl Recorder {
    /// A recorder and the receiving end, undrained — the tests' shape,
    /// and what [`Recorder::spawn`] wires a drain task onto.
    pub fn channel(capacity: usize) -> (Self, mpsc::Receiver<Value>) {
        let (tx, rx) = mpsc::channel(capacity);
        (
            Self {
                tx,
                dropped: Arc::new(AtomicU64::new(0)),
            },
            rx,
        )
    }

    /// A recorder whose channel one task drains to `upstream` every
    /// [`EVERY`], for the life of the runtime.
    pub fn spawn(upstream: Arc<dyn Upstream>, sensor: String) -> Self {
        let (recorder, rx) = Self::channel(CAPACITY);
        let dropped = recorder.dropped.clone();
        tokio::spawn(run(rx, upstream, sensor, dropped));
        recorder
    }

    /// The sensor id the deployment names, or the default.
    pub fn sensor_from_env() -> String {
        sensor_named(&std::env::var("BOSS_SITE_VISITS_SENSOR").unwrap_or_default())
    }

    /// Queue one view. Never waits: a full channel drops it and counts
    /// the drop; a closed one (the drain task gone) is the same, since
    /// the page owes the visitor an answer either way.
    pub fn record(&self, view: &View) {
        if self.tx.try_send(view.reading()).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Views dropped since the count was last taken.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// Post everything the channel holds NOW, in batches of [`BATCH`];
/// returns how many readings were handed to the upstream. A refused
/// batch is logged — status and body — and let go; the next batch is
/// still sent, since one refusal says nothing about the next.
pub async fn drain(rx: &mut mpsc::Receiver<Value>, upstream: &dyn Upstream, sensor: &str) -> usize {
    let mut sent = 0;
    loop {
        let mut batch = Vec::with_capacity(BATCH);
        while batch.len() < BATCH {
            match rx.try_recv() {
                Ok(v) => batch.push(v),
                Err(_) => break,
            }
        }
        if batch.is_empty() {
            return sent;
        }
        let n = batch.len();
        match upstream.record(sensor, &batch).await {
            Ok(()) => sent += n,
            Err(why) => {
                tracing::warn!(
                    sensor,
                    readings = n,
                    why,
                    "site visits: a batch of page views was not recorded and is let go"
                );
            }
        }
        if n < BATCH {
            return sent;
        }
    }
}

/// The drain task: every [`EVERY`] post what is queued, and every
/// sixth drain say how many views a full channel dropped, if any.
async fn run(
    mut rx: mpsc::Receiver<Value>,
    upstream: Arc<dyn Upstream>,
    sensor: String,
    dropped: Arc<AtomicU64>,
) {
    let mut tick = tokio::time::interval(EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut drains: u32 = 0;
    loop {
        tick.tick().await;
        drain(&mut rx, upstream.as_ref(), &sensor).await;
        drains = drains.wrapping_add(1);
        if drains.is_multiple_of(REPORT_EVERY_DRAINS) {
            let n = dropped.swap(0, Ordering::Relaxed);
            if n > 0 {
                tracing::warn!(
                    sensor,
                    dropped = n,
                    "site visits: page views dropped in the last minute (the channel was full)"
                );
            }
        }
    }
}

/// The jobs API as the port: `POST /api/sensors/{id}/readings`, signed
/// as the gateway (`passkey::sign_as_gateway`), ten seconds at most —
/// the drain task's time, never a visitor's.
pub struct JobsApi {
    http: reqwest::Client,
    base: String,
}

impl JobsApi {
    /// `BOSS_JOBS_UPSTREAM`, or the port registry's jobs URL — the
    /// resolution sponsors.rs and inquiries.rs use.
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
impl Upstream for JobsApi {
    async fn record(&self, sensor: &str, readings: &[Value]) -> Result<(), String> {
        let url = format!("{}/api/sensors/{sensor}/readings", self.base);
        let resp = boss_gateway::passkey::sign_as_gateway(self.http.post(&url))
            .json(readings)
            .send()
            .await
            .map_err(|e| format!("{url}: unreachable: {e}"))?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        let text = resp.text().await.unwrap_or_default();
        Err(format!("{url}: answered {status}: {text}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use std::sync::Mutex;

    /// The in-memory upstream: every batch it was handed, and whether
    /// it refuses.
    #[derive(Default)]
    struct Stub {
        batches: Mutex<Vec<(String, Vec<Value>)>>,
        refuse: Mutex<bool>,
    }

    #[async_trait::async_trait]
    impl Upstream for Stub {
        async fn record(&self, sensor: &str, readings: &[Value]) -> Result<(), String> {
            if *self.refuse.lock().unwrap() {
                return Err("answered 404: no sensor www-visits".into());
            }
            self.batches
                .lock()
                .unwrap()
                .push((sensor.to_string(), readings.to_vec()));
            Ok(())
        }
    }

    fn at() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-17T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn a_user_agent_is_a_phone_a_desktop_or_a_bot() {
        for (ua, want) in [
            ("", "bot"),
            (
                "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)",
                "bot",
            ),
            ("curl/8.4.0", "bot"),
            ("python-requests/2.31", "bot"),
            (
                "Mozilla/5.0 (X11; Linux x86_64) HeadlessChrome/120.0",
                "bot",
            ),
            (
                "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1",
                "phone",
            ),
            (
                "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Mobile Safari/537.36",
                "phone",
            ),
            (
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15",
                "desktop",
            ),
            (
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36",
                "desktop",
            ),
            (
                "Mozilla/5.0 (X11; Linux x86_64; rv:120.0) Gecko/20100101 Firefox/120.0",
                "desktop",
            ),
        ] {
            assert_eq!(ua_class(ua), want, "{ua}");
        }
    }

    #[test]
    fn a_referrer_is_reduced_to_its_host() {
        assert_eq!(
            referrer_host("https://news.ycombinator.com/item?id=1"),
            "news.ycombinator.com"
        );
        assert_eq!(
            referrer_host("http://Example.COM:8080/a/b#c"),
            "example.com"
        );
        assert_eq!(referrer_host("https://u:p@host.test/x"), "host.test");
        assert_eq!(referrer_host("https://[::1]:8443/x"), "[::1]");
        assert_eq!(referrer_host(""), "");
        assert_eq!(referrer_host("not a url"), "");
        assert_eq!(referrer_host("android-app://com.example"), "com.example");
    }

    /// A view enqueues as one reading of exactly the declared shape —
    /// a UUIDv4 external id, the instant, and a payload with the five
    /// fields and NOTHING the request carried beyond them.
    #[test]
    fn a_view_enqueues_one_reading_of_the_declared_shape() {
        let (rec, mut rx) = Recorder::channel(10);
        let mut h = HeaderMap::new();
        h.insert(
            header::REFERER,
            HeaderValue::from_static("https://lobste.rs/s/abc?x=1"),
        );
        h.insert("cf-ipcountry", HeaderValue::from_static("us"));
        h.insert("cf-connecting-ip", HeaderValue::from_static("203.0.113.9"));
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("boss_session=forged"),
        );
        h.insert(header::USER_AGENT, HeaderValue::from_static("curl/8.4.0"));
        rec.record(&View::of("/pricing/", &h, at()));
        let reading = rx.try_recv().expect("one reading queued");
        assert!(rx.try_recv().is_err(), "and only one");
        assert_eq!(rec.dropped(), 0);
        let id = reading["external_id"].as_str().unwrap();
        let parsed = uuid::Uuid::parse_str(id).expect("a uuid");
        assert_eq!(parsed.get_version_num(), 4);
        assert_eq!(reading["observed_at"], "2026-09-17T10:00:00Z");
        assert_eq!(
            reading["payload"],
            json!({
                "path": "/pricing/",
                "referrer_host": "lobste.rs",
                "country": "US",
                "ua_class": "bot",
                "observed_at": "2026-09-17T10:00:00Z",
            })
        );
        let text = reading.to_string();
        assert!(!text.contains("203.0.113.9"), "no ip: {text}");
        assert!(!text.contains("forged"), "no cookie: {text}");
        assert!(!text.contains("curl"), "no user agent string: {text}");
        // A second view of the same page is a different reading.
        rec.record(&View::of("/pricing/", &h, at()));
        let again = rx.try_recv().unwrap();
        assert_ne!(again["external_id"], reading["external_id"]);
        // Without the tunnel's header the country is simply absent.
        let none = View::of("/", &HeaderMap::new(), at());
        assert_eq!(none.country, None);
        assert!(
            !none.reading()["payload"]
                .as_object()
                .unwrap()
                .contains_key("country")
        );
    }

    /// A full channel drops and counts; the recorder never waits (the
    /// send is `try_send`, so this test would hang if it did).
    #[test]
    fn a_full_queue_drops_without_blocking_and_counts() {
        let (rec, mut rx) = Recorder::channel(2);
        let v = View::of("/", &HeaderMap::new(), at());
        for _ in 0..5 {
            rec.record(&v);
        }
        assert_eq!(rec.dropped(), 3);
        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_err());
        // The channel closed (drain task gone) is a drop too, not a
        // panic and not a wait.
        drop(rx);
        rec.record(&v);
        assert_eq!(rec.dropped(), 4);
    }

    /// The drain posts what is queued in batches of BATCH, to the
    /// named sensor, and posts nothing when nothing is queued.
    #[tokio::test]
    async fn the_drain_posts_the_queue_in_batches() {
        let (rec, mut rx) = Recorder::channel(CAPACITY);
        let v = View::of("/", &HeaderMap::new(), at());
        for _ in 0..(BATCH * 2 + 3) {
            rec.record(&v);
        }
        let stub = Stub::default();
        let sent = drain(&mut rx, &stub, "www-visits").await;
        assert_eq!(sent, BATCH * 2 + 3);
        let (sizes, sensors): (Vec<usize>, Vec<String>) = stub
            .batches
            .lock()
            .unwrap()
            .iter()
            .map(|(s, b)| (b.len(), s.clone()))
            .unzip();
        assert_eq!(sizes, [BATCH, BATCH, 3]);
        assert!(sensors.iter().all(|s| s == "www-visits"));
        assert_eq!(drain(&mut rx, &stub, "www-visits").await, 0);
        assert_eq!(
            stub.batches.lock().unwrap().len(),
            3,
            "an empty queue posts nothing"
        );
    }

    /// A refused batch is let go — logged, not requeued, not retried —
    /// and the next drain starts empty.
    #[tokio::test]
    async fn a_failed_post_is_let_go_not_retried() {
        let (rec, mut rx) = Recorder::channel(CAPACITY);
        let v = View::of("/", &HeaderMap::new(), at());
        for _ in 0..3 {
            rec.record(&v);
        }
        let stub = Stub::default();
        *stub.refuse.lock().unwrap() = true;
        assert_eq!(drain(&mut rx, &stub, "www-visits").await, 0);
        assert!(rx.try_recv().is_err(), "the refused batch is not requeued");
        *stub.refuse.lock().unwrap() = false;
        assert_eq!(drain(&mut rx, &stub, "www-visits").await, 0);
        assert!(
            stub.batches.lock().unwrap().is_empty(),
            "nothing was retried"
        );
    }

    #[test]
    fn the_sensor_is_www_visits_unless_named() {
        assert_eq!(sensor_named(""), "www-visits");
        assert_eq!(sensor_named("  "), "www-visits");
        assert_eq!(sensor_named(" site-views "), "site-views");
    }
}
