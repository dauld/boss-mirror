//! The inquiry door — `POST /site/inquiries` on the tenant site host:
//! a visitor's form becomes a `receive-an-inquiry` packet with a
//! prospect account as its subject (design 1c20d3a9, decided by David
//! 2026-09-17 as proposed; backlog 68126ec9).
//!
//! THE SITE'S ONE WRITE. site.rs answers reads from a directory and
//! nothing else, and stays that way: this door is its own layer,
//! mounted OUTSIDE the site layer, that takes exactly one method on
//! exactly one path for the site host and hands everything else on
//! untouched. It reads no cookie and mints none; the site stays
//! session-free, and the visitor is identified by nothing but what
//! they typed.
//!
//! WHAT AN ACCEPT DOES, in order, each through the service's own
//! HTTP door signed as the gateway (`passkey::sign_as_gateway`):
//! (1) an account `acct-<slug of email>` is inserted if absent —
//! `account_type = prospect` (the tenant declares that Class), name
//! the company or else the person, one contact — and an existing
//! one (409) is fine: the second inquiry from an address lands on
//! the same subject; (2) a packet is opened, the same envelope the
//! sensor.poll handler sends, carrying every field the visitor gave
//! plus the referring page and `source = site:<host>`; (3) a form
//! post is sent to `/thanks.html` (303; the tenant ships the page), a
//! JSON post is answered `201 {"packet": id}`.
//!
//! WHAT IS REFUSED, and how. A field out of shape → 400 with the
//! problems named. More than [`LIMIT`] posts from one address in
//! [`WINDOW`] → 429; the address is `CF-Connecting-IP` when the
//! tunnel sets it, else the peer. A filled honeypot (`website`, a
//! field no person sees) → the SAME answer an accept gives and
//! NOTHING opened: a bot learns nothing from the reply. An upstream
//! that refuses — the kind not published, the Class absent, a service
//! down — → 503 with one plain sentence, and the refusal logged with
//! the body it answered. Nothing is dropped silently.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{ConnectInfo, FromRequest, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

/// The path on the site host.
pub const PATH: &str = "/site/inquiries";

/// Where a form post lands afterwards. The tenant ships the page in
/// its `site/`; the door only names it.
pub const THANKS: &str = "/thanks.html";

/// The workflow kind an inquiry opens.
pub const KIND: &str = "receive-an-inquiry";

/// The Class a prospect account is filed under.
pub const ACCOUNT_TYPE: &str = "prospect";

/// Posts one address may make in one window before it is told to
/// wait — the whole door, valid or not.
pub const LIMIT: usize = 5;
pub const WINDOW: Duration = Duration::from_secs(60 * 60);

/// The needs a visitor can name. Anything else is a 400, not a
/// guess: the value routes the packet, so it is closed here.
pub const NEEDS: [&str; 4] = ["support", "hosting", "curious", "other"];

/// The longest body read. A form of five short fields is under a
/// kilobyte; a message at its 4000-char ceiling in UTF-8 is under
/// sixteen.
const BODY_LIMIT: usize = 64 * 1024;

/// The account id's whole length, prefix included: `acct-` plus the
/// slug, cut so a long address still gives a stable, short id.
const ID_MAX: usize = 60;

/// What the visitor typed — every field present, defaulted to empty,
/// so a missing one is a validation problem with a name rather than
/// a parse failure without one. `website` is the honeypot.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Fields {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub company: String,
    #[serde(default)]
    pub need: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub website: String,
}

/// A validated inquiry: trimmed, in shape, ready to file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inquiry {
    pub name: String,
    pub email: String,
    pub company: Option<String>,
    pub need: String,
    pub message: String,
}

/// Why an upstream did not do what was asked: the status it answered
/// and its body, or the transport error — the log line's content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused(pub String);

/// The port: the two doors an accept goes through.
#[async_trait::async_trait]
pub trait Upstream: Send + Sync {
    /// `POST /api/people/accounts` with the account as the accounts
    /// service takes it. Ok whether it was created or already there.
    async fn ensure_account(&self, account: &Value) -> Result<(), Refused>;
    /// `POST /api/jobs` with the packet envelope; Ok(the packet id).
    async fn open_packet(&self, body: &Value) -> Result<String, Refused>;
}

