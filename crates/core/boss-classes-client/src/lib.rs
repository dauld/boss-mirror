//! HTTP client port for reaching the `boss-classes` registry service.
//!
//! Other services need to validate Class codes (e.g. `employees.role`,
//! `accounts.account_type`) against the registry before accepting
//! writes, and to load tag-driven role lists at startup (which roles
//! are executives, which carry global read). Trait + reqwest adapter
//! live here so any consumer (boss-people, boss-accounts, boss-jobs,
//! …) can call the same canonical contract.

use async_trait::async_trait;
use boss_core::http_client::{self, HttpClientError, ServiceLabel};
use boss_core::primitives::{Class, ClassRef};
use percent_encoding::{AsciiSet, utf8_percent_encode};

/// RFC 3986 "unreserved" set — everything EXCEPT `A-Z a-z 0-9 - . _ ~`
/// is percent-encoded. A Class code goes into a URL path segment, so any
/// code containing `/` (e.g. the `1/2-bbl-keg` package_unit), a space,
/// `?`, or `#` would otherwise split or truncate the path. Hyphenated
/// codes (`grain-supplier`, `past-due`) contain only unreserved chars,
/// so they pass through unchanged — this is a no-op for every existing
/// caller and only rescues codes with reserved characters.
const PATH_SEGMENT: &AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

fn seg(s: &str) -> impl std::fmt::Display + '_ {
    utf8_percent_encode(s, PATH_SEGMENT)
}

/// Fetch employee Classes from the registry and seed
/// [`boss_core::roles`]' executive cache from the rows whose
/// `metadata.is_executive` is true. Returns the count seeded so
/// callers can log a startup banner.
///
/// Tolerant of registry failures: on transport error the cache stays
/// uninitialised and `is_executive` returns false for every role
/// (platform-admin + audit-readonly still grant global read). This
/// matches the broader rollout posture — services boot even when a
/// downstream registry is briefly unreachable.
pub async fn seed_executive_role_cache(
    client: &dyn ClassesClient,
) -> Result<usize, ClassesClientError> {
    let classes = client.list_for_subject_kind("employee").await?;
    let codes = executive_role_codes(&classes);
    let count = codes.len();
    boss_core::roles::init_executive_roles(codes);
    Ok(count)
}

