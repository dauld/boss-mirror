//! The login door: `X-Boss-User.id` resolves through the alias table
//! before anything downstream reads it.
//!
//! Where a human's login is resolved at the gateway (oidc.rs →
//! `bootstrap_email` → `session.employee_id`), an agent's is resolved
//! HERE, because the pod's doors (`boss-api`, the `boss` CLI) speak to
//! the jobs API directly with the address in the header — there is no
//! gateway session in that path to hold the resolved id. So the jobs
//! API's own edge is the one place every agent write passes.
//!
//! Layer order is the contract: this layer sits OUTSIDE
//! `request_context_middleware`, so the ambient actor is set from the
//! REWRITTEN header, and the `CurrentUser` extractor every handler uses
//! reads the same rewritten header. Nothing downstream has to know an
//! alias existed — which is exactly the property oidc.rs gives humans.
//!
//! [`decide`] is the rule, pure and pinned below; [`resolve_login`] is
//! the axum shell around it.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method};
use axum::middleware::Next;
use axum::response::Response;
use boss_core::publisher::DomainPublisher;
use boss_policy_client::User;

use super::port::AgentsRegistry;

/// The event kind that counts the window: one per WRITE admitted under
/// an address-shaped login no alias maps. Declared in
/// `20260915212644-an-agent-login-resolves-to-its-registered-identity.sql`.
pub const UNRESOLVED_LOGIN: &str = "actor.login.unresolved";

/// The header the extractor and the request-context middleware read.
const HEADER: &str = "x-boss-user";

/// What the door does with one request's identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// The id is an alias: sign the request as the actor it maps to.
    Resolved(String),
    /// An address-shaped login no alias maps, on a write: admit it (the
    /// window is open) and count it.
    Unresolved,
    /// Not a login at all (an employee id, an automation slug, the
    /// anonymous request), a read, or an id already canonical: pass
    /// through untouched, count nothing.
    PassThrough,
}

/// Is this id shaped like a login rather than an identity? An `@` is
/// the whole test — the same test `isHumanActor` on the client uses to
/// call an address a session rather than a person — because every
/// identity the vocabulary already describes (`emp-*`, `automation:*`,
/// `<mode>:<model>`, `agent-*`) is address-free.
fn is_login_shaped(id: &str) -> bool {
    id.contains('@')
}

