//! The invoice status transitions, one contract run against BOTH
//! adapters (backlog 203ef806).
//!
//! Three verbs move an invoice — mark paid, mark past-due, write off —
//! and until this pin each adapter decided for itself which invoices
//! they accept. Pg's past-due flip had no guard at all, so a paid or
//! written-off invoice could be flipped back to past-due and re-enter
//! the receivable (a conservation break: the money was already out of
//! 1100 A/R). Pg wrote off only outstanding|past-due while the
//! in-memory adapter refused only paid, and the in-memory paid and
//! past-due verbs changed nothing at all.
//!
//! The table below is written out as data, deliberately NOT derived
//! from `InvoiceStatus::transition_to`: a contract computed from the
//! rule it checks proves nothing. Every cell is run on both adapters,
//! and each observes three things — the verb's answer, the status the
//! invoice is left in, and how many of the verb's events were recorded
//! (one on a flip; none on a redelivery or a refusal).

use boss_commerce::port::{CommerceError, CommerceRepository};
use boss_commerce::types::*;
use boss_commerce::{InMemoryCommerce, PgCommerce};
use boss_core::publisher::EventStamp;
use boss_testing::TestDb;
use chrono::NaiveDate;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    Paid,
    PastDue,
    WrittenOff,
}

impl Verb {
    const ALL: [Verb; 3] = [Verb::Paid, Verb::PastDue, Verb::WrittenOff];