/// Pure: the problems with what was typed, or the inquiry.
pub fn validate(f: &Fields) -> Result<Inquiry, Vec<String>> {
    let name = f.name.trim();
    let email = f.email.trim();
    let company = f.company.trim();
    let need = f.need.trim();
    let message = f.message.trim();
    let mut problems = Vec::new();
    match name.chars().count() {
        0 => problems.push("name is required".to_string()),
        n if n > 200 => problems.push("name is longer than 200 characters".to_string()),
        _ => {}
    }
    if !email_in_shape(email) {
        problems.push("email is not an address".to_string());
    }
    if company.chars().count() > 200 {
        problems.push("company is longer than 200 characters".to_string());
    }
    if !NEEDS.contains(&need) {
        problems.push(format!("need must be one of {}", NEEDS.join(", ")));
    }
    match message.chars().count() {
        0 => problems.push("message is required".to_string()),
        n if n > 4000 => problems.push("message is longer than 4000 characters".to_string()),
        _ => {}
    }
    if !problems.is_empty() {
        return Err(problems);
    }
    Ok(Inquiry {
        name: name.to_string(),
        email: email.to_string(),
        company: (!company.is_empty()).then(|| company.to_string()),
        need: need.to_string(),
        message: message.to_string(),
    })
}

/// The basic shape of an address, no more: one `@`, something on
/// each side, a dot in the domain, no whitespace, RFC-length. The
/// packet's reader replies to it; a wrong-but-shaped address costs
/// one bounced reply, not a refused inquiry.
pub fn email_in_shape(s: &str) -> bool {
    if s.is_empty() || s.len() > 254 || s.chars().any(char::is_whitespace) {
        return false;
    }
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && !domain.is_empty()
        && !domain.contains('@')
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
}

/// Pure: the account id an address maps to — lowercase, `[a-z0-9-]`,
/// runs of anything else folded to one `-`, cut to [`ID_MAX`] with
/// the prefix. Stable: the same address always names the same
/// account, which is what makes insert-if-absent the right verb.
pub fn account_id(email: &str) -> String {
    let mut slug = String::new();
    for c in email.trim().to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let mut id = format!("acct-{}", slug.trim_end_matches('-'));
    if id.len() > ID_MAX {
        id.truncate(ID_MAX);
    }
    id.trim_end_matches('-').to_string()
}

/// Pure: the account as the accounts service takes it. Name is the
/// company when one was given, else the person; one primary contact.
pub fn account_body(inq: &Inquiry) -> Value {
    let id = account_id(&inq.email);
    json!({
        "id": id,
        "name": inq.company.as_deref().unwrap_or(&inq.name),
        "account_type": ACCOUNT_TYPE,
        "contacts": [{
            "id": format!("{id}-contact"),
            "account_id": id,
            "name": inq.name,
            "role": "inquirer",
            "email": inq.email,
            "phone": null,
            "is_primary": true,
        }],
    })
}

/// Pure: the packet envelope — the shape sensor.poll's
/// `packet_body` sends, so the jobs API's required-field scan is
/// satisfied by construction. `referrer` is the `Referer` the browser
/// sent (the page the form was on) and `path` its path part.
pub fn packet_body(inq: &Inquiry, host: &str, referrer: &str, actor: &str) -> Value {
    json!({
        "kind": KIND,
        "subject": {"subject_kind": "account", "id": account_id(&inq.email)},
        "title": format!("Inquiry from {} — {}", inq.name, inq.need),
        "owner_id": actor,
        "priority": "standard",
        "status": "open",
        "metadata": {
            "name": inq.name,
            "email": inq.email,
            "company": inq.company,
            "need": inq.need,
            "message": inq.message,
            "referrer": referrer,
            "path": referrer_path(referrer),
            "source": format!("site:{host}"),
        },
        "tags": ["site"],
    })
}

