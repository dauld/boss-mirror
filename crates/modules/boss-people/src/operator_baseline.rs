//! The operator baseline — `emp-audit` plus the injected bootstrap
//! admin — POSTed to a running people-api, idempotently. The library
//! half of `boss-operator-baseline-seed`; the binary parses argv and
//! calls [`seed`], so a test can run the SAME path against a real
//! people router over a TestDb instead of a hand-typed mirror of it.
//!
//! WHY THE INJECTION ASKS THE PEOPLE API FIRST (backlog 0d2d7daa,
//! 2026-09-16). Until then the injection checked only its OWN seed
//! file for the bootstrap email, so a tenant that declares the same
//! person — the real company's `emp-david` carries the very address
//! `BOSS_BOOTSTRAP_ADMIN_EMAIL` names — got a second row for one
//! email (or the schema's LOWER(email) unique index refused whichever
//! came second, and the OIDC login's lower(email) match had two
//! candidates). The roster is the fact; the seed file is one input to
//! it. So: `GET /api/people?email=` first, and when someone holds the
//! address, no injection — the line printed names who.
//!
//! An unreachable roster is NOT "nobody holds it": the lookup's
//! transport error fails the run, and the caller
//! (infra/seed-operator-baseline.sh) retries while the API binds. No
//! evidence is not a pass.

use std::path::Path;

use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde::Deserialize;
use tracing::{info, warn};

use crate::types::Employee;

/// `infra/operator-baseline/operator_hires.toml`'s shape.
#[derive(Debug, Deserialize)]
pub struct OperatorSeed {
    pub hire: Vec<Employee>,
}

/// The id the injection uses; the people API retires it the moment a
/// named platform-admin is hired (`http::BOOTSTRAP_IDENTITY`).
pub const BOOTSTRAP_ID: &str = "emp-bootstrap-admin";

/// What the injection decided, for the caller's log and for the proof
/// that reads it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Injection {
    /// No email resolved (env unset, no readable credentials file).
    NoEmail,
    /// The row was put at the head of the hire list.
    Injected { email: String },
    /// The seed file itself lists the email; nothing injected.
    InSeedFile { email: String },
    /// The roster already holds the email — a tenant declared the
    /// person — so nothing is injected. `role` is carried because a
    /// holder who is NOT a platform-admin leaves the deployment with no
    /// platform-admin at all, and Q7 owner resolution (every platform
    /// Job names a human owner, by role) then refuses automation-owned
    /// packets; the caller warns by name rather than discovering it at
    /// the first train.
    HeldBy {
        email: String,
        id: String,
        role: Option<String>,
    },
}

/// What one run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub injection: Injection,
    pub inserted: u64,
    pub skipped: u64,
}

