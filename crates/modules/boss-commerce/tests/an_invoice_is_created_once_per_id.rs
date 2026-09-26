//! An invoice is created once per id — one contract, run against BOTH
//! adapters (backlog 9d2af748).
//!
//! `create_invoice_at` recorded `commerce.invoice.created` on EVERY
//! call. The Pg adapter's `ON CONFLICT (id) DO UPDATE` overwrote the
//! header, deleted and re-inserted the lines, and recorded the event
//! again; only `finance.invoice.issued` was protected, by the unique
//! (kind, source) fact index. The event is what the dispatcher's
//! `products-consume-on-invoice-created` rule reacts to, so a second
//! create under one id could drive a second finished-goods consume — a
//! conservation break, not a duplicate log row. And the overwrite put
//! the body's status back: a redelivered issue (the dispatcher's
//! `commerce.invoice.issue` re-POSTs the same `outstanding` body when
//! its event is redelivered) turned a PAID invoice owed again.
//!
//! Each adapter is held to four things: a repeat of the same body
//! writes nothing and records nothing; it answers the invoice as
//! stored, and says it was already created; a repeat after the invoice
//! moved (paid) leaves it where it moved to — status and `paid_on` are
//! the verbs' to write, not the create's; and a body that differs from
//! the stored invoice in anything fixed at issuance is refused by name.

use boss_commerce::port::{CommerceError, CommerceRepository, InvoiceCreate};
use boss_commerce::types::*;
use boss_commerce::{InMemoryCommerce, PgCommerce};
use boss_core::publisher::EventStamp;
use boss_testing::TestDb;
use chrono::NaiveDate;

const ID: &str = "inv-once-1";

fn stamp() -> EventStamp {
    EventStamp::new(
        "commerce",
        boss_core::actor::ActorId::Automation("test".into()),
    )
    .with_timestamp(chrono::Utc::now())
}

fn invoice() -> Invoice {
    Invoice {
        id: ID.to_string(),
        account_id: "account-once-1".to_string(),
        issued_on: NaiveDate::from_ymd_opt(2026, 9, 25).unwrap(),
        due_on: NaiveDate::from_ymd_opt(2026, 10, 25).unwrap(),
        paid_on: None,
        status: InvoiceStatus::OUTSTANDING.into(),
        amount_cents: 48_000,
        tax_cents: 0,
        tax_jurisdiction: None,
        currency: "USD".to_string(),
        payment_method: None,
        line_items: vec![InvoiceLineItem {
            id: format!("{ID}-l1"),
            invoice_id: ID.to_string(),
            revenue_category: RevenueCategory::from("wholesale"),
            amount_cents: 48_000,
            currency: "USD".to_string(),
            description: "Keg order".to_string(),
            ref_id: None,
            sku: Some("sku-ipa-half".to_string()),
            qty: Some(2),
            cost_basis_cents: None,
            cost_total_cents: None,
        }],
    }
}

#[async_trait::async_trait]
trait Adapter: Sync {
    type Repo: CommerceRepository;
    fn repo(&self) -> &Self::Repo;
    /// How many `commerce.invoice.created` events name `ID`.
    async fn created_events(&self) -> usize;
}

/// Run every clause and collect each divergence, so one run names
/// every broken clause instead of stopping at the first.
async fn contract<A: Adapter>(adapter: &A) -> Vec<String> {
    let mut broken = Vec::new();
    let repo = adapter.repo();
    let sent = invoice();

    match repo
        .create_invoice_at(&sent, chrono::Utc::now(), &stamp())
        .await
    {
        Ok(InvoiceCreate::Created(_)) => {}
        got => broken.push(format!("first create: answered {got:?}")),
    }
    if adapter.created_events().await != 1 {
        broken.push(format!(
            "first create recorded {} created events, want 1",
            adapter.created_events().await
        ));
    }

    // The same body again — a retried POST, a redelivered issue.
    match repo
        .create_invoice_at(&sent, chrono::Utc::now(), &stamp())
        .await
    {
        Ok(InvoiceCreate::AlreadyCreated(stored)) if stored.id == ID => {}
        got => broken.push(format!("repeat create: answered {got:?}")),
    }
    let n = adapter.created_events().await;
    if n != 1 {
        broken.push(format!("repeat create: {n} created events, want 1"));
    }

    // The invoice moves; the redelivered issue arrives after it.
    let paid_on = NaiveDate::from_ymd_opt(2026, 9, 26).unwrap();
    repo.mark_invoice_paid_at(ID, paid_on, &stamp())
        .await
        .expect("an outstanding invoice is paid");
    match repo
        .create_invoice_at(&sent, chrono::Utc::now(), &stamp())
        .await
    {
        Ok(InvoiceCreate::AlreadyCreated(_)) => {}
        got => broken.push(format!("create after paid: answered {got:?}")),
    }
    let after = repo.invoice_by_id(ID).await.unwrap().expect("it exists");
    if !after.status.is_paid() || after.paid_on != Some(paid_on) {
        broken.push(format!(
            "create after paid moved the invoice back: status {} paid_on {:?}",
            after.status, after.paid_on
        ));
    }
    let n = adapter.created_events().await;
    if n != 1 {
        broken.push(format!("create after paid: {n} created events, want 1"));
    }

    // Another invoice under the same id.
    let mut other = invoice();
    other.amount_cents = 96_000;
    other.line_items[0].amount_cents = 96_000;
    other.line_items[0].qty = Some(4);
    match repo
        .create_invoice_at(&other, chrono::Utc::now(), &stamp())
        .await
    {
        Err(CommerceError::Conflict(msg))
            if msg.contains("amount_cents") && msg.contains("line_items") => {}
        got => broken.push(format!(
            "a different body under the id: answered {got:?}, want a Conflict naming \
             amount_cents and line_items"
        )),
    }
    let stored = repo.invoice_by_id(ID).await.unwrap().expect("it exists");
    if stored.amount_cents != 48_000 || stored.line_items.len() != 1 {
        broken.push(format!("the refused body was written: {stored:?}"));
    }
    let n = adapter.created_events().await;
    if n != 1 {
        broken.push(format!("refused create: {n} created events, want 1"));
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
    async fn created_events(&self) -> usize {
        self.0
            .recorded_events()
            .into_iter()
            .filter(|e| e.kind == boss_commerce::events::INVOICE_CREATED && e.payload["id"] == ID)
            .count()
    }
}

#[tokio::test]
async fn in_memory_adapter_creates_an_invoice_once_per_id() {
    let broken = contract(&Memory(InMemoryCommerce::new(vec![]))).await;
    assert!(
        broken.is_empty(),
        "in-memory adapter:\n{}",
        broken.join("\n")
    );
}

/// Pg on a scratch `TestDb` (never a port-forward); its events are read
/// from `event_outbox`, where the create records them in-tx.
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
    async fn created_events(&self) -> usize {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM event_outbox WHERE kind = $1 AND payload->>'id' = $2",
        )
        .bind(boss_commerce::events::INVOICE_CREATED)
        .bind(ID)
        .fetch_one(&self.db.pool)
        .await
        .unwrap();
        n as usize
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_adapter_creates_an_invoice_once_per_id() {
    let db = TestDb::new().await;
    let repo = PgCommerce::new(db.pool.clone());
    let broken = contract(&Pg { db, repo }).await;
    assert!(
        broken.is_empty(),
        "postgres adapter:\n{}",
        broken.join("\n")
    );
}
