//! In-memory adapter for `MessageRepository`.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::RwLock;

use crate::port::{MessageError, MessageRepository};
use crate::types::{Message, MessageKind};

pub struct InMemoryMessages {
    messages: RwLock<Vec<Message>>,
    recorded: std::sync::Mutex<Vec<boss_core::event::Event>>,
}

impl InMemoryMessages {
    pub fn new(messages: Vec<Message>) -> Self {
        Self {
            messages: RwLock::new(messages),
            recorded: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Events the outbox paths recorded — test visibility (the
    /// in-memory analogue of the Pg adapter's in-tx recording).
    pub fn recorded_events(&self) -> Vec<boss_core::event::Event> {
        self.recorded.lock().map(|v| v.clone()).unwrap_or_default()
    }

    fn record(&self, event: boss_core::event::Event) {
        if let Ok(mut v) = self.recorded.lock() {
            v.push(event);
        }
    }
}

#[async_trait]
impl MessageRepository for InMemoryMessages {
    async fn inbox(
        &self,
        recipient_id: &str,
        include_archived: bool,
    ) -> Result<Vec<Message>, MessageError> {
        let guard = self.messages.read().await;
        let mut msgs: Vec<Message> = guard
            .iter()
            .filter(|m| m.recipient_id == recipient_id)
            .filter(|m| include_archived || m.archived_at.is_none())
            .cloned()
            .collect();
        msgs.sort_by_key(|m| std::cmp::Reverse(m.sent_at));
        Ok(msgs)
    }

    async fn unread_count(
        &self,
        recipient_id: &str,
        kind: Option<&str>,
    ) -> Result<u32, MessageError> {
        let guard = self.messages.read().await;
        let count = guard
            .iter()
            .filter(|m| {
                m.recipient_id == recipient_id && m.read_at.is_none() && m.archived_at.is_none()
            })
            .filter(|m| kind.is_none_or(|k| m.kind.0 == k))
            .count();
        Ok(count as u32)
    }

    async fn message_by_id(&self, id: &str) -> Result<Option<Message>, MessageError> {
        let guard = self.messages.read().await;
        Ok(guard.iter().find(|m| m.id == id).cloned())
    }

    async fn mark_read(
        &self,
        id: &str,
        read_at: DateTime<Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), MessageError> {
        let updated = {
            let mut guard = self.messages.write().await;
            match guard.iter_mut().find(|m| m.id == id) {
                Some(msg) => {
                    msg.read_at = Some(read_at);
                    true
                }
                None => false,
            }
        };
        // Mirrors the Pg gate: a phantom id records nothing.
        if updated {
            self.record(stamp.event(
                crate::events::MESSAGE_READ,
                serde_json::json!({ "id": id, "read_at": read_at }),
            ));
        }
        Ok(())
    }

    async fn send(
        &self,
        msg: &Message,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), MessageError> {
        // Mirrors the Pg ON CONFLICT (id) DO NOTHING collapse: a
        // duplicate id is an idempotent no-op and records nothing.
        let inserted = {
            let mut guard = self.messages.write().await;
            if guard.iter().any(|m| m.id == msg.id) {
                false
            } else {
                guard.push(msg.clone());
                true
            }
        };
        if inserted {
            self.record(stamp.event(
                crate::events::MESSAGE_SENT,
                serde_json::to_value(msg).unwrap_or_else(|_| serde_json::json!({ "id": msg.id })),
            ));
        }
        Ok(())
    }

    async fn delete_message(
        &self,
        id: &str,
        now: DateTime<Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), MessageError> {
        {
            let mut guard = self.messages.write().await;
            let len_before = guard.len();
            guard.retain(|m| m.id != id);
            if guard.len() == len_before {
                return Err(MessageError::NotFound(format!("no message with ID {id}")));
            }
        }
        self.record(stamp.event(
            crate::events::MESSAGE_DELETED,
            serde_json::json!({ "id": id, "deleted_at": now }),
        ));
        Ok(())
    }

    async fn expire_signals_under(
        &self,
        path_prefix: &str,
        now: DateTime<Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<u32, MessageError> {
        let ids: Vec<String> = {
            let mut guard = self.messages.write().await;
            let mut hit = Vec::new();
            for m in guard.iter_mut() {
                let matches = m.entity_ref.as_ref().is_some_and(|e| {
                    e.entity_path
                        .as_deref()
                        .is_some_and(|p| p.starts_with(path_prefix))
                });
                if matches
                    && m.kind.0 == MessageKind::SIGNAL
                    && m.archived_at.is_none()
                    && m.read_at.is_none()
                {
                    m.archived_at = Some(now);
                    hit.push(m.id.clone());
                }
            }
            hit
        };
        // One event per row, mirroring the Pg adapter — a summary
        // event would leave the rebuilder knowing how many moved but
        // not which.
        for id in &ids {
            self.record(stamp.event(
                crate::events::MESSAGE_ARCHIVED,
                serde_json::json!({ "id": id, "archived_at": now, "reason": "entity-past-relevancy" }),
            ));
        }
        Ok(ids.len() as u32)
    }

    async fn expire_notices_under(
        &self,
        path_prefix: &str,
        id_prefix: &str,
        now: DateTime<Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<u32, MessageError> {
        let ids: Vec<String> = {
            let mut guard = self.messages.write().await;
            let mut hit = Vec::new();
            for m in guard.iter_mut() {
                let under = m.entity_ref.as_ref().is_some_and(|e| {
                    e.entity_path
                        .as_deref()
                        .is_some_and(|p| p.starts_with(path_prefix))
                });
                if under
                    && m.id.starts_with(id_prefix)
                    && m.archived_at.is_none()
                    && m.read_at.is_none()
                {
                    m.archived_at = Some(now);
                    hit.push(m.id.clone());
                }
            }
            hit
        };
        for id in &ids {
            self.record(stamp.event(
                crate::events::MESSAGE_ARCHIVED,
                serde_json::json!({ "id": id, "archived_at": now, "reason": "entity-past-relevancy" }),
            ));
        }
        Ok(ids.len() as u32)
    }

    async fn archive_message(
        &self,
        id: &str,
        now: DateTime<Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), MessageError> {
        // Mirrors the Pg `archived_at IS NULL` guard (backlog 9bda9726):
        // a repeat archive is Ok and records nothing, and `kind` stays.
        {
            let mut guard = self.messages.write().await;
            match guard.iter_mut().find(|m| m.id == id) {
                Some(msg) if msg.archived_at.is_some() => return Ok(()),
                Some(msg) => msg.archived_at = Some(now),
                None => return Err(MessageError::NotFound(format!("no message with ID {id}"))),
            }
        }
        self.record(stamp.event(
            crate::events::MESSAGE_ARCHIVED,
            serde_json::json!({ "id": id, "archived_at": now }),
        ));
        Ok(())
    }

    async fn thread(&self, message_id: &str) -> Result<Vec<Message>, MessageError> {
        let guard = self.messages.read().await;
        let mut thread: Vec<Message> = guard
            .iter()
            .filter(|m| m.id == message_id || m.reply_to.as_deref() == Some(message_id))
            .cloned()
            .collect();
        thread.sort_by_key(|m| m.sent_at);
        Ok(thread)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;

    fn test_message(id: &str, recipient: &str, hours_ago: i64, read: bool) -> Message {
        let sent_at = Utc::now() - chrono::Duration::hours(hours_ago);
        Message {
            id: id.to_string(),
            sender_id: "sender-001".to_string(),
            recipient_id: recipient.to_string(),
            subject: format!("Subject {id}"),
            body: format!("Body {id}"),
            entity_ref: None,
            kind: MessageKind::DIRECT.into(),
            sent_at,
            read_at: if read { Some(Utc::now()) } else { None },
            reply_to: None,
            archived_at: None,
        }
    }

    fn test_stamp() -> boss_core::publisher::EventStamp {
        boss_core::publisher::EventStamp::new(
            "messages",
            boss_core::actor::ActorId::Automation("test".into()),
        )
    }

    fn test_repo() -> InMemoryMessages {
        InMemoryMessages::new(vec![
            test_message("msg-001", "emp-001", 3, false),
            test_message("msg-002", "emp-001", 1, false),
            test_message("msg-003", "emp-001", 5, true),
            test_message("msg-004", "emp-002", 2, false),
        ])
    }

    #[tokio::test]
    async fn inbox_returns_messages_for_recipient() {
        let repo = test_repo();
        let inbox = repo.inbox("emp-001", false).await.unwrap();
        assert_eq!(inbox.len(), 3);
        assert!(inbox.iter().all(|m| m.recipient_id == "emp-001"));
    }

    #[tokio::test]
    async fn inbox_sorted_by_sent_at_desc() {
        let repo = test_repo();
        let inbox = repo.inbox("emp-001", false).await.unwrap();
        for pair in inbox.windows(2) {
            assert!(pair[0].sent_at >= pair[1].sent_at);
        }
    }

    #[tokio::test]
    async fn inbox_empty_for_unknown_recipient() {
        let repo = test_repo();
        let inbox = repo.inbox("emp-999", false).await.unwrap();
        assert!(inbox.is_empty());
    }

    #[tokio::test]
    async fn unread_count_correct() {
        let repo = test_repo();
        let count = repo.unread_count("emp-001", None).await.unwrap();
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn message_by_id_found() {
        let repo = test_repo();
        let msg = repo.message_by_id("msg-001").await.unwrap();
        assert!(msg.is_some());
        assert_eq!(msg.unwrap().id, "msg-001");
    }

    #[tokio::test]
    async fn message_by_id_not_found() {
        let repo = test_repo();
        assert!(repo.message_by_id("msg-999").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn mark_read_sets_read_at() {
        let repo = test_repo();
        assert!(
            repo.message_by_id("msg-001")
                .await
                .unwrap()
                .unwrap()
                .read_at
                .is_none()
        );
        repo.mark_read("msg-001", Utc::now(), &test_stamp())
            .await
            .unwrap();
        let msg = repo.message_by_id("msg-001").await.unwrap().unwrap();
        assert!(msg.read_at.is_some());
    }

    #[tokio::test]
    async fn mark_read_reduces_unread_count() {
        let repo = test_repo();
        let before = repo.unread_count("emp-001", None).await.unwrap();
        repo.mark_read("msg-001", Utc::now(), &test_stamp())
            .await
            .unwrap();
        let after = repo.unread_count("emp-001", None).await.unwrap();
        assert_eq!(after, before - 1);
    }

    #[tokio::test]
    async fn delete_message_removes_it() {
        let repo = test_repo();
        assert!(repo.message_by_id("msg-001").await.unwrap().is_some());
        repo.delete_message("msg-001", Utc::now(), &test_stamp())
            .await
            .unwrap();
        assert!(repo.message_by_id("msg-001").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn delete_message_not_found() {
        let repo = test_repo();
        let err = repo
            .delete_message("msg-999", Utc::now(), &test_stamp())
            .await
            .unwrap_err();
        assert!(matches!(err, MessageError::NotFound(_)));
    }

    /// Backlog 9bda9726: archiving is a state beside `kind`, not a
    /// value of it. It used to overwrite `kind` with `archived`, so the
    /// projection forgot the direct-vs-signal fact the needs-you filter
    /// is built on.
    #[tokio::test]
    async fn an_archived_direct_still_reads_as_direct() {
        let repo = test_repo();
        let at = Utc::now();
        repo.archive_message("msg-001", at, &test_stamp())
            .await
            .unwrap();
        let msg = repo.message_by_id("msg-001").await.unwrap().unwrap();
        assert_eq!(msg.kind.as_str(), MessageKind::DIRECT);
        assert_eq!(msg.archived_at, Some(at));
    }

    /// Backlog 9bda9726 (idempotence): a second archive of an archived
    /// message is a no-op — it answers Ok, records no second event, and
    /// leaves the first archive's time standing.
    #[tokio::test]
    async fn archiving_twice_records_one_event() {
        let repo = test_repo();
        let first = Utc::now();
        repo.archive_message("msg-001", first, &test_stamp())
            .await
            .unwrap();
        repo.archive_message(
            "msg-001",
            first + chrono::Duration::minutes(5),
            &test_stamp(),
        )
        .await
        .unwrap();
        let archived = repo
            .recorded_events()
            .into_iter()
            .filter(|e| e.kind == crate::events::MESSAGE_ARCHIVED)
            .count();
        assert_eq!(archived, 1, "a repeat archive records nothing");
        let msg = repo.message_by_id("msg-001").await.unwrap().unwrap();
        assert_eq!(msg.archived_at, Some(first), "the first archive stands");
    }

    #[tokio::test]
    async fn archive_message_not_found() {
        let repo = test_repo();
        let err = repo
            .archive_message("msg-999", Utc::now(), &test_stamp())
            .await
            .unwrap_err();
        assert!(matches!(err, MessageError::NotFound(_)));
    }
}

#[cfg(test)]
mod expiry_tests {
    use super::*;
    use crate::types::*;

    fn msg(id: &str, kind: &str, path: Option<&str>, read: bool) -> Message {
        Message {
            id: id.to_string(),
            sender_id: "automation:dispatcher".to_string(),
            recipient_id: "emp-001".to_string(),
            subject: format!("S {id}"),
            body: "b".to_string(),
            entity_ref: path.map(|p| EntityRef {
                entity_type: "job".to_string(),
                entity_id: "job-1".to_string(),
                entity_path: Some(p.to_string()),
            }),
            kind: kind.into(),
            sent_at: Utc::now(),
            read_at: if read { Some(Utc::now()) } else { None },
            reply_to: None,
            archived_at: None,
        }
    }

    fn stamp() -> boss_core::publisher::EventStamp {
        boss_core::publisher::EventStamp::new(
            "messages",
            boss_core::actor::ActorId::Automation("test".into()),
        )
    }

    async fn kind_of(repo: &InMemoryMessages, id: &str) -> String {
        repo.message_by_id(id).await.unwrap().unwrap().kind.0
    }

    async fn archived(repo: &InMemoryMessages, id: &str) -> bool {
        repo.message_by_id(id)
            .await
            .unwrap()
            .unwrap()
            .archived_at
            .is_some()
    }

    /// The three narrowings the port doc promises, asserted together
    /// because the value of this expiry is entirely in what it does
    /// NOT touch. A version that cleared directs would delete the one
    /// category the inbox's needs-you filter is built on.
    #[tokio::test]
    async fn expires_only_unread_signals_under_the_prefix() {
        let repo = InMemoryMessages::new(vec![
            msg("stale-job", MessageKind::SIGNAL, Some("/jobs/job-1"), false),
            msg(
                "stale-step",
                MessageKind::SIGNAL,
                Some("/jobs/job-1/steps/s1"),
                false,
            ),
            msg("a-direct", MessageKind::DIRECT, Some("/jobs/job-1"), false),
            msg(
                "already-read",
                MessageKind::SIGNAL,
                Some("/jobs/job-1"),
                true,
            ),
            msg("other-job", MessageKind::SIGNAL, Some("/jobs/job-2"), false),
            msg("no-entity", MessageKind::SIGNAL, None, false),
        ]);

        let n = repo
            .expire_signals_under("/jobs/job-1", Utc::now(), &stamp())
            .await
            .unwrap();
        assert_eq!(n, 2, "both shapes under the prefix, and nothing else");

        // Archived is a state beside kind (backlog 9bda9726): the two
        // that moved keep reading as signals.
        for id in ["stale-job", "stale-step"] {
            assert!(archived(&repo, id).await, "{id} expired");
            assert_eq!(kind_of(&repo, id).await, MessageKind::SIGNAL, "{id}");
        }
        assert!(
            !archived(&repo, "a-direct").await,
            "a direct is addressed to a person and does not expire with the job"
        );
        assert!(
            !archived(&repo, "already-read").await,
            "a read message already did its job"
        );
        assert!(
            !archived(&repo, "other-job").await,
            "the prefix must not leak across jobs"
        );
        assert!(
            !archived(&repo, "no-entity").await,
            "a message about nothing cannot be past relevancy"
        );
    }

    /// `/jobs/job-1` must not match `/jobs/job-10`.
    #[tokio::test]
    async fn the_prefix_does_not_match_a_longer_id() {
        let repo = InMemoryMessages::new(vec![msg(
            "sibling",
            MessageKind::SIGNAL,
            Some("/jobs/job-10"),
            false,
        )]);
        let n = repo
            .expire_signals_under("/jobs/job-1/", Utc::now(), &stamp())
            .await
            .unwrap();
        assert_eq!(n, 0);
    }

    /// A notice the machine sent about a STEP stops being true when the
    /// step ends, whatever its kind (backlog 0b2bac00: 83 of David's 90
    /// direct notices pointed at completed steps, and every one of them
    /// counted in his "waiting on you" headline and badge). The id
    /// prefix is what makes it a notice: the notifier's ids are
    /// `notify:{step}:{recipient}`, and nothing else under the step is
    /// touched — a person's direct about the same step, the `done:`
    /// announcement the step's END sends, a read notice, a sibling step.
    #[tokio::test]
    async fn expires_only_unread_notices_by_id_under_the_prefix() {
        let step = "/jobs/job-1/steps/s1";
        let mut from_a_person = msg("msg-human", MessageKind::DIRECT, Some(step), false);
        from_a_person.sender_id = "emp-colleague".to_string();
        let repo = InMemoryMessages::new(vec![
            msg("notify:s1:emp-001", MessageKind::DIRECT, Some(step), false),
            // An older role-fallback notice, sent as a signal: the same
            // notice, and just as finished.
            msg("notify:s1:emp-002", MessageKind::SIGNAL, Some(step), false),
            from_a_person,
            msg("done:s1:emp-001", MessageKind::DIRECT, Some(step), false),
            msg("notify:s1:emp-003", MessageKind::DIRECT, Some(step), true),
            msg(
                "notify:s2:emp-001",
                MessageKind::DIRECT,
                Some("/jobs/job-1/steps/s2"),
                false,
            ),
        ]);

        let n = repo
            .expire_notices_under(step, "notify:", Utc::now(), &stamp())
            .await
            .unwrap();
        assert_eq!(n, 2, "the step's two unread notices, and nothing else");

        assert!(archived(&repo, "notify:s1:emp-001").await);
        assert_eq!(
            kind_of(&repo, "notify:s1:emp-001").await,
            MessageKind::DIRECT,
            "a retired notice still reads as the direct it was sent as"
        );
        assert!(archived(&repo, "notify:s1:emp-002").await);
        assert_eq!(
            kind_of(&repo, "notify:s1:emp-002").await,
            MessageKind::SIGNAL
        );
        assert!(
            !archived(&repo, "msg-human").await,
            "a person asking about the step does not stop asking because it ended"
        );
        assert!(
            !archived(&repo, "done:s1:emp-001").await,
            "the announcement that the step ended is not a notice that it is waiting"
        );
        assert!(
            !archived(&repo, "notify:s1:emp-003").await,
            "a read message already did its job"
        );
        assert!(
            !archived(&repo, "notify:s2:emp-001").await,
            "the prefix must not leak across steps"
        );
        assert_eq!(
            repo.unread_count("emp-001", Some(MessageKind::DIRECT))
                .await
                .unwrap(),
            3,
            "the retired notice leaves the unread-direct count the badge reads \
             (the person's, the done announcement's and the sibling step's stay)"
        );
        assert_eq!(repo.recorded_events().len(), 2, "one event per row moved");
    }

    #[tokio::test]
    async fn records_one_event_per_archived_row() {
        let repo = InMemoryMessages::new(vec![
            msg("a", MessageKind::SIGNAL, Some("/jobs/job-1"), false),
            msg("b", MessageKind::SIGNAL, Some("/jobs/job-1"), false),
        ]);
        repo.expire_signals_under("/jobs/job-1", Utc::now(), &stamp())
            .await
            .unwrap();
        let events = repo.recorded_events();
        assert_eq!(
            events.len(),
            2,
            "the rebuilder needs to know WHICH rows moved"
        );
    }
}
