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
//!
//! WHY THE DECLARED ROSTER IS READ FROM THE SEED FILE FIRST (backlog
//! 1ee28274, 2026-09-18). The API's answer is only worth asking after
//! the tenant has published — and the baseline must run BEFORE the
//! publish: it is what creates the only platform-admin on an example
//! instance, and Q7 owner resolution needs one before ANY platform Job
//! can open. Publish-first (b644d727) left the fresh playground
//! DEGRADED at seeds/workflows.toml ("no responsible human resolvable
//! for owner automation:bootstrap"), 0 tenant rules, sim down. So the
//! file the publish is about to send — `<tenant dir>/seeds/
//! employees.json` — is read first: when it declares the bootstrap
//! email, the injection is skipped ("the publish lands it"), the other
//! operator hires still seed, and the API is not asked. A tenant dir
//! with no roster file, or no tenant dir at all, is the behaviour
//! before this car; a roster that cannot be parsed is a refusal naming
//! the file, because injecting over an unreadable declaration is how a
//! real tenant's founder gets refused on the unique email.

use std::path::{Path, PathBuf};

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
    /// The tenant's declared roster (`<tenant dir>/seeds/employees.json`,
    /// at `path`) carries the email: the publish that follows the
    /// baseline lands that row, so nothing is injected and the API is
    /// not asked.
    DeclaredByTenant { email: String, path: PathBuf },
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

/// The roster file's relative path inside a tenant directory — the
/// file `boss tenant publish` POSTs to /api/people.
pub const ROSTER_FILE: &str = "seeds/employees.json";

/// What the tenant's declared roster says about the bootstrap email.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Roster {
    /// No tenant dir, or no roster file in it: the behaviour before
    /// backlog 1ee28274 — the API alone decides.
    NoFile,
    /// The file at `path` declares an employee with the email.
    Declares { path: PathBuf },
    /// The file at `path` parses and nobody in it carries the email.
    Silent { path: PathBuf },
}

/// Read `<tenant_dir>/seeds/employees.json` and say whether any
/// declared employee carries `email` (case-insensitively — the
/// schema's unique index is on LOWER(email)). Only the `email` field
/// is read; the file's other fields are the people API's business at
/// publish time. A file that exists but cannot be read or parsed is an
/// Err naming it, never `Silent`.
pub fn tenant_roster_holds(tenant_dir: Option<&Path>, email: &str) -> Result<Roster> {
    #[derive(Deserialize)]
    struct Declared {
        #[serde(default)]
        email: Option<String>,
    }
    let Some(dir) = tenant_dir else {
        return Ok(Roster::NoFile);
    };
    let path = dir.join(ROSTER_FILE);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Roster::NoFile),
        Err(e) => {
            return Err(e).with_context(|| format!("reading the tenant roster {}", path.display()));
        }
    };
    let rows: Vec<Declared> = serde_json::from_str(&raw)
        .with_context(|| format!("parsing the tenant roster {}", path.display()))?;
    let declared = rows.iter().any(|r| {
        r.email
            .as_deref()
            .is_some_and(|e| e.trim().eq_ignore_ascii_case(email))
    });
    Ok(if declared {
        Roster::Declares { path }
    } else {
        Roster::Silent { path }
    })
}