/// The path of a referring URL: `https://www.x/pricing?a=b` →
/// `/pricing`. A bare path is itself; nothing is `""`.
fn referrer_path(referrer: &str) -> String {
    let after_scheme = referrer
        .split_once("://")
        .map_or(referrer, |(_, rest)| rest);
    let path = if referrer.contains("://") {
        after_scheme.find('/').map_or("", |i| &after_scheme[i..])
    } else {
        after_scheme
    };
    path.split(['?', '#']).next().unwrap_or("").to_string()
}

/// The address a post is counted against: what the tunnel says the
/// visitor's address is, else the peer that connected.
pub fn client_ip(headers: &HeaderMap, peer: Option<SocketAddr>) -> String {
    headers
        .get("cf-connecting-ip")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| peer.map(|p| p.ip().to_string()))
        .unwrap_or_else(|| "unknown".to_string())
}

/// The door: the host it answers on, the services behind it, and the
/// recent posts per address.
pub struct Door {
    host: String,
    upstream: Arc<dyn Upstream>,
    actor: String,
    recent: Mutex<HashMap<String, Vec<Instant>>>,
}

impl std::fmt::Debug for Door {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Door").field("host", &self.host).finish()
    }
}

impl Door {
    /// For the site `host` (compared the way site.rs compares it).
    pub fn new(host: &str, upstream: Arc<dyn Upstream>) -> Self {
        Self {
            host: host.trim().to_ascii_lowercase(),
            upstream,
            actor: boss_gateway::passkey::GATEWAY_ACTOR.to_string(),
            recent: Mutex::new(HashMap::new()),
        }
    }

    /// Count one post from `ip` now; `false` when it is past the
    /// limit for the window. Stale stamps are dropped as they are
    /// met, and the whole table is swept when it grows past a
    /// thousand addresses, so an hour of bots cannot pin memory.
    fn admit(&self, ip: &str, now: Instant) -> bool {
        let Ok(mut recent) = self.recent.lock() else {
            // A poisoned lock is a panicked writer, which this code
            // has none of; refusing is the safe answer if it happens.
            return false;
        };
        if recent.len() > 1000 {
            recent.retain(|_, stamps| {
                stamps.retain(|t| now.duration_since(*t) < WINDOW);
                !stamps.is_empty()
            });
        }
        let stamps = recent.entry(ip.to_string()).or_default();
        stamps.retain(|t| now.duration_since(*t) < WINDOW);
        if stamps.len() >= LIMIT {
            return false;
        }
        stamps.push(now);
        true
    }
}

/// The router with the door in front of it, or the router itself
/// when there is no site to take inquiries for. Outermost: a site
/// post never reaches the session middleware, and site.rs (which
/// answers 405 to any write) never sees this one.
pub fn mount(app: axum::Router, door: Option<Door>) -> axum::Router {
    match door {
        Some(door) => app.layer(axum::middleware::from_fn_with_state(Arc::new(door), serve)),
        None => app,
    }
}

/// The layer: `POST /site/inquiries` on the site host is the door's;
/// everything else goes on unchanged.
async fn serve(State(door): State<Arc<Door>>, req: Request, next: Next) -> Response {
    if req.method() != Method::POST
        || req.uri().path() != PATH
        || !crate::site::names_host(req.headers(), &door.host)
    {
        return next.run(req).await;
    }
    door.respond(req).await
}

/// Whether a post came from a form or a fetch: decides both how the
/// body is read and how the answer is shaped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Form,
    Json,
}

fn shape_of(headers: &HeaderMap) -> Option<Shape> {
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())?
        .split(';')
        .next()?
        .trim()
        .to_ascii_lowercase();
    match ct.as_str() {
        "application/x-www-form-urlencoded" => Some(Shape::Form),
        "application/json" => Some(Shape::Json),
        _ => None,
    }
}

