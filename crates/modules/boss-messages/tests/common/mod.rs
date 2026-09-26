#![allow(dead_code)] // tests/common/ helpers used selectively across test files

//! Shared test scaffolding for the messages crate.
//!
//! Provides:
//! - `MessageTestApp` builder that wires InMemoryMessages + HTTP router
//!   (publisher `None` — outbox phase 2: adapters record events in the
//!   domain write, so the in-memory repo's `recorded_events()` is the
//!   assertion surface, not a bus)
//! - `message_fixture()` helper that builds a valid Message

use std::sync::Arc;

use axum::Router;
use boss_messages::http::{MessageApiState, router};
use boss_messages::in_memory::InMemoryMessages;
use boss_messages::types::{Message, MessageKind};
use chrono::Utc;

/// Fully wired messages service for tests.
pub struct MessageTestApp {
    pub router: Router,
    pub messages: Arc<InMemoryMessages>,
}

impl MessageTestApp {
    /// Build a fresh test app with no messages.
    pub fn new() -> Self {
        Self::with_messages(vec![])
    }

    /// Build a test app pre-populated with the given messages.
    pub fn with_messages(messages: Vec<Message>) -> Self {
        let repo = Arc::new(InMemoryMessages::new(messages));
        let state = MessageApiState {
            messages: repo.clone(),
            publisher: None,
            clock: Arc::new(boss_clock_client::WallClockClient),
            classes_client: None,
        };
        let router = router(state);
        Self {
            router,
            messages: repo,
        }
    }

    /// Assert exactly-one recorded event of `kind` and return it.
    pub fn assert_recorded(&self, kind: &str) -> boss_core::event::Event {
        let matches: Vec<_> = self
            .messages
            .recorded_events()
            .into_iter()
            .filter(|e| e.kind == kind)
            .collect();
        assert_eq!(
            matches.len(),
            1,
            "expected exactly one recorded `{kind}` event, got {}",
            matches.len()
        );
        matches.into_iter().next().unwrap()
    }

    /// Assert no recorded event of `kind`.
    pub fn assert_not_recorded(&self, kind: &str) {
        assert!(
            !self
                .messages
                .recorded_events()
                .iter()
                .any(|e| e.kind == kind),
            "expected no recorded `{kind}` event"
        );
    }
}

/// The `x-boss-user` of a signed-in person, `id`, at user tier. The
/// doors that act on one message let its owner through (backlog
/// 4dd10336); the role is not `guest`, which is not an identity.
pub fn signed_in(id: &str) -> String {
    serde_json::json!({ "id": id, "role": "employee", "access_tier": "user" }).to_string()
}

/// The `x-boss-user` of an operator-tier automation — what a caller
/// that acts on messages it does not own presents.
pub fn operator() -> String {
    serde_json::json!({
        "id": "automation:messages-test",
        "role": "platform-admin",
        "access_tier": "operator",
    })
    .to_string()
}

/// Build a valid Message with sensible defaults.
pub fn message_fixture(id: &str) -> Message {
    Message {
        id: id.to_string(),
        sender_id: "emp-sender".to_string(),
        recipient_id: "emp-recipient".to_string(),
        subject: format!("Subject {id}"),
        body: format!("Body for {id}"),
        entity_ref: None,
        kind: MessageKind::DIRECT.into(),
        sent_at: Utc::now(),
        read_at: None,
        reply_to: None,
        archived_at: None,
    }
}
