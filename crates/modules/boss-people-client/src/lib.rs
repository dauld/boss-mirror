//! HTTP client port for reaching the boss-people service.
//!
//! Several domain services need to ask people questions as part of
//! their own write guards — "does this employee actually exist
//! before we accept an event with their actor_id?", "does this
//! account actually exist before we create an opportunity targeting
//! it?". We express those as trait methods so production code can
//! call the real people HTTP API while tests substitute a fake.
//!
//! Mirror of `boss-assets-client`. The trait and reqwest adapter live
//! in this crate so both `boss-assets` and `boss-commerce` can depend
//! on one shared definition without creating awkward cross-domain
//! crate dependencies.

use async_trait::async_trait;
use boss_core::http_client::{self, HttpClientError, ServiceLabel};

pub mod platform_owner;
pub use platform_owner::ReqwestPlatformOwner;

/// Service-name marker for the shared [`HttpClientError`]. Keeps the
/// `Display` text reading `"people service unreachable: …"`.
#[derive(Debug)]
pub struct People;
impl ServiceLabel for People {
    const NAME: &'static str = "people";
}

/// Transport error for the People client. Alias of the shared
/// [`HttpClientError`] so existing `PeopleClientError::Unreachable`
/// constructors and matches keep compiling.
pub type PeopleClientError = HttpClientError<People>;

/// Existence questions other domain services ask people before
/// accepting writes that reference an employee or account id.
#[async_trait]
pub trait PeopleClient: Send + Sync {
    /// Whether an employee with the given id exists. Used by assets
    /// to validate `AssetEvent.actor_id` (when not None) so a
    /// typo or stale reference doesn't pollute the audit log.
    async fn employee_exists(&self, employee_id: &str) -> Result<bool, PeopleClientError>;

    /// Whether a account with the given id exists. Used by commerce
    /// to validate `Opportunity.account_id` so we can't create a
    /// pipeline opportunity for a account that was never onboarded
    /// or has since been deleted.
    async fn account_exists(&self, account_id: &str) -> Result<bool, PeopleClientError>;
}

/// Production `PeopleClient` that calls the people HTTP API over
/// reqwest. 5-second timeout per call so an unresponsive people
/// service can't wedge a write indefinitely.
pub struct ReqwestPeopleClient {
    base_url: String,
    http: reqwest::Client,
}

impl ReqwestPeopleClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        let (base_url, http) = http_client::base(base_url);
        Self { base_url, http }
    }
}

#[async_trait]
impl PeopleClient for ReqwestPeopleClient {
    async fn employee_exists(&self, employee_id: &str) -> Result<bool, PeopleClientError> {
        let url = format!("{}/api/people/{}/exists", self.base_url, employee_id);
        http_client::get_exists(&self.http, &url).await
    }

    async fn account_exists(&self, account_id: &str) -> Result<bool, PeopleClientError> {
        let url = format!(
            "{}/api/people/accounts/{}/exists",
            self.base_url, account_id
        );
        http_client::get_exists(&self.http, &url).await
    }
}

/// Permissive test fake: every employee and account id exists.
///
/// Default for guard tests that don't care about people state — they
/// just need a client that satisfies the trait bound and never
/// rejects a reference.
pub struct AlwaysExistsPeople;

#[async_trait]
impl PeopleClient for AlwaysExistsPeople {
    async fn employee_exists(&self, _id: &str) -> Result<bool, PeopleClientError> {
        Ok(true)
    }
    async fn account_exists(&self, _id: &str) -> Result<bool, PeopleClientError> {
        Ok(true)
    }
}

/// Set-backed test fake for [`PeopleClient`].
///
/// Only the explicitly added employee/account ids exist; everything
/// else reports absent. Use to exercise the rejection path of a guard
/// that validates an `actor_id` or `account_id` reference. Build with
/// [`new`](Self::new), then add known ids with
/// [`with_employee`](Self::with_employee) /
/// [`with_account`](Self::with_account).
pub struct FakePeopleClient {
    employees: std::sync::Mutex<std::collections::HashSet<String>>,
    accounts: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl FakePeopleClient {
    /// Empty fake — no employees or accounts exist yet.
    pub fn new() -> Self {
        Self {
            employees: std::sync::Mutex::new(std::collections::HashSet::new()),
            accounts: std::sync::Mutex::new(std::collections::HashSet::new()),
        }
    }

    /// Register a known employee id, so `employee_exists` returns true
    /// for it.
    pub fn with_employee(self, id: impl Into<String>) -> Self {
        self.employees
            .lock()
            .expect("poisoned fake-people mutex")
            .insert(id.into());
        self
    }

    /// Register a known account id, so `account_exists` returns true
    /// for it.
    pub fn with_account(self, id: impl Into<String>) -> Self {
        self.accounts
            .lock()
            .expect("poisoned fake-people mutex")
            .insert(id.into());
        self
    }
}

impl Default for FakePeopleClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PeopleClient for FakePeopleClient {
    async fn employee_exists(&self, id: &str) -> Result<bool, PeopleClientError> {
        Ok(self
            .employees
            .lock()
            .expect("poisoned fake-people mutex")
            .contains(id))
    }
    async fn account_exists(&self, id: &str) -> Result<bool, PeopleClientError> {
        Ok(self
            .accounts
            .lock()
            .expect("poisoned fake-people mutex")
            .contains(id))
    }
}

/// Fail-closed test fake: every call returns
/// [`PeopleClientError::Unreachable`]. Use to exercise the fail-closed
/// path of any guard that depends on people.
pub struct UnreachablePeopleClient;

#[async_trait]
impl PeopleClient for UnreachablePeopleClient {
    async fn employee_exists(&self, _id: &str) -> Result<bool, PeopleClientError> {
        Err(PeopleClientError::Unreachable("test fake".into()))
    }
    async fn account_exists(&self, _id: &str) -> Result<bool, PeopleClientError> {
        Err(PeopleClientError::Unreachable("test fake".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn always_exists_accepts_anything() {
        let c = AlwaysExistsPeople;
        assert!(c.employee_exists("anyone").await.unwrap());
        assert!(c.account_exists("anything").await.unwrap());
    }

    #[tokio::test]
    async fn set_backed_fake_gates_on_known_ids() {
        let c = FakePeopleClient::new()
            .with_employee("emp-007")
            .with_account("account-001");
        assert!(c.employee_exists("emp-007").await.unwrap());
        assert!(!c.employee_exists("emp-999").await.unwrap());
        assert!(c.account_exists("account-001").await.unwrap());
        assert!(!c.account_exists("account-999").await.unwrap());
    }

    #[tokio::test]
    async fn unreachable_fails_every_call() {
        let c = UnreachablePeopleClient;
        assert!(matches!(
            c.employee_exists("emp-1").await.unwrap_err(),
            PeopleClientError::Unreachable(_)
        ));
        assert!(matches!(
            c.account_exists("account-1").await.unwrap_err(),
            PeopleClientError::Unreachable(_)
        ));
    }
}
