//! The open-AR aggregate on Postgres (backlog 5257bfa9): the SQL the
//! live service runs, driven through the same lifecycle writes the
//! workflows make — a create, a mark-paid, a write-off — so the
//! statuses it filters on are the ones those writes leave behind.

use boss_commerce::PgCommerce;
use boss_commerce::port::CommerceRepository;
use boss_commerce::types::*;
use boss_core::publisher::EventStamp;
use boss_testing::TestDb;
use chrono::NaiveDate;

fn stamp() -> EventStamp {
    EventStamp::new(
        "commerce",
        boss_core::actor::ActorId::Automation("test".into()),
    )
    .with_timestamp(chrono::Utc::now())
}

fn invoice(id: &str, account: &str, cents: i64, status: &str) -> Invoice {
    Invoice {
        id: id.to_string(),
        account_id: account.to_string(),
        issued_on: NaiveDate::from_ymd_opt(2025, 4, 15).unwrap(),
        due_on: NaiveDate::from_ymd_opt(2025, 5, 15).unwrap(),
        paid_on: None,
        status: status.into(),
        amount_cents: cents,
        tax_cents: 0,
        tax_jurisdiction: None,
        currency: "USD".to_string(),
        payment_method: None,
        line_items: vec![InvoiceLineItem {
            id: format!("{id}-l1"),
            invoice_id: id.to_string(),
            revenue_category: RevenueCategory::from("wholesale"),
            amount_cents: cents,
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

#[tokio::test]
async fn open_ar_by_account_sums_what_is_still_owed() {
    let db = TestDb::new().await;
    let repo = PgCommerce::new(db.pool.clone());
    for inv in [
        invoice(
            "inv-ar-1",
            "acct-ar-zed",
            125_000,
            InvoiceStatus::OUTSTANDING,
        ),
        invoice("inv-ar-2", "acct-ar-zed", 2_550, InvoiceStatus::PAST_DUE),
        invoice("inv-ar-3", "acct-ar-zed", 5_000, InvoiceStatus::OUTSTANDING),
        invoice("inv-ar-4", "acct-ar-zed", 9_900, InvoiceStatus::PAST_DUE),
        invoice(
            "inv-ar-5",
            "acct-ar-anchor",
            40_000,
            InvoiceStatus::OUTSTANDING,
        ),
        invoice("inv-ar-6", "acct-ar-bay", 700, InvoiceStatus::OUTSTANDING),
    ] {
        repo.create_invoice(&inv).await.unwrap();
    }
    repo.mark_invoice_paid_at(
        "inv-ar-3",
        NaiveDate::from_ymd_opt(2025, 5, 1).unwrap(),
        &stamp(),
    )
    .await
    .unwrap();
    repo.mark_invoice_paid_at(
        "inv-ar-5",
        NaiveDate::from_ymd_opt(2025, 5, 1).unwrap(),
        &stamp(),
    )
    .await
    .unwrap();
    assert!(
        repo.mark_invoice_written_off("inv-ar-4", &stamp())
            .await
            .unwrap()
    );

    let rows = repo.open_ar_by_account().await.unwrap();
    assert_eq!(
        rows,
        vec![
            AccountOpenAr {
                account_id: "acct-ar-bay".into(),
                open_ar_cents: 700,
                open_count: 1,
            },
            AccountOpenAr {
                account_id: "acct-ar-zed".into(),
                open_ar_cents: 127_550,
                open_count: 2,
            },
        ],
        "paid and written-off are not owed; an account with nothing open has no row"
    );
}

/// The summary's AR aging reads the same one definition of owed
/// (backlog 926d64a3). It filtered `status <> 'paid'`, so a write-off
/// — which credits 1100 A/R — stayed in `total_outstanding_cents`
/// and disagreed with the ledger, the open-AR read above and the
/// Finance InvoicesTab.
#[tokio::test]
async fn summary_ar_aging_excludes_written_off_invoices() {
    let db = TestDb::new().await;
    let repo = PgCommerce::new(db.pool.clone());
    let due = |y, m, d| NaiveDate::from_ymd_opt(y, m, d).unwrap();
    let with_due = |mut inv: Invoice, due_on: NaiveDate| {
        inv.issued_on = due_on - chrono::Days::new(30);
        inv.due_on = due_on;
        inv
    };
    for inv in [
        with_due(
            invoice("inv-sum-1", "acct-sum", 125_000, InvoiceStatus::OUTSTANDING),
            due(2025, 6, 10),
        ),
        with_due(
            invoice("inv-sum-2", "acct-sum", 2_550, InvoiceStatus::PAST_DUE),
            due(2025, 4, 1),
        ),
        with_due(
            invoice("inv-sum-3", "acct-sum", 5_000, InvoiceStatus::OUTSTANDING),
            due(2025, 4, 1),
        ),
        with_due(
            invoice("inv-sum-4", "acct-sum", 9_900, InvoiceStatus::PAST_DUE),
            due(2025, 4, 1),
        ),
    ] {
        repo.create_invoice(&inv).await.unwrap();
    }
    repo.mark_invoice_paid_at("inv-sum-3", due(2025, 5, 1), &stamp())
        .await
        .unwrap();
    assert!(
        repo.mark_invoice_written_off("inv-sum-4", &stamp())
            .await
            .unwrap()
    );

    let summary = repo.invoice_summary(due(2025, 6, 1)).await.unwrap();
    let bucket = |label: &str, count, total_cents| ArAgingBucket {
        label: label.into(),
        count,
        total_cents,
    };
    assert_eq!(
        summary.ar_aging,
        vec![
            bucket("current", 1, 125_000),
            bucket("1-30", 0, 0),
            bucket("31-60", 0, 0),
            bucket("61-90", 1, 2_550),
            bucket("90+", 0, 0),
        ],
        "paid and written-off are not owed, so neither ages"
    );
    assert_eq!(summary.total_outstanding_cents, 127_550);
    assert_eq!(summary.total_invoice_count, 4, "the count is every status");
}