impl Door {
    async fn respond(&self, req: Request) -> Response {
        let peer = req
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0);
        let ip = client_ip(req.headers(), peer);
        if !self.admit(&ip, Instant::now()) {
            return plain(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many inquiries from your address; please try again in an hour.",
            );
        }
        let Some(shape) = shape_of(req.headers()) else {
            return plain(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "Post the form as application/x-www-form-urlencoded or application/json.",
            );
        };
        let referrer = req
            .headers()
            .get(header::REFERER)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let fields = match read_fields(req, shape).await {
            Ok(f) => f,
            Err(why) => return plain(StatusCode::BAD_REQUEST, &why),
        };
        // The honeypot: a field no person sees, filled. Answer as an
        // accept would and do nothing — the reply must not tell a
        // bot which field gave it away.
        if !fields.website.trim().is_empty() {
            tracing::info!(ip, "site inquiry: honeypot filled, nothing opened");
            return thanks(shape, None);
        }
        let inq = match validate(&fields) {
            Ok(i) => i,
            Err(problems) => {
                return plain(StatusCode::BAD_REQUEST, &problems.join("; "));
            }
        };
        if let Err(Refused(why)) = self.upstream.ensure_account(&account_body(&inq)).await {
            tracing::warn!(
                ip,
                email = %inq.email,
                why,
                "site inquiry: the accounts service refused the prospect account"
            );
            return unavailable();
        }
        let body = packet_body(&inq, &self.host, &referrer, &self.actor);
        match self.upstream.open_packet(&body).await {
            Ok(id) => {
                tracing::info!(ip, packet = %id, need = %inq.need, "site inquiry: packet opened");
                thanks(shape, Some(&id))
            }
            Err(Refused(why)) => {
                tracing::warn!(
                    ip,
                    email = %inq.email,
                    why,
                    "site inquiry: the jobs API refused the open"
                );
                unavailable()
            }
        }
    }
}

/// The fields out of the body, bounded, parsed by the shape the
/// content type declared.
async fn read_fields(req: Request, shape: Shape) -> Result<Fields, String> {
    let (parts, body) = req.into_parts();
    let bytes = axum::body::to_bytes(body, BODY_LIMIT)
        .await
        .map_err(|_| format!("The form is larger than {BODY_LIMIT} bytes."))?;
    let req = Request::from_parts(parts, Body::from(bytes));
    match shape {
        Shape::Form => axum::Form::<Fields>::from_request(req, &())
            .await
            .map(|f| f.0)
            .map_err(|e| format!("The form did not parse: {}.", e.body_text())),
        Shape::Json => axum::Json::<Fields>::from_request(req, &())
            .await
            .map(|j| j.0)
            .map_err(|e| format!("The JSON did not parse: {}.", e.body_text())),
    }
}

/// The accept answer: a form is sent to the thanks page, a fetch
/// gets the packet id. `None` is the honeypot's answer — the same
/// status, no packet.
fn thanks(shape: Shape, packet: Option<&str>) -> Response {
    match shape {
        Shape::Form => {
            let mut headers = no_store();
            headers.insert(header::LOCATION, HeaderValue::from_static(THANKS));
            (StatusCode::SEE_OTHER, headers).into_response()
        }
        Shape::Json => {
            let mut headers = no_store();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            (
                StatusCode::CREATED,
                headers,
                json!({ "packet": packet }).to_string(),
            )
                .into_response()
        }
    }
}

fn no_store() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers
}

fn plain(status: StatusCode, sentence: &str) -> Response {
    (status, no_store(), format!("{sentence}\n")).into_response()
}

/// An upstream refused: one sentence, never a swallowed post.
fn unavailable() -> Response {
    plain(
        StatusCode::SERVICE_UNAVAILABLE,
        "Your inquiry could not be filed right now; please try again shortly.",
    )
}

/// The accounts and jobs services as the port, signed as the gateway.
pub struct Services {
    http: reqwest::Client,
    accounts_base: String,
    jobs_base: String,
}

impl Services {
    /// `BOSS_ACCOUNTS_UPSTREAM` / `BOSS_JOBS_UPSTREAM`, or the port
    /// registry — the same resolution the proxies and passkey.rs use.
    pub fn from_env() -> Self {
        Self::new(
            std::env::var("BOSS_ACCOUNTS_UPSTREAM").unwrap_or_else(|_| boss_ports::url("accounts")),
            std::env::var("BOSS_JOBS_UPSTREAM").unwrap_or_else(|_| boss_ports::url("jobs")),
        )
    }