    fn target(self) -> &'static str {
        match self {
            Verb::Paid => InvoiceStatus::PAID,
            Verb::PastDue => InvoiceStatus::PAST_DUE,
            Verb::WrittenOff => InvoiceStatus::WRITTEN_OFF,
        }
    }

    fn event_kind(self) -> &'static str {
        match self {
            Verb::Paid => boss_commerce::events::INVOICE_PAID,
            Verb::PastDue => boss_commerce::events::INVOICE_PAST_DUE,
            Verb::WrittenOff => boss_commerce::events::INVOICE_WRITTEN_OFF,
        }
    }

    async fn apply<R: CommerceRepository>(self, repo: &R, id: &str) -> Result<(), CommerceError> {
        let stamp = stamp();
        match self {
            Verb::Paid => {
                repo.mark_invoice_paid_at(id, NaiveDate::from_ymd_opt(2025, 6, 1).unwrap(), &stamp)
                    .await
            }
            Verb::PastDue => repo.mark_invoice_past_due(id, &stamp).await,
            Verb::WrittenOff => repo.mark_invoice_written_off(id, &stamp).await.map(|_| ()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// The invoice moves to the target and one event records.
    Flip,
    /// Already at the target: a redelivered drive converges, Ok, no event.
    Already,
    /// Refused by name: a Conflict naming both statuses, nothing changes.
    Refused,
}

/// A tenant's own status — still owed, because it is not paid or
/// written off, so every verb may move it.
const TENANT_OWED: &str = "disputed";

/// from-status, then the expectation for [paid, past-due, written-off].
const TABLE: [(&str, [Expect; 3]); 5] = {
    use Expect::*;
    [
        (InvoiceStatus::OUTSTANDING, [Flip, Flip, Flip]),
        (InvoiceStatus::PAST_DUE, [Flip, Already, Flip]),
        (InvoiceStatus::PAID, [Already, Refused, Refused]),
        (InvoiceStatus::WRITTEN_OFF, [Refused, Refused, Already]),
        (TENANT_OWED, [Flip, Flip, Flip]),
    ]
};

fn stamp() -> EventStamp {
    EventStamp::new(
        "commerce",
        boss_core::actor::ActorId::Automation("test".into()),
    )
    .with_timestamp(chrono::Utc::now())
}

fn case_id(from: &str, verb: Verb) -> String {
    format!("inv-tr-{from}-{}", verb.target())
}

fn invoice(id: &str, status: &str) -> Invoice {
    Invoice {
        id: id.to_string(),
        account_id: "account-tr-1".to_string(),
        issued_on: NaiveDate::from_ymd_opt(2025, 4, 15).unwrap(),
        due_on: NaiveDate::from_ymd_opt(2025, 5, 15).unwrap(),
        paid_on: None,
        status: status.into(),
        amount_cents: 48_000,
        tax_cents: 0,
        tax_jurisdiction: None,
        currency: "USD".to_string(),
        payment_method: None,
        line_items: vec![InvoiceLineItem {
            id: format!("{id}-l1"),
            invoice_id: id.to_string(),
            revenue_category: RevenueCategory::from("wholesale"),
            amount_cents: 48_000,
            currency: "USD".to_string(),
            description: "Keg order".to_string(),
            ref_id: None,
            sku: None,
            qty: None,
            cost_basis_cents: None,
            cost_total_cents: None,
        }],
    }
}

/// One invoice per cell of the table, each at its row's status.
fn seed() -> Vec<Invoice> {
    TABLE
        .iter()
        .flat_map(|(from, _)| {
            Verb::ALL
                .iter()
                .map(move |verb| invoice(&case_id(from, *verb), from))
        })
        .collect()
}

/// An adapter under the contract: the repository, and a reader for the
/// events it recorded (the outbox on Pg, the recorded list in memory).
#[async_trait::async_trait]
trait Adapter: Sync {
    type Repo: CommerceRepository;
    fn repo(&self) -> &Self::Repo;
    /// The statuses of the recorded `kind` events about `invoice_id`.
    async fn recorded(&self, kind: &str, invoice_id: &str) -> Vec<String>;
}

/// Run every cell and collect each divergence, so one run names every
/// broken cell instead of stopping at the first.
async fn contract<A: Adapter>(adapter: &A) -> Vec<String> {
    let mut broken = Vec::new();
    for (from, row) in TABLE {
        for (verb, expect) in Verb::ALL.into_iter().zip(row) {
            let id = case_id(from, verb);
            let to = verb.target();
            let cell = format!("{from} --{to}--> (expect {expect:?})");
            let result = verb.apply(adapter.repo(), &id).await;
            let after = adapter
                .repo()
                .invoice_by_id(&id)
                .await
                .unwrap()
                .expect("seeded invoice exists")
                .status
                .as_str()
                .to_string();
            let events = adapter.recorded(verb.event_kind(), &id).await;
            let (want_status, want_events) = match expect {
                Expect::Flip => (to, vec![to.to_string()]),
                Expect::Already | Expect::Refused => (from, vec![]),
            };
            match (&result, expect) {
                (Ok(()), Expect::Flip | Expect::Already) => {}
                (Err(CommerceError::Conflict(msg)), Expect::Refused) => {
                    let named = format!("'{from}' -> '{to}'");
                    if !msg.contains(&named) {
                        broken.push(format!("{cell}: refusal {msg:?} does not name {named}"));
                    }
                }
                (got, _) => broken.push(format!("{cell}: answered {got:?}")),
            }
            if after != want_status {
                broken.push(format!("{cell}: left at '{after}', want '{want_status}'"));
            }
            if events != want_events {
                broken.push(format!(
                    "{cell}: recorded {} events with statuses {events:?}, want {want_events:?}",
                    verb.event_kind()
                ));
            }
        }
    }
    for verb in Verb::ALL {
        match verb.apply(adapter.repo(), "inv-tr-nope").await {
            Err(CommerceError::NotFound(_)) => {}
            got => broken.push(format!("missing invoice --{}-->: {got:?}", verb.target())),
        }
    }
    broken
}

struct Memory(InMemoryCommerce);

#[async_trait::async_trait]
impl Adapter for Memory {
    type Repo = InMemoryCommerce;
    fn repo(&self) -> &InMemoryCommerce {
        &self.0
    }
    async fn recorded(&self, kind: &str, invoice_id: &str) -> Vec<String> {
        self.0
            .recorded_events()
            .into_iter()
            .filter(|e| e.kind == kind && e.payload["id"] == invoice_id)
            .map(|e| e.payload["status"].as_str().unwrap_or_default().to_string())
            .collect()
    }
}

#[tokio::test]
async fn in_memory_adapter_keeps_the_transition_table() {
    let broken = contract(&Memory(InMemoryCommerce::new(seed()))).await;
    assert!(
        broken.is_empty(),
        "in-memory adapter:\n{}",
        broken.join("\n")
    );
}

/// Pg on a scratch `TestDb` (never a port-forward); its events are
/// read from `event_outbox`, where the verbs record them in-tx.
struct Pg {
    db: TestDb,
    repo: PgCommerce,
}

#[async_trait::async_trait]
impl Adapter for Pg {
    type Repo = PgCommerce;
    fn repo(&self) -> &PgCommerce {
        &self.repo
    }
    async fn recorded(&self, kind: &str, invoice_id: &str) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT payload->>'status' FROM event_outbox \
             WHERE kind = $1 AND payload->>'id' = $2 ORDER BY id",
        )
        .bind(kind)
        .bind(invoice_id)
        .fetch_all(&self.db.pool)
        .await
        .unwrap()
    }
}

#[tokio::test]
async fn postgres_adapter_keeps_the_transition_table() {
    let db = TestDb::new().await;
    let repo = PgCommerce::new(db.pool.clone());
    for inv in seed() {
        repo.create_invoice(&inv).await.unwrap();
    }
    let broken = contract(&Pg { db, repo }).await;
    assert!(
        broken.is_empty(),
        "postgres adapter:\n{}",
        broken.join("\n")
    );
}