/// Find the bootstrap-admin email. Precedence:
///   1. BOSS_BOOTSTRAP_ADMIN_EMAIL env var
///   2. First `[[credential]]` row in BOSS_AUTH_FILE (default
///      /var/lib/boss/auth/credentials.toml). This is the file
///      the gateway's local_auth reads; using it as the source
///      means there's one canonical "who is the operator" file.
///
/// Returns None when neither produces a value — bootstrap runs
/// without injection, and the operator must POST /api/people
/// manually before login.
pub fn resolve_bootstrap_admin_email() -> Option<String> {
    if let Ok(email) = std::env::var("BOSS_BOOTSTRAP_ADMIN_EMAIL") {
        let trimmed = email.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    let auth_file = std::env::var("BOSS_AUTH_FILE")
        .unwrap_or_else(|_| "/var/lib/boss/auth/credentials.toml".to_string());
    let raw = match std::fs::read_to_string(&auth_file) {
        Ok(raw) => raw,
        Err(e) => {
            // Loud, with the path and the reason. This fallback failing
            // silently is what let a reset produce a demo with no
            // platform-admin — and therefore no publishable Workflows,
            // since the Q7 owner gate rejects the bootstrap Job.
            info!(
                path = %auth_file,
                error = %e,
                "bootstrap-admin: credentials file unreadable"
            );
            return None;
        }
    };
    match bootstrap_email_from_credentials(&raw) {
        Some(email) => Some(email),
        None => {
            info!(
                path = %auth_file,
                "bootstrap-admin: credentials file has no [[credential]] email"
            );
            None
        }
    }
}

/// Pull the first `[[credential]]` row's `email` out of the gateway's
/// auth file.
///
/// Split out of `resolve_bootstrap_admin_email` so the parse is
/// testable without env vars or a real file — the untestability is
/// why the bug below survived.
///
/// Deserializes into a typed struct rather than walking a
/// `toml::Value`. Under `toml` 1.x a `[[credential]]` document parses
/// to a `Value::Table` whose `get("credential")` the previous code
/// chained through with `?`, and any single step returning `None`
/// collapsed the whole thing to "no email" with no way to tell which.
/// A `Deserialize` impl is both what the rest of the codebase does and
/// impossible to silently mis-index.
pub fn bootstrap_email_from_credentials(raw: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct AuthFile {
        #[serde(default)]
        credential: Vec<Credential>,
    }
    #[derive(serde::Deserialize)]
    struct Credential {
        email: String,
    }
    let parsed: AuthFile = toml::from_str(raw).ok()?;
    let email = parsed.credential.into_iter().next()?.email;
    let trimmed = email.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn display_name_from_email(email: &str) -> String {
    let local = email.split('@').next().unwrap_or(email);
    let mut chars = local.chars();
    match chars.next() {
        Some(c) => c.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

/// The bootstrap admin's row: role=platform-admin, the single row the
/// system needs to bootstrap itself — after it exists, the credential
/// → Employee resolution via lower(email) match works at login time
/// without the gateway auto-provisioning.
pub fn bootstrap_admin_row(email: &str) -> Employee {
    Employee {
        id: BOOTSTRAP_ID.to_string(),
        name: Some(display_name_from_email(email)),
        email: Some(email.to_string()),
        role: Some("platform-admin".to_string()),
        // `it`, not `platform`. The operator and the agent ARE
        // the IT department — one person and one AI — so they
        // are employees of the tenant like anyone else. A
        // department invented to hold the people who run the
        // software is a silo the org chart does not have.
        department: Some("it".to_string()),
        skill_level: None,
        skills: Vec::new(),
        hire_date: chrono::NaiveDate::from_ymd_opt(2023, 1, 1),
        location: Some("loc-hq".to_string()),
        manager_id: None,
        employment_type: Some("full-time".to_string()),
        status: Some("active".to_string()),
        certifications: Vec::new(),
        annual_salary_cents: None,
    }
}

/// Who on the roster holds `email`, as `(id, role)`, or None. Asks
/// `GET /api/people?email=` — the people API's own case-insensitive
/// filter — so the answer is the roster's, not this binary's reading
/// of a file. A transport error or a non-2xx is an Err, never None.
pub fn holder_of_email(
    client: &Client,
    people_base: &str,
    email: &str,
) -> Result<Option<(String, Option<String>)>> {
    let url = format!("{}/api/people", people_base.trim_end_matches('/'));
    let resp = client
        .get(&url)
        .query(&[("email", email)])
        .send()
        .with_context(|| format!("GET {url}?email=… (is the people-api up?)"))?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!(
            "GET {url}?email=… → {status} {}",
            resp.text().unwrap_or_default()
        );
    }
    let rows: Vec<Employee> = resp
        .json()
        .with_context(|| format!("GET {url}?email=… body"))?;
    Ok(rows.into_iter().next().map(|e| (e.id, e.role)))
}

/// The decision, pure: given the resolved email (if any) and who holds
/// it on the roster (if anyone), inject or not. Mutates `seed` only in
/// the `Injected` case.
pub fn decide_injection(
    seed: &mut OperatorSeed,
    email: Option<String>,
    holder: Option<(String, Option<String>)>,
) -> Injection {
    let Some(email) = email else {
        return Injection::NoEmail;
    };
    if let Some((id, role)) = holder {
        return Injection::HeldBy { email, id, role };
    }
    let in_file = seed.hire.iter().any(|h| {
        h.email
            .as_deref()
            .is_some_and(|e| e.eq_ignore_ascii_case(&email))
    });
    if in_file {
        return Injection::InSeedFile { email };
    }
    seed.hire.insert(0, bootstrap_admin_row(&email));
    Injection::Injected { email }
}

fn log_injection(injection: &Injection) {
    match injection {
        Injection::NoEmail => info!(
            "BOSS_BOOTSTRAP_ADMIN_EMAIL unset and no credentials file readable; \
             skipping bootstrap-admin Employee injection. Logins will require an \
             explicit /api/people POST or a matching template row in \
             operator_hires.toml."
        ),
        Injection::Injected { email } => info!(
            operator_id = BOOTSTRAP_ID,
            email = %email,
            "injecting bootstrap-admin Employee row at head of hire list"
        ),
        Injection::InSeedFile { email } => info!(
            email = %email,
            "bootstrap-admin email already present in operator_hires.toml; no injection"
        ),
        Injection::HeldBy { email, id, role } => {
            info!(
                email = %email,
                held_by = %id,
                role = role.as_deref().unwrap_or(""),
                "bootstrap-admin email already held on the roster (a tenant declared the person); no injection"
            );
            if role.as_deref() != Some("platform-admin") {
                warn!(
                    held_by = %id,
                    role = role.as_deref().unwrap_or(""),
                    "no bootstrap admin injected and the holder is not a platform-admin: \
                     unless another active platform-admin exists, Q7 owner resolution \
                     cannot name a human for automation-owned platform Jobs \
                     (owner_role = platform-admin) — declare the role on the tenant's \
                     row or hire one"
                );
            }
        }
    }
}

/// Read the seed file, decide the injection against the live roster,
/// and POST every hire. 409 on a duplicate id is "already hired";
/// any other failure fails the run.
pub fn seed(people_base: &str, seed_path: &Path) -> Result<Summary> {
    let raw = std::fs::read_to_string(seed_path)
        .with_context(|| format!("reading operator-baseline seed at {}", seed_path.display()))?;
    let mut seed: OperatorSeed =
        toml::from_str(&raw).with_context(|| format!("parsing {}", seed_path.display()))?;

    // The operator-baseline loads AS the public API like every
    // other external caller. The actor identity is a dedicated
    // platform-admin automation identity — these are founding
    // platform operators, not tenant employees.
    //
    // SIGNED, NOT SIMULATED (backlog 09887242, 2026-09-17). Until then
    // the client also sent `x-sim-origin: true`, from the initial
    // commit, with no reason recorded — and it was not load-bearing:
    // POST /api/people has no policy gate, and the tier above holds
    // every grant. Its effect was that emp-audit, the platform's own
    // auditor (the audit-readonly reader every projection admits),
    // was prod's one `_simulated: true` employee, in the set the
    // cutover TRIMS. The rows this seed lands are the platform's, and
    // real.
    let user_header = serde_json::json!({
        "id": "automation:operator-baseline",
        "role": "platform-admin",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "it",
    })
    .to_string();
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "x-boss-user",
        reqwest::header::HeaderValue::from_str(&user_header)
            .with_context(|| "x-boss-user header value")?,
    );
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .default_headers(headers)
        .build()
        .with_context(|| "building reqwest client")?;

    let email = resolve_bootstrap_admin_email();
    let holder = match &email {
        Some(e) => holder_of_email(&client, people_base, e)?,
        None => None,
    };
    let injection = decide_injection(&mut seed, email, holder);
    log_injection(&injection);

    let url = format!("{}/api/people", people_base.trim_end_matches('/'));
    let mut inserted = 0u64;
    let mut skipped = 0u64;
    let mut failed = 0u64;
    for emp in &seed.hire {
        let resp = match client.post(&url).json(emp).send() {
            Ok(r) => r,
            Err(e) => {
                warn!(operator_id = %emp.id, error = %e, "POST operator transport error");
                failed += 1;
                continue;
            }
        };
        let status = resp.status();
        if status.is_success() {
            inserted += 1;
            info!(operator_id = %emp.id, role = emp.role.as_deref().unwrap_or(""), "operator hired");
        } else if status.as_u16() == 409 {
            skipped += 1;
            info!(operator_id = %emp.id, "operator already hired, skipping");
        } else {
            let body = resp.text().unwrap_or_default();
            warn!(operator_id = %emp.id, %status, body = %body, "POST operator failed");
            failed += 1;
        }
    }

    if failed > 0 {
        anyhow::bail!(
            "{failed} operator-baseline POSTs failed (inserted={inserted}, skipped={skipped}). \
             The operator-baseline must land before downstream references resolve."
        );
    }

    info!(inserted, skipped, "operator-baseline seed complete");
    Ok(Summary {
        injection,
        inserted,
        skipped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact shape the gateway's local_auth writes, byte-for-byte
    /// from the playground's `/var/lib/boss/auth/credentials.toml`.
    /// This file read as valid UTF-8 and parsed fine in other TOML
    /// implementations, yet the old `toml::Value`-walking resolver
    /// returned None for it — leaving a reset with no platform-admin,
    /// which the Q7 owner gate then turned into "prepare publishes
    /// zero Workflows" and a demo that could only idle.
    const REAL_CREDENTIALS: &str = r#"[[credential]]
email = "david@algedonic.dev"
password_hash = "$argon2id$v=19$m=19456,t=2,p=1$4LpQUAH90UxRT3D73PzcyQ$R6v2W4pFoHc9czv8L4cbvbr1vX3VYxGQZhkvNqRiegE"
created_at = "2026-06-03T04:12:15.045062115Z"
last_rotated = "2026-07-07T16:13:36.282845447Z"
"#;

    #[test]
    fn reads_the_email_from_a_real_credentials_file() {
        assert_eq!(
            bootstrap_email_from_credentials(REAL_CREDENTIALS).as_deref(),
            Some("david@algedonic.dev"),
        );
    }

    #[test]
    fn takes_the_first_credential_when_several_exist() {
        let raw = r#"
[[credential]]
email = "first@example.com"
password_hash = "x"

[[credential]]
email = "second@example.com"
password_hash = "y"
"#;
        assert_eq!(
            bootstrap_email_from_credentials(raw).as_deref(),
            Some("first@example.com"),
        );
    }

    #[test]
    fn no_credentials_yields_none() {
        assert_eq!(bootstrap_email_from_credentials(""), None);
        assert_eq!(
            bootstrap_email_from_credentials("[[credential]]\nemail = \"\"\n"),
            None
        );
    }

    #[test]
    fn malformed_toml_yields_none_rather_than_panicking() {
        assert_eq!(
            bootstrap_email_from_credentials("this is not = = toml"),
            None
        );
    }

    fn audit_only() -> OperatorSeed {
        let mut audit = bootstrap_admin_row("audit@boss.example");
        audit.id = "emp-audit".into();
        audit.role = Some("audit-readonly".into());
        OperatorSeed { hire: vec![audit] }
    }

    #[test]
    fn injects_when_nobody_holds_the_email() {
        let mut seed = audit_only();
        let got = decide_injection(&mut seed, Some("ops@example.com".into()), None);
        assert_eq!(
            got,
            Injection::Injected {
                email: "ops@example.com".into()
            }
        );
        assert_eq!(seed.hire[0].id, BOOTSTRAP_ID, "at the head of the list");
        assert_eq!(seed.hire.len(), 2);
    }

    /// The packet's case (0d2d7daa): the tenant declared `emp-david`
    /// with the bootstrap email. The roster's answer wins over the
    /// seed file's silence, and the row is NOT injected.
    #[test]
    fn a_roster_holder_of_the_email_stops_the_injection() {
        let mut seed = audit_only();
        let got = decide_injection(
            &mut seed,
            Some("david@example.com".into()),
            Some(("emp-david".into(), Some("founder".into()))),
        );
        assert_eq!(
            got,
            Injection::HeldBy {
                email: "david@example.com".into(),
                id: "emp-david".into(),
                role: Some("founder".into()),
            }
        );
        assert_eq!(seed.hire.len(), 1, "nothing inserted");
        assert!(seed.hire.iter().all(|h| h.id != BOOTSTRAP_ID));
    }

    #[test]
    fn the_seed_files_own_row_stops_the_injection_as_before() {
        let mut seed = audit_only();
        let got = decide_injection(&mut seed, Some("AUDIT@boss.example".into()), None);
        assert_eq!(
            got,
            Injection::InSeedFile {
                email: "AUDIT@boss.example".into()
            }
        );
        assert_eq!(seed.hire.len(), 1);
    }

    #[test]
    fn no_email_means_no_injection() {
        let mut seed = audit_only();
        assert_eq!(decide_injection(&mut seed, None, None), Injection::NoEmail);
        assert_eq!(seed.hire.len(), 1);
    }
}