/// A write is a call whose actor is recorded; reads attribute nothing,
/// which is why boss-cli's identity.rs lets an unnamed read through
/// and refuses an unnamed write. HEAD/OPTIONS ride with GET.
fn is_write(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// The rule. `alias` is what the registry answered for `id`.
pub fn decide(id: &str, alias: Option<String>, method: &Method) -> Resolution {
    match alias {
        Some(actor) if actor != id => Resolution::Resolved(actor),
        Some(_) => Resolution::PassThrough,
        None if is_login_shaped(id) && is_write(method) => Resolution::Unresolved,
        None => Resolution::PassThrough,
    }
}

/// The door's two dependencies: who answers the alias question, and
/// where the count is written. Mount it with
/// `axum::middleware::from_fn_with_state(Arc::new(door), resolve_login)`,
/// applied AFTER (so it runs BEFORE) `request_context_middleware`.
pub struct LoginDoor {
    registry: Arc<dyn AgentsRegistry>,
    publisher: DomainPublisher,
}

impl LoginDoor {
    pub fn new(registry: Arc<dyn AgentsRegistry>, publisher: DomainPublisher) -> Self {
        Self {
            registry,
            publisher,
        }
    }
}

/// The shell: read the header, ask the registry, apply [`decide`].
///
/// A registry that cannot answer (`Err`) is NOT a refusal during the
/// window: the request passes unresolved and the failure is logged at
/// ERROR. Refusing here would let a database blip take every write
/// down with it — the boot-guard lesson of 2026-09-07 — and the window
/// exists precisely so nothing is refused yet. When the next car closes
/// the window, this arm becomes a 503 that names the registry.
pub async fn resolve_login(
    State(door): State<Arc<LoginDoor>>,
    mut req: Request,
    next: Next,
) -> Response {
    let Some(user) = req
        .headers()
        .get(HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| serde_json::from_str::<User>(s).ok())
    else {
        // No header, or one the extractor will 400 on its own: not
        // this door's question.
        return next.run(req).await;
    };
    if user.id == "anonymous" {
        return next.run(req).await;
    }
    let alias = match door.registry.resolve_login(&user.id).await {
        Ok(alias) => alias,
        Err(e) => {
            tracing::error!(
                login = %user.id,
                error = %e,
                "agents registry could not answer; the request passes UNRESOLVED (migration window open)"
            );
            None
        }
    };
    match decide(&user.id, alias, req.method()) {
        Resolution::Resolved(actor) => {
            let resolved = User { id: actor, ..user };
            match serde_json::to_string(&resolved)
                .ok()
                .and_then(|s| HeaderValue::from_str(&s).ok())
            {
                Some(value) => {
                    req.headers_mut().insert(HEADER, value);
                }
                None => {
                    // A User that deserialized re-serializes; this arm
                    // is unreachable in practice, and if it is reached
                    // the request passes as it arrived rather than
                    // being dropped on the floor.
                    tracing::error!(login = %resolved.id, "could not re-serialize the resolved user; passing the request unresolved");
                }
            }
        }
        Resolution::Unresolved => {
            let method = req.method().to_string();
            let path = req.uri().path().to_string();
            tracing::warn!(
                login = %user.id,
                method = %method,
                path = %path,
                "write signed with an address no actor alias maps — admitted (migration window open) and counted as {UNRESOLVED_LOGIN}"
            );
            // The door observed it: the event's actor is the service,
            // and the unresolved login is the SUBJECT in the payload.
            // Stamping the event with the login itself would put the
            // address into `_actor`, which is the defect being counted.
            let stamp = door
                .publisher
                .stamp_with_actor(door.publisher.default_actor())
                .await;
            let event = stamp.event(
                UNRESOLVED_LOGIN,
                serde_json::json!({
                    "login": user.id,
                    "method": method,
                    "path": path,
                }),
            );
            door.publisher.publish(event).await;
        }
        Resolution::PassThrough => {}
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn some(s: &str) -> Option<String> {
        Some(s.to_string())
    }

    /// The alias resolves on every method: reads must see the same
    /// identity writes stamp, or "my steps" would answer for the wrong
    /// actor.
    #[test]
    fn an_alias_resolves_on_reads_and_writes_alike() {
        for m in [
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
        ] {
            assert_eq!(
                decide("claude@algedonic.dev", some("agent-claude"), &m),
                Resolution::Resolved("agent-claude".into()),
                "{m}"
            );
        }
    }

    /// An unmatched address counts on a write and on nothing else.
    #[test]
    fn an_unmatched_address_counts_only_on_a_write() {
        for m in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            assert_eq!(
                decide("nobody@example.test", None, &m),
                Resolution::Unresolved,
                "{m}"
            );
        }
        for m in [Method::GET, Method::HEAD, Method::OPTIONS] {
            assert_eq!(
                decide("nobody@example.test", None, &m),
                Resolution::PassThrough,
                "{m}"
            );
        }
    }

    /// Identities the vocabulary already describes are not logins and
    /// never count, even on writes and even unregistered.
    #[test]
    fn identities_that_are_not_logins_pass_through() {
        for id in [
            "emp-david",
            "automation:train-conductor",
            "rule:bill-approve",
            "claude:opus-5[1m]",
            "agent-claude",
            "operator:unidentified",
            "brewery-sim",
        ] {
            assert_eq!(
                decide(id, None, &Method::PUT),
                Resolution::PassThrough,
                "{id}"
            );
        }
    }

    /// A registry answering with the id itself changes nothing.
    #[test]
    fn a_self_alias_is_a_pass_through() {
        assert_eq!(
            decide("agent-claude", some("agent-claude"), &Method::PUT),
            Resolution::PassThrough
        );
    }
}