    pub fn new(accounts_base: String, jobs_base: String) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
            accounts_base,
            jobs_base,
        }
    }

    async fn post(
        &self,
        url: String,
        body: &Value,
    ) -> Result<(reqwest::StatusCode, String), Refused> {
        let resp = boss_gateway::passkey::sign_as_gateway(self.http.post(&url))
            .json(body)
            .send()
            .await
            .map_err(|e| Refused(format!("{url}: unreachable: {e}")))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        Ok((status, text))
    }
}

#[async_trait::async_trait]
impl Upstream for Services {
    async fn ensure_account(&self, account: &Value) -> Result<(), Refused> {
        let url = format!("{}/api/people/accounts", self.accounts_base);
        let (status, text) = self.post(url.clone(), account).await?;
        if status.is_success() || status == reqwest::StatusCode::CONFLICT {
            return Ok(());
        }
        Err(Refused(format!("{url}: answered {status}: {text}")))
    }

    async fn open_packet(&self, body: &Value) -> Result<String, Refused> {
        let url = format!("{}/api/jobs", self.jobs_base);
        let (status, text) = self.post(url.clone(), body).await?;
        if !status.is_success() {
            return Err(Refused(format!("{url}: answered {status}: {text}")));
        }
        let created: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        created
            .get("id")
            .or_else(|| created.pointer("/data/id"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| Refused(format!("{url}: answered without an id: {text}")))
    }
}

#[cfg(test)]
mod tests {
    //! Through the gateway's own router with the site mounted and the
    //! two services stubbed in memory: every answer below is one the
    //! mounted router gave, and every write is one the stub saw.

    use super::*;
    use crate::AppState;
    use crate::perf::PerfCollector;
    use crate::site::{self, Site};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    const HOST: &str = "www.site.test";

    /// The in-memory services: what they were asked to write, and
    /// whether each door answers or refuses.
    #[derive(Default)]
    struct Stub {
        accounts: Mutex<Vec<Value>>,
        packets: Mutex<Vec<Value>>,
        account_exists: Mutex<bool>,
        jobs_refuse: Mutex<Option<String>>,
    }