/// The whole decision: the declared roster answers first, and only
/// when it does not is the live roster asked through `holder_of`.
/// Mutates `seed` only in the `Injected` case.
pub fn injection_for(
    seed: &mut OperatorSeed,
    email: Option<String>,
    roster: &Roster,
    holder_of: impl FnOnce(&str) -> Result<Option<(String, Option<String>)>>,
) -> Result<Injection> {
    let Some(email) = email else {
        return Ok(Injection::NoEmail);
    };
    if let Roster::Declares { path } = roster {
        return Ok(Injection::DeclaredByTenant {
            email,
            path: path.clone(),
        });
    }
    let holder = holder_of(&email)?;
    Ok(decide_injection(seed, Some(email), holder))
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
        Injection::DeclaredByTenant { email, path } => info!(
            email = %email,
            roster = %path.display(),
            "bootstrap-admin email is declared by the tenant's roster; the publish lands it — no injection"
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

/// Read the seed file, decide the injection — the tenant's declared
/// roster under `tenant_dir` first, then the live roster — and POST
/// every hire. 409 on a duplicate id is "already hired"; any other
/// failure fails the run.
pub fn seed(people_base: &str, seed_path: &Path, tenant_dir: Option<&Path>) -> Result<Summary> {
    let raw = std::fs::read_to_string(seed_path)
        .with_context(|| format!("reading operator-baseline seed at {}", seed_path.display()))?;
    let mut seed: OperatorSeed =
        toml::from_str(&raw).with_context(|| format!("parsing {}", seed_path.display()))?;

    // Before the API check: the file is what the publish is about to
    // send, and the API's roster is empty until it has.
    let email = resolve_bootstrap_admin_email();
    let roster = match &email {
        Some(e) => tenant_roster_holds(tenant_dir, e)?,
        None => Roster::NoFile,
    };

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

    let injection = injection_for(&mut seed, email, &roster, |e| {
        holder_of_email(&client, people_base, e)
    })?;
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

    // -----------------------------------------------------------------
    // The tenant's declared roster, read from the seed FILE before the
    // baseline asks the API (backlog 1ee28274, 2026-09-18): the
    // baseline runs BEFORE the publish now, so the roster the API holds
    // is empty at that moment and only the file can say whether the
    // tenant is about to land the bootstrap email itself.
    // -----------------------------------------------------------------

    use boss_testing::{create_dir, scratch_dir, write_file};

    const FOUNDER_ROSTER: &str = r#"[{"id": "emp-david", "name": "David Auld",
  "email": "David@Algedonic.dev", "github_username": "dauld", "role": "founder",
  "department": "operations", "skill_level": null, "hire_date": "2026-09-16",
  "location": "loc-algedonic-hq", "manager_id": null, "employment_type": "full-time",
  "status": "active", "skills": [], "certifications": [], "annual_salary_cents": 0}]"#;

    fn tenant_with_roster(name: &str, roster: &str) -> std::path::PathBuf {
        let dir = scratch_dir(&format!("operator-baseline-roster-{name}"));
        create_dir(&dir.join("seeds"));
        write_file(&dir.join("seeds/employees.json"), roster);
        dir
    }

    /// A roster lookup that must NOT be consulted: when the file has
    /// already answered, the API is not asked (it is empty before the
    /// publish anyway).
    fn never_asked(_: &str) -> Result<Option<(String, Option<String>)>> {
        panic!("the roster file answered; the API must not be asked")
    }

    /// The real tenant's case: the roster declares the founder with
    /// the bootstrap email (case differs — the schema's unique index
    /// is on LOWER(email)), so the baseline SKIPS the injection, says
    /// who declares it, and the publish that follows lands the row.
    #[test]
    fn a_roster_file_declaring_the_email_stops_the_injection() {
        let dir = tenant_with_roster("declared", FOUNDER_ROSTER);
        let roster = tenant_roster_holds(Some(&dir), "david@algedonic.dev").unwrap();
        let path = dir.join("seeds/employees.json");
        assert_eq!(roster, Roster::Declares { path: path.clone() });

        let mut seed = audit_only();
        let got = injection_for(
            &mut seed,
            Some("david@algedonic.dev".into()),
            &roster,
            never_asked,
        )
        .unwrap();
        assert_eq!(
            got,
            Injection::DeclaredByTenant {
                email: "david@algedonic.dev".into(),
                path,
            }
        );
        assert_eq!(seed.hire.len(), 1, "the other operator hires still seed");
        assert!(seed.hire.iter().all(|h| h.id != BOOTSTRAP_ID));
    }

    /// The playground's case: the example tenant's roster does not
    /// carry the operator's address, so the baseline asks the API as
    /// before and, nobody holding it, injects the platform-admin — the
    /// row Q7 owner resolution needs before the publish can open its first
    /// platform Job.
    #[test]
    fn a_roster_file_without_the_email_leaves_the_injection_alone() {
        let dir = tenant_with_roster(
            "silent",
            r#"[{"id": "emp-aa-001", "email": "ceo@example.test", "role": "ceo"}]"#,
        );
        let roster = tenant_roster_holds(Some(&dir), "david@algedonic.dev").unwrap();
        assert_eq!(
            roster,
            Roster::Silent {
                path: dir.join("seeds/employees.json")
            }
        );
        let mut seed = audit_only();
        let got = injection_for(
            &mut seed,
            Some("david@algedonic.dev".into()),
            &roster,
            |_| Ok(None),
        )
        .unwrap();
        assert_eq!(
            got,
            Injection::Injected {
                email: "david@algedonic.dev".into()
            }
        );
        assert_eq!(seed.hire[0].id, BOOTSTRAP_ID);
    }

    /// No tenant dir, or a dir with no roster file, is today's
    /// behaviour: nothing to read, the API decides.
    #[test]
    fn no_tenant_dir_or_no_roster_file_reads_as_absent() {
        assert_eq!(
            tenant_roster_holds(None, "david@algedonic.dev").unwrap(),
            Roster::NoFile
        );
        let dir = scratch_dir("operator-baseline-roster-no-file");
        create_dir(&dir.join("seeds"));
        assert_eq!(
            tenant_roster_holds(Some(&dir), "david@algedonic.dev").unwrap(),
            Roster::NoFile
        );
        let missing = dir.join("no-such-tenant");
        assert_eq!(
            tenant_roster_holds(Some(&missing), "david@algedonic.dev").unwrap(),
            Roster::NoFile
        );
        // And the API's holder still wins, as before this car.
        let mut seed = audit_only();
        let got = injection_for(
            &mut seed,
            Some("david@algedonic.dev".into()),
            &Roster::NoFile,
            |_| Ok(Some(("emp-david".into(), Some("platform-admin".into())))),
        )
        .unwrap();
        assert!(matches!(got, Injection::HeldBy { .. }), "{got:?}");
    }

    /// A roster the binary cannot read is NOT "nobody declares it":
    /// injecting over it would hand a real tenant the very duplicate
    /// this check exists to prevent. Refuse, naming the file.
    #[test]
    fn a_malformed_roster_file_is_a_refusal_naming_the_file() {
        let dir = tenant_with_roster("malformed", "[{\"id\": \"emp-x\", ");
        let err = tenant_roster_holds(Some(&dir), "david@algedonic.dev").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains(&dir.join("seeds/employees.json").display().to_string()),
            "names the file: {msg}"
        );
        // An object where the roster's array should be is malformed too.
        let dir = tenant_with_roster("not-employees", "{\"employees\": []}");
        let err = tenant_roster_holds(Some(&dir), "david@algedonic.dev").unwrap_err();
        assert!(format!("{err:#}").contains("employees.json"), "{err:#}");
    }
}
