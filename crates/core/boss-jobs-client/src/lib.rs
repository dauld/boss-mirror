//! HTTP client port for reaching the `boss-jobs` service.
//!
//! Defines tenant-neutral, question-shaped methods other services
//! need to ask Jobs — today one: "what Jobs match these filters?".
//! A consumer projects the rows into its own shape in its own crate,
//! keeping this trait generic over Workflow / StepType vocabulary.
//!
//! The phase-distribution question it also asked had one consumer, a
//! warehouse stage count filled only by a retired example tenant's
//! Workflows, and left with it (backlog a8991c86).

use async_trait::async_trait;
use boss_core::http_client::{self, HttpClientError, ServiceLabel};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// Service-name marker for the shared [`HttpClientError`]. Keeps the
/// `Display` text reading `"jobs service unreachable: …"`.
#[derive(Debug)]
pub struct Jobs;
impl ServiceLabel for Jobs {
    const NAME: &'static str = "jobs";
}

/// Transport error for the Jobs client. Alias of the shared
/// [`HttpClientError`] so existing constructors and matches keep
/// compiling.
pub type JobsClientError = HttpClientError<Jobs>;

/// One row from `/api/jobs`. Generic across Workflows — callers
/// filter by `kind` and project to their own row shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobSummary {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub priority: String,
    pub status: String,
    pub opened_on: NaiveDate,
    pub closed_on: Option<NaiveDate>,
}

/// Who a flights read is for — the actor a behaviour flight gates, passed
/// explicitly because a service has no session of its own to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewer<'a> {
    pub id: &'a str,
    pub role: &'a str,
}

/// The flights on for one viewer (design c4c2a607, backlog 73c31776):
/// the Rust half of the web's `flightOn`, reading the SAME
/// `GET /api/flights/mine` the gateway inlines into the page, so a
/// behaviour flight and a UI flight cannot disagree about who is in.
///
/// A code it does not hold is off. The default — no answer — holds
/// none, so a caller that falls back to it on an error takes the old
/// path, which is the safe one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FlightsOn(std::collections::BTreeSet<String>);

impl FlightsOn {
    /// Is the flight `code` on for the viewer this was read for?
    pub fn on(&self, code: &str) -> bool {
        self.0.contains(code)
    }

    /// The set a `{"flights": [codes]}` answer names. A body of any
    /// other shape is refused by name, never read as "nothing on" —
    /// the caller decides whether a refusal falls back to off.
    pub fn from_answer(body: &serde_json::Value) -> Result<Self, JobsClientError> {
        body.get("flights")
            .and_then(serde_json::Value::as_array)
            .and_then(|codes| {
                codes
                    .iter()
                    .map(|c| c.as_str().map(str::to_string))
                    .collect::<Option<_>>()
            })
            .map(Self)
            .ok_or_else(|| {
                JobsClientError::MalformedBody(format!(
                    "not a flights answer (want {{\"flights\": [codes]}}): {body}"
                ))
            })
    }
}

#[async_trait]
pub trait JobsClient: Send + Sync {
    /// List Jobs matching the given filters. `kind` selects a
    /// Workflow; `subject_id` filters by the Job subject id;
    /// `limit` caps the response.
    async fn list_jobs(
        &self,
        kind: Option<&str>,
        subject_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<JobSummary>, JobsClientError>;

    /// The flights on for `viewer` — `GET /api/flights/mine` asked as
    /// that viewer. `FlightsOn::default()` is the safe fallback on an
    /// error: every flight off.
    async fn flights_for(&self, viewer: &Viewer<'_>) -> Result<FlightsOn, JobsClientError>;
}

/// Production `JobsClient` that calls the jobs HTTP API over reqwest.
/// 5-second timeout matches the `AssetsClient` convention — an
/// unresponsive jobs service shouldn't wedge its caller indefinitely.
pub struct ReqwestJobsClient {
    base_url: String,
    http: reqwest::Client,
}

impl ReqwestJobsClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        let (base_url, http) = http_client::base(base_url);
        Self { base_url, http }
    }
}