    impl Stub {
        fn new() -> Arc<Self> {
            Arc::new(Self::default())
        }
        fn accounts(&self) -> Vec<Value> {
            self.accounts.lock().unwrap().clone()
        }
        fn packets(&self) -> Vec<Value> {
            self.packets.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl Upstream for Stub {
        async fn ensure_account(&self, account: &Value) -> Result<(), Refused> {
            if *self.account_exists.lock().unwrap() {
                // The accounts service's 409: nothing written, and
                // the caller carries on.
                return Ok(());
            }
            self.accounts.lock().unwrap().push(account.clone());
            Ok(())
        }
        async fn open_packet(&self, body: &Value) -> Result<String, Refused> {
            if let Some(why) = self.jobs_refuse.lock().unwrap().clone() {
                return Err(Refused(why));
            }
            let mut packets = self.packets.lock().unwrap();
            packets.push(body.clone());
            Ok(format!("packet-{}", packets.len()))
        }
    }

    fn app(stub: Arc<Stub>) -> axum::Router {
        let state = Arc::new(AppState {
            session_key: vec![0u8; 32],
            proxy_client: reqwest::Client::new(),
            perf: Arc::new(PerfCollector::new()),
        });
        let root = boss_testing::scratch_dir("gateway-inquiries");
        boss_testing::create_dir(&root);
        let site = Site::from_values(HOST, root.to_str().unwrap());
        let app = site::mount(
            crate::build_router(None, &crate::public_reads::PublicReads::none()).with_state(state),
            site,
        );
        mount(app, Some(Door::new(HOST, stub)))
    }

    const FORM: &str = "application/x-www-form-urlencoded";
    const JSON: &str = "application/json";

    /// One post: `host`, content type, body, the visitor's address
    /// as the tunnel reports it, and a referring page.
    async fn post(
        app: axum::Router,
        host: &str,
        content_type: &str,
        body: &str,
        ip: &str,
    ) -> (StatusCode, HeaderMap, String) {
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(PATH)
                    .header(header::HOST, host)
                    .header(header::CONTENT_TYPE, content_type)
                    .header("cf-connecting-ip", ip)
                    .header(header::REFERER, "https://www.site.test/pricing?ref=x")
                    // A session cookie rides along: the door must
                    // not read it, and must not answer differently.
                    .header(header::COOKIE, "boss_session=forged")
                    .body(Body::from(body.to_string()))
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

    const GOOD_FORM: &str = "name=Ada+Lovelace&email=Ada%40Example.org&company=Analytical+Engines&need=hosting&message=Can+you+host+us%3F&website=";

    fn good_json() -> String {
        json!({
            "name": "Ada Lovelace",
            "email": "ada@example.org",
            "need": "support",
            "message": "We need help.",
        })
        .to_string()
    }

    #[tokio::test]
    async fn a_form_post_opens_a_packet_on_a_prospect_account_and_lands_on_thanks() {
        let stub = Stub::new();
        let (status, h, body) = post(app(stub.clone()), HOST, FORM, GOOD_FORM, "203.0.113.7").await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
        assert_eq!(
            h.get(header::LOCATION).and_then(|v| v.to_str().ok()),
            Some(THANKS)
        );
        assert!(
            h.get(header::SET_COOKIE).is_none(),
            "the site stays session-free"
        );

        let accounts = stub.accounts();
        assert_eq!(accounts.len(), 1, "{accounts:?}");
        let acct = &accounts[0];
        assert_eq!(acct["id"], "acct-ada-example-org", "{acct}");
        assert_eq!(acct["name"], "Analytical Engines", "company over person");
        assert_eq!(acct["account_type"], ACCOUNT_TYPE);
        assert_eq!(acct["contacts"][0]["name"], "Ada Lovelace");
        assert_eq!(acct["contacts"][0]["email"], "Ada@Example.org");
        assert_eq!(acct["contacts"][0]["account_id"], "acct-ada-example-org");

        let packets = stub.packets();
        assert_eq!(packets.len(), 1, "{packets:?}");
        let p = &packets[0];
        assert_eq!(p["kind"], KIND);
        assert_eq!(p["subject"]["subject_kind"], "account");
        assert_eq!(p["subject"]["id"], "acct-ada-example-org");
        assert_eq!(p["title"], "Inquiry from Ada Lovelace — hosting");
        assert_eq!(p["owner_id"], boss_gateway::passkey::GATEWAY_ACTOR);
        assert_eq!(p["status"], "open");
        assert_eq!(p["priority"], "standard");
        assert!(p["tags"].is_array());
        let m = &p["metadata"];
        assert_eq!(m["name"], "Ada Lovelace");
        assert_eq!(m["email"], "Ada@Example.org");
        assert_eq!(m["company"], "Analytical Engines");
        assert_eq!(m["need"], "hosting");
        assert_eq!(m["message"], "Can you host us?");
        assert_eq!(m["referrer"], "https://www.site.test/pricing?ref=x");
        assert_eq!(m["path"], "/pricing");
        assert_eq!(m["source"], format!("site:{HOST}"));
    }

    #[tokio::test]
    async fn a_json_post_is_answered_with_the_packet_id() {
        let stub = Stub::new();
        let (status, h, body) =
            post(app(stub.clone()), HOST, JSON, &good_json(), "203.0.113.8").await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(
            h.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
        let v: Value = serde_json::from_str(&body).expect("json");
        assert_eq!(v["packet"], "packet-1");
        // No company: the account is named after the person.
        assert_eq!(stub.accounts()[0]["name"], "Ada Lovelace");
        assert_eq!(stub.packets()[0]["metadata"]["company"], Value::Null);
        assert_eq!(
            stub.packets()[0]["title"],
            "Inquiry from Ada Lovelace — support"
        );
    }

    #[tokio::test]
    async fn a_filled_honeypot_is_thanked_and_nothing_is_opened() {
        let stub = Stub::new();
        let bot = format!("{GOOD_FORM}http://spam.example");
        let (status, h, _) = post(app(stub.clone()), HOST, FORM, &bot, "203.0.113.9").await;
        assert_eq!(
            status,
            StatusCode::SEE_OTHER,
            "the bot sees the accept's answer"
        );
        assert_eq!(
            h.get(header::LOCATION).and_then(|v| v.to_str().ok()),
            Some(THANKS)
        );
        assert!(stub.accounts().is_empty(), "no account for a bot");
        assert!(stub.packets().is_empty(), "no packet for a bot");

        let mut j: Value = serde_json::from_str(&good_json()).unwrap();
        j["website"] = json!("http://spam.example");
        let (status, _, body) =
            post(app(stub.clone()), HOST, JSON, &j.to_string(), "203.0.113.9").await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert!(stub.packets().is_empty());
    }

    #[tokio::test]
    async fn a_field_out_of_shape_is_a_400_that_names_it_and_opens_nothing() {
        let stub = Stub::new();
        let long = "x".repeat(4001);
        let cases: [(&str, &str); 6] = [
            (
                "name=&email=a%40b.io&need=other&message=hi",
                "name is required",
            ),
            (
                "name=A&email=not-an-address&need=other&message=hi",
                "email is not an address",
            ),
            (
                "name=A&email=a%40b.io&need=sales&message=hi",
                "need must be one of",
            ),
            (
                "name=A&email=a%40b.io&need=other&message=",
                "message is required",
            ),
            ("email=a%40b.io", "need must be one of"),
            ("", "name is required"),
        ];
        for (body, names) in cases {
            let (status, _, text) = post(app(stub.clone()), HOST, FORM, body, "203.0.113.10").await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {text}");
            assert!(text.contains(names), "{body}: {text}");
        }
        // Too long, through JSON (the 4001-char message).
        let mut j: Value = serde_json::from_str(&good_json()).unwrap();
        j["message"] = json!(long);
        let (status, _, text) = post(
            app(stub.clone()),
            HOST,
            JSON,
            &j.to_string(),
            "203.0.113.11",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
        assert!(text.contains("4000"), "{text}");
        // An unknown content type is told which two are taken.
        let (status, _, text) = post(
            app(stub.clone()),
            HOST,
            "text/plain",
            "hello",
            "203.0.113.12",
        )
        .await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{text}");
        assert!(stub.accounts().is_empty());
        assert!(stub.packets().is_empty());
    }

    #[tokio::test]
    async fn the_sixth_post_from_one_address_in_an_hour_is_told_to_wait() {
        let stub = Stub::new();
        let app = app(stub.clone());
        for i in 0..LIMIT {
            let (status, _, body) =
                post(app.clone(), HOST, JSON, &good_json(), "198.51.100.1").await;
            assert_eq!(status, StatusCode::CREATED, "post {i}: {body}");
        }
        let (status, _, body) = post(app.clone(), HOST, JSON, &good_json(), "198.51.100.1").await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
        assert!(body.contains("hour"), "{body}");
        assert_eq!(
            stub.packets().len(),
            LIMIT,
            "the refused post opened nothing"
        );
        // Another address is not held by this one's count.
        let (status, _, body) = post(app.clone(), HOST, JSON, &good_json(), "198.51.100.2").await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        // The window: a stamp older than it no longer counts.
        let door = Door::new(HOST, stub.clone());
        let t0 = Instant::now();
        for _ in 0..LIMIT {
            assert!(door.admit("a", t0));
        }
        assert!(!door.admit("a", t0 + Duration::from_secs(60)));
        assert!(door.admit("a", t0 + WINDOW + Duration::from_secs(1)));
    }

    #[tokio::test]
    async fn an_address_already_on_file_still_opens_the_packet() {
        let stub = Stub::new();
        *stub.account_exists.lock().unwrap() = true;
        let (status, _, body) =
            post(app(stub.clone()), HOST, FORM, GOOD_FORM, "203.0.113.20").await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
        assert!(stub.accounts().is_empty(), "nothing re-inserted");
        assert_eq!(
            stub.packets().len(),
            1,
            "the second inquiry lands on the same subject"
        );
        assert_eq!(stub.packets()[0]["subject"]["id"], "acct-ada-example-org");
    }

    #[tokio::test]
    async fn a_refused_open_is_a_503_with_a_sentence_never_a_thanks() {
        let stub = Stub::new();
        *stub.jobs_refuse.lock().unwrap() = Some(
            "/api/jobs: answered 422: no published workflow of kind receive-an-inquiry".into(),
        );
        let (status, h, body) =
            post(app(stub.clone()), HOST, FORM, GOOD_FORM, "203.0.113.30").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert!(h.get(header::LOCATION).is_none(), "not sent to thanks");
        assert!(body.contains("could not be filed"), "{body}");
        assert!(
            !body.contains("422"),
            "the visitor gets a sentence, not the upstream's body: {body}"
        );
        assert!(stub.packets().is_empty());
    }

    #[tokio::test]
    async fn any_other_host_does_not_reach_the_door() {
        let stub = Stub::new();
        let (status, h, body) = post(
            app(stub.clone()),
            "boss.site.test",
            FORM,
            GOOD_FORM,
            "203.0.113.40",
        )
        .await;
        assert_ne!(status, StatusCode::SEE_OTHER, "{body}");
        assert_ne!(status, StatusCode::CREATED, "{body}");
        assert!(h.get(header::LOCATION).is_none());
        assert!(stub.accounts().is_empty());
        assert!(stub.packets().is_empty());
        // And a read of the path on the site host is the site's own
        // miss, not the door's.
        let resp = app(stub.clone())
            .oneshot(
                Request::builder()
                    .uri(PATH)
                    .header(header::HOST, HOST)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn an_address_always_names_the_same_short_safe_account() {
        assert_eq!(account_id("Ada@Example.org"), "acct-ada-example-org");
        assert_eq!(account_id("  ada@example.org "), "acct-ada-example-org");
        assert_eq!(account_id("a.b+c@d--e.io"), "acct-a-b-c-d-e-io");
        let long = format!("{}@example.org", "x".repeat(80));
        let id = account_id(&long);
        assert!(id.len() <= ID_MAX, "{id}");
        assert!(id.starts_with("acct-x"));
        assert!(!id.ends_with('-'));
        assert!(
            id.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        );
    }

    #[test]
    fn an_email_is_judged_on_shape_alone() {
        for ok in ["a@b.io", "first.last+tag@sub.example.co.uk", "A@B.C"] {
            assert!(email_in_shape(ok), "{ok}");
        }
        for bad in [
            "", "a", "a@", "@b.io", "a@b", "a b@c.io", "a@@b.io", "a@.io", "a@b.",
        ] {
            assert!(!email_in_shape(bad), "{bad}");
        }
        let too_long = format!("{}@b.io", "a".repeat(250));
        assert!(!email_in_shape(&too_long));
    }

    #[test]
    fn the_address_is_the_tunnels_word_else_the_peer() {
        let peer: SocketAddr = "10.0.0.5:4444".parse().unwrap();
        let mut h = HeaderMap::new();
        assert_eq!(client_ip(&h, Some(peer)), "10.0.0.5");
        assert_eq!(client_ip(&h, None), "unknown");
        h.insert("cf-connecting-ip", HeaderValue::from_static("203.0.113.1"));
        assert_eq!(client_ip(&h, Some(peer)), "203.0.113.1");
        h.insert("cf-connecting-ip", HeaderValue::from_static("  "));
        assert_eq!(client_ip(&h, Some(peer)), "10.0.0.5");
    }

    #[test]
    fn the_referring_page_keeps_its_path_and_loses_the_rest() {
        assert_eq!(
            referrer_path("https://www.x.test/pricing?ref=x"),
            "/pricing"
        );
        assert_eq!(referrer_path("https://www.x.test"), "");
        assert_eq!(referrer_path("https://www.x.test/"), "/");
        assert_eq!(referrer_path("/docs/#top"), "/docs/");
        assert_eq!(referrer_path(""), "");
    }
}