/// Filter a class list to codes whose `metadata.is_executive` is true.
/// Pairs with [`boss_core::roles::init_executive_roles`]: services
/// call `client.list_for_subject_kind("employee")` at startup, pass
/// the result here, and feed the returned codes into the cache.
///
/// `metadata` is JSON; a missing key or non-bool value is treated as
/// `false`. Retired Classes are excluded so an executive role that
/// gets retired stops carrying global read on the next service boot.
pub fn executive_role_codes(classes: &[Class]) -> Vec<String> {
    classes
        .iter()
        .filter(|c| c.retired_at.is_none())
        .filter(|c| {
            c.metadata
                .get("is_executive")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .map(|c| c.code.clone())
        .collect()
}

/// Sister of [`seed_executive_role_cache`] for the broad-account-access
/// role set. Fetches employee Classes, filters by
/// `metadata.broad_account_access`, and seeds the in-process cache.
/// Services that don't seed fall back to the hardcoded union at
/// `boss_core::roles::DEFAULT_BROAD_ACCOUNT_ACCESS_ROLES`.
pub async fn seed_broad_account_access_role_cache(
    client: &dyn ClassesClient,
) -> Result<usize, ClassesClientError> {
    let classes = client.list_for_subject_kind("employee").await?;
    let codes = broad_account_access_role_codes(&classes);
    let count = codes.len();
    boss_core::roles::init_broad_account_access_roles(codes);
    Ok(count)
}

/// Filter a class list to codes whose `metadata.broad_account_access`
/// is true. Same `metadata` shape as [`executive_role_codes`].
pub fn broad_account_access_role_codes(classes: &[Class]) -> Vec<String> {
    classes
        .iter()
        .filter(|c| c.retired_at.is_none())
        .filter(|c| {
            c.metadata
                .get("broad_account_access")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .map(|c| c.code.clone())
        .collect()
}

/// Service-name marker for the shared [`HttpClientError`]. Keeps the
/// `Display` text reading `"classes service unreachable: …"`.
#[derive(Debug)]
pub struct Classes;
impl ServiceLabel for Classes {
    const NAME: &'static str = "classes";
}

/// Transport error for the Classes client. Alias of the shared
/// [`HttpClientError`] so existing constructors and matches keep
/// compiling.
pub type ClassesClientError = HttpClientError<Classes>;

/// Existence + listing questions other services ask the Class
/// registry. v1 exposes:
///
/// - `class_exists` — hot-path validation that replaces closed-enum
///   CHECK constraints on writes.
/// - `list_for_subject_kind` — startup-cache primitive for tag-driven
///   role lookups (e.g. which `employee` Classes are executive).
#[async_trait]
pub trait ClassesClient: Send + Sync {
    /// True iff a non-retired Class with the given key exists in the
    /// registry.
    async fn class_exists(&self, class_ref: &ClassRef) -> Result<bool, ClassesClientError>;

    /// All non-retired Classes for a `subject_kind`. Used at service
    /// startup to seed in-process caches that read metadata tags
    /// (e.g. `metadata.is_executive`).
    async fn list_for_subject_kind(
        &self,
        subject_kind: &str,
    ) -> Result<Vec<Class>, ClassesClientError>;

    /// True iff a non-retired Class with the given key exists AND
    /// classifies `member_attribute` — the column on the Subject its
    /// code is a value of.
    ///
    /// WHY `class_exists` IS NOT ENOUGH. One subject_kind can carry
    /// several taxonomies, told apart only by `member_attribute`: the
    /// `employee` drawer held 22 live codes across role, department,
    /// status and employment_type on 2026-09-23 (backlog a45ab09d). An
    /// axis-blind check accepts any of them for any column, so an
    /// employee's `role` could be written as `terminated` and its
    /// `department` as `platform-admin`, and both would pass. Ask this
    /// when the value is one column's; ask `class_exists` only when the
    /// kind has a single taxonomy.
    ///
    /// Answered from `list_for_subject_kind`, which the registry already
    /// serves, rather than a new route, so no server has to move first.
    async fn class_exists_on(
        &self,
        class_ref: &ClassRef,
        member_attribute: &str,
    ) -> Result<bool, ClassesClientError> {
        Ok(self
            .list_for_subject_kind(&class_ref.subject_kind)
            .await?
            .iter()
            .any(|c| {
                c.code == class_ref.code
                    && c.retired_at.is_none()
                    && c.member_attribute.as_deref() == Some(member_attribute)
            }))
    }
}

/// Production `ClassesClient` that calls the boss-classes HTTP API
/// over reqwest. 5-second timeout per call so an unresponsive
/// registry can't wedge a write indefinitely.
pub struct ReqwestClassesClient {
    base_url: String,
    http: reqwest::Client,
}

impl ReqwestClassesClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        let (base_url, http) = http_client::base(base_url);
        Self { base_url, http }
    }
}

#[async_trait]
impl ClassesClient for ReqwestClassesClient {
    async fn class_exists(&self, class_ref: &ClassRef) -> Result<bool, ClassesClientError> {
        let url = format!(
            "{}/api/classes/{}/{}/exists",
            self.base_url,
            seg(&class_ref.subject_kind),
            seg(&class_ref.code)
        );
        http_client::get_exists(&self.http, &url).await
    }

    async fn list_for_subject_kind(
        &self,
        subject_kind: &str,
    ) -> Result<Vec<Class>, ClassesClientError> {
        let url = format!("{}/api/classes?subject_kind={subject_kind}", self.base_url);
        http_client::get_json(&self.http, &url).await
    }
}

/// Test fake — accepts a fixed allow-list of `(subject_kind, code)`
/// pairs and optionally returns a fixture list. Use
/// `FakeClassesClient::permissive()` to accept everything,
/// `FakeClassesClient::with(...)` to gate to specific codes, or
/// `FakeClassesClient::with_classes(...)` to drive `list_for_subject_kind`.
pub struct FakeClassesClient {
    permissive: bool,
    allowed: Vec<ClassRef>,
    classes: Vec<Class>,
}

impl FakeClassesClient {
    /// Accept any `class_exists` query. Use in tests that don't care
    /// about registry state.
    pub fn permissive() -> Self {
        Self {
            permissive: true,
            allowed: Vec::new(),
            classes: Vec::new(),
        }
    }

    /// Only accept queries whose `class_ref` is in `allowed`.
    pub fn with(allowed: Vec<ClassRef>) -> Self {
        Self {
            permissive: false,
            allowed,
            classes: Vec::new(),
        }
    }

    /// Drive `list_for_subject_kind` with a fixture vector. Use to
    /// pin executive-role tagging in service-startup tests.
    pub fn with_classes(classes: Vec<Class>) -> Self {
        Self {
            permissive: true,
            allowed: Vec::new(),
            classes,
        }
    }
}

#[async_trait]
impl ClassesClient for FakeClassesClient {
    async fn class_exists(&self, class_ref: &ClassRef) -> Result<bool, ClassesClientError> {
        if self.permissive {
            return Ok(true);
        }
        Ok(self.allowed.iter().any(|c| c == class_ref))
    }

    async fn list_for_subject_kind(
        &self,
        subject_kind: &str,
    ) -> Result<Vec<Class>, ClassesClientError> {
        Ok(self
            .classes
            .iter()
            .filter(|c| c.subject_kind == subject_kind)
            .cloned()
            .collect())
    }

    /// With a fixture, the real axis-aware answer. Without one there is
    /// no axis to consult, so the fake answers exactly as
    /// `class_exists` does — every `permissive()` / `with(...)` test
    /// written before the axis mattered keeps meaning what it meant.
    async fn class_exists_on(
        &self,
        class_ref: &ClassRef,
        member_attribute: &str,
    ) -> Result<bool, ClassesClientError> {
        if self.classes.is_empty() {
            return self.class_exists(class_ref).await;
        }
        Ok(self.classes.iter().any(|c| {
            c.subject_kind == class_ref.subject_kind
                && c.code == class_ref.code
                && c.retired_at.is_none()
                && c.member_attribute.as_deref() == Some(member_attribute)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_segment_encodes_reserved_chars_only() {
        // Slash-bearing code (a product package_unit) must encode, or it
        // splits the `/exists` path — the products-gate bug.
        assert_eq!(seg("1/2-bbl-keg").to_string(), "1%2F2-bbl-keg");
        // Unreserved codes (the common case) pass through untouched, so
        // this is a no-op for every existing caller.
        assert_eq!(seg("grain-supplier").to_string(), "grain-supplier");
        assert_eq!(seg("past-due").to_string(), "past-due");
        assert_eq!(seg("beer").to_string(), "beer");
    }

    #[tokio::test]
    async fn permissive_fake_accepts_anything() {
        let c = FakeClassesClient::permissive();
        assert!(
            c.class_exists(&ClassRef::new("employee", "ceo"))
                .await
                .unwrap()
        );
        assert!(
            c.class_exists(&ClassRef::new("anything", "totally-bogus"))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn restricted_fake_gates_on_allow_list() {
        let c = FakeClassesClient::with(vec![
            ClassRef::new("employee", "ceo"),
            ClassRef::new("employee", "service-tech"),
        ]);
        assert!(
            c.class_exists(&ClassRef::new("employee", "ceo"))
                .await
                .unwrap()
        );
        assert!(
            !c.class_exists(&ClassRef::new("employee", "no-such"))
                .await
                .unwrap()
        );
        assert!(
            !c.class_exists(&ClassRef::new("account", "ceo"))
                .await
                .unwrap(),
            "subject_kind matters too"
        );
    }

    fn class(kind: &str, code: &str, metadata: serde_json::Value, retired: bool) -> Class {
        Class {
            subject_kind: kind.into(),
            code: code.into(),
            display_name: code.into(),
            parent_code: None,
            member_attribute: Some("role".into()),
            metadata,
            sort_order: 0,
            retired_at: if retired {
                Some(chrono::Utc::now())
            } else {
                None
            },
        }
    }

    #[test]
    fn executive_role_codes_filters_on_metadata_flag() {
        let classes = vec![
            class(
                "employee",
                "ceo",
                serde_json::json!({ "is_executive": true }),
                false,
            ),
            class(
                "employee",
                "service-tech",
                serde_json::json!({ "is_executive": false }),
                false,
            ),
            class(
                "employee",
                "head-of-sales",
                serde_json::json!({ "department": "sales", "is_executive": true }),
                false,
            ),
            class("employee", "no-metadata", serde_json::Value::Null, false),
            class(
                "employee",
                "retired-cto",
                serde_json::json!({ "is_executive": true }),
                true,
            ),
        ];
        let codes = executive_role_codes(&classes);
        assert_eq!(codes, vec!["ceo".to_string(), "head-of-sales".to_string()]);
    }

    #[tokio::test]
    async fn fixture_list_filters_by_subject_kind() {
        let make = |kind: &str, code: &str| Class {
            subject_kind: kind.into(),
            code: code.into(),
            display_name: code.into(),
            parent_code: None,
            member_attribute: Some("role".into()),
            metadata: serde_json::Value::Null,
            sort_order: 0,
            retired_at: None,
        };
        let c = FakeClassesClient::with_classes(vec![
            make("employee", "ceo"),
            make("employee", "service-tech"),
            make("account", "distributor"),
        ]);
        let employee = c.list_for_subject_kind("employee").await.unwrap();
        assert_eq!(employee.len(), 2);
        let account = c.list_for_subject_kind("account").await.unwrap();
        assert_eq!(account.len(), 1);
        assert_eq!(account[0].code, "distributor");
    }

    /// The `employee` drawer holds four taxonomies under one
    /// subject_kind (backlog a45ab09d: role, department, status,
    /// employment_type — 22 live codes, 2026-09-23), and `member_attribute`
    /// is what tells them apart. A code asked for on the wrong axis is
    /// not a member of that axis, however active its row is.
    #[tokio::test]
    async fn a_class_exists_only_on_its_own_axis() {
        let on = |code: &str, attribute: &str, retired: bool| Class {
            member_attribute: Some(attribute.into()),
            ..class("employee", code, serde_json::Value::Null, retired)
        };
        let c = FakeClassesClient::with_classes(vec![
            on("platform-admin", "role", false),
            on("terminated", "status", false),
            on("it", "department", false),
            on("ceo", "role", true),
        ]);
        let emp = |code: &str| ClassRef::new("employee", code);

        assert!(
            c.class_exists_on(&emp("platform-admin"), "role")
                .await
                .unwrap()
        );
        assert!(
            c.class_exists_on(&emp("terminated"), "status")
                .await
                .unwrap()
        );
        assert!(
            !c.class_exists_on(&emp("terminated"), "role").await.unwrap(),
            "a status is not a role"
        );
        assert!(
            !c.class_exists_on(&emp("it"), "status").await.unwrap(),
            "a department is not a status"
        );
        assert!(
            !c.class_exists_on(&emp("ceo"), "role").await.unwrap(),
            "a retired row is not a member of anything"
        );
        assert!(
            !c.class_exists_on(&ClassRef::new("account", "platform-admin"), "role")
                .await
                .unwrap(),
            "subject_kind matters too"
        );
    }

    /// A fake with no fixture has no axis to consult, so it answers the
    /// axis-blind question it was built for — which keeps every
    /// existing `permissive()` / `with(...)` caller meaning what it did.
    #[tokio::test]
    async fn a_fake_without_a_fixture_answers_as_class_exists() {
        let permissive = FakeClassesClient::permissive();
        assert!(
            permissive
                .class_exists_on(&ClassRef::new("employee", "anything"), "role")
                .await
                .unwrap()
        );
        let gated = FakeClassesClient::with(vec![ClassRef::new("employee", "ceo")]);
        assert!(
            gated
                .class_exists_on(&ClassRef::new("employee", "ceo"), "role")
                .await
                .unwrap()
        );
        assert!(
            !gated
                .class_exists_on(&ClassRef::new("employee", "cto"), "role")
                .await
                .unwrap()
        );
    }
}