#[async_trait]
impl JobsClient for ReqwestJobsClient {
    async fn list_jobs(
        &self,
        kind: Option<&str>,
        subject_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<JobSummary>, JobsClientError> {
        // List endpoint's query param for "filter by subject" is
        // `subject_id` (see `ListJobsQuery` in boss-jobs/src/http.rs).
        let mut url = format!("{}/api/jobs?limit={limit}", self.base_url);
        if let Some(k) = kind {
            url.push_str(&format!("&kind={k}"));
        }
        if let Some(sid) = subject_id {
            url.push_str(&format!("&subject_id={sid}"));
        }
        let body: serde_json::Value = http_client::get_json(&self.http, &url).await?;
        let data = body.get("data").and_then(|v| v.as_array()).ok_or_else(|| {
            JobsClientError::MalformedBody(format!("missing data array in {body}"))
        })?;
        Ok(project_job_summaries(data))
    }

    async fn flights_for(&self, viewer: &Viewer<'_>) -> Result<FlightsOn, JobsClientError> {
        // The identity header the jobs API resolves the audience from —
        // the shape the gateway signs for a browser session.
        let user = serde_json::json!({ "id": viewer.id, "role": viewer.role }).to_string();
        let resp = self
            .http
            .get(format!("{}/api/flights/mine", self.base_url))
            .header("x-boss-user", user)
            .send()
            .await
            .map_err(|e| JobsClientError::Unreachable(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(JobsClientError::UnexpectedStatus(resp.status().as_u16()));
        }
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| JobsClientError::MalformedBody(e.to_string()))?;
        FlightsOn::from_answer(&body)
    }
}

/// Pure projector: `/api/jobs` data array → JobSummary rows.
/// Rows missing `opened_on` are skipped (the field is non-nullable
/// on the server side; absence indicates malformed data).
pub fn project_job_summaries(rows: &[serde_json::Value]) -> Vec<JobSummary> {
    rows.iter()
        .filter_map(|r| {
            Some(JobSummary {
                id: r.get("id").and_then(|v| v.as_str())?.to_string(),
                kind: r
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                title: r
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                priority: r
                    .get("priority")
                    .and_then(|v| v.as_str())
                    .unwrap_or("standard")
                    .to_string(),
                status: r
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("open")
                    .to_string(),
                opened_on: r
                    .get("opened_on")
                    .and_then(|v| v.as_str())
                    .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())?,
                closed_on: r
                    .get("closed_on")
                    .and_then(|v| v.as_str())
                    .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn project_job_summaries_reads_dates_and_fields() {
        let rows = vec![
            json!({
                "id": "J-1",
                "kind": "field-service",
                "title": "On-site repair — heat sink",
                "priority": "urgent",
                "status": "closed",
                "opened_on": "2026-01-05",
                "closed_on": "2026-01-07"
            }),
            json!({
                "id": "J-2",
                "kind": "field-service",
                "title": "Calibration follow-up",
                "priority": "standard",
                "status": "open",
                "opened_on": "2026-03-20"
            }),
        ];
        let projected = project_job_summaries(&rows);
        assert_eq!(projected.len(), 2);
        assert_eq!(projected[0].id, "J-1");
        assert_eq!(projected[0].priority, "urgent");
        assert_eq!(
            projected[0].closed_on,
            Some(NaiveDate::from_ymd_opt(2026, 1, 7).unwrap())
        );
        assert!(projected[1].closed_on.is_none());
    }

    #[test]
    fn a_flights_answer_is_the_set_of_codes_on_and_unlisted_is_off() {
        let on = FlightsOn::from_answer(&json!({"flights": ["it-map-motion"]})).unwrap();
        assert!(on.on("it-map-motion"));
        assert!(!on.on("something-else"));
        assert!(
            !FlightsOn::default().on("it-map-motion"),
            "no answer: all off"
        );
    }

    #[test]
    fn a_body_that_is_not_the_answer_is_refused_not_read_as_all_off() {
        for bad in [
            json!({}),
            json!({"flights": "it-map-motion"}),
            json!({"flights": ["x", 1]}),
        ] {
            assert!(
                matches!(
                    FlightsOn::from_answer(&bad),
                    Err(JobsClientError::MalformedBody(_))
                ),
                "{bad}"
            );
        }
    }

    /// The Rust half reads the SAME endpoint the page does, as the
    /// viewer it names: the id and role ride as the `x-boss-user` the
    /// jobs API resolves the audience from.
    #[tokio::test]
    async fn flights_for_asks_the_one_read_as_the_named_viewer() {
        use axum::http::HeaderMap;
        let app = axum::Router::new().route(
            "/api/flights/mine",
            axum::routing::get(|headers: HeaderMap| async move {
                let user: serde_json::Value = headers
                    .get("x-boss-user")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default();
                let on = user["id"] == "emp-david" && user["role"] == "platform-admin";
                axum::Json(json!({ "flights": if on { vec!["it-map-motion"] } else { vec![] } }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let client = ReqwestJobsClient::new(base);
        let david = client
            .flights_for(&Viewer {
                id: "emp-david",
                role: "platform-admin",
            })
            .await
            .unwrap();
        assert!(david.on("it-map-motion"));
        let other = client
            .flights_for(&Viewer {
                id: "emp-ops",
                role: "operator",
            })
            .await
            .unwrap();
        assert!(!other.on("it-map-motion"));
    }

    #[test]
    fn project_job_summaries_skips_rows_without_opened_on() {
        let rows = vec![
            json!({ "id": "ok", "opened_on": "2026-04-01" }),
            json!({ "id": "bad" }),
        ];
        let projected = project_job_summaries(&rows);
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].id, "ok");
    }
}
