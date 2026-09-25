//! In-memory adapter for `CommerceRepository`.

use async_trait::async_trait;
use boss_core::publisher::EventStamp;

use crate::port::{CommerceError, CommerceRepository};
use crate::types::{AccountOpenAr, ArAgingBucket, Invoice, InvoiceSummary, RevenueLine};

pub struct InMemoryCommerce {
    invoices: Vec<Invoice>,
    revenue: Vec<RevenueLine>,
    /// Ids flipped by `mark_invoice_written_off` — enough state for
    /// HTTP tests to exercise the converge-on-double-delivery
    /// contract (the transactional flip itself is pg-pinned).
    written_off: std::sync::Mutex<std::collections::HashSet<String>>,
    /// Events the outbox-migrated paths would have recorded in-tx —
    /// the in-memory analogue of `event_outbox`, collected for test
    /// assertions (no relay here; the pg path is the real contract).
    recorded: std::sync::Mutex<Vec<boss_core::event::Event>>,
}

impl InMemoryCommerce {
    pub fn new(invoices: Vec<Invoice>) -> Self {
        Self {
            invoices,
            revenue: Vec::new(),
            written_off: std::sync::Mutex::new(std::collections::HashSet::new()),
            recorded: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Events the outbox paths recorded — test visibility.
    pub fn recorded_events(&self) -> Vec<boss_core::event::Event> {
        self.recorded.lock().map(|v| v.clone()).unwrap_or_default()
    }

    fn record(&self, event: boss_core::event::Event) {
        if let Ok(mut v) = self.recorded.lock() {
            v.push(event);
        }
    }

    pub fn with_revenue(mut self, revenue: Vec<RevenueLine>) -> Self {
        self.revenue = revenue;
        self
    }

    /// The invoices still owed — `InvoiceStatus::is_owed`, with the
    /// write-off overlay applied. The one filter `open_ar_by_account`
    /// and the summary's AR aging both read (backlog 926d64a3).
    fn owed_invoices(&self) -> Result<Vec<&Invoice>, CommerceError> {
        let written_off = self
            .written_off
            .lock()
            .map_err(|e| CommerceError::Storage(format!("written_off lock: {e}")))?;
        Ok(self
            .invoices
            .iter()
            .filter(|i| i.status.is_owed() && !written_off.contains(&i.id))
            .collect())
    }
}

#[async_trait]
impl CommerceRepository for InMemoryCommerce {
    async fn all_revenue(&self) -> Result<Vec<RevenueLine>, CommerceError> {
        Ok(self.revenue.clone())
    }

    async fn all_invoices(&self) -> Result<Vec<Invoice>, CommerceError> {
        Ok(self.invoices.clone())
    }

    async fn list_invoices(
        &self,
        limit: i64,
        offset: i64,
        account_id: Option<&str>,
    ) -> Result<(Vec<Invoice>, i64), CommerceError> {
        let filtered: Vec<&Invoice> = match account_id {
            Some(cid) => self
                .invoices
                .iter()
                .filter(|i| i.account_id == cid)
                .collect(),
            None => self.invoices.iter().collect(),
        };
        let total = filtered.len() as i64;
        let start = (offset as usize).min(filtered.len());
        let end = (start + limit as usize).min(filtered.len());
        Ok((
            filtered[start..end].iter().map(|&i| i.clone()).collect(),
            total,
        ))
    }

    async fn open_ar_by_account(&self) -> Result<Vec<AccountOpenAr>, CommerceError> {
        let by_account = self.owed_invoices()?.into_iter().fold(
            std::collections::BTreeMap::<&str, (i64, i64)>::new(),
            |mut acc, i| {
                let e = acc.entry(i.account_id.as_str()).or_default();
                *e = (e.0 + i.amount_cents, e.1 + 1);
                acc
            },
        );
        Ok(by_account
            .into_iter()
            .map(|(account_id, (open_ar_cents, open_count))| AccountOpenAr {
                account_id: account_id.to_string(),
                open_ar_cents,
                open_count,
            })
            .collect())
    }

    async fn invoice_by_id(&self, id: &str) -> Result<Option<Invoice>, CommerceError> {
        let mut inv = self.invoices.iter().find(|i| i.id == id).cloned();
        // Overlay the write-off flip so post-flip reads (the event
        // emit path) see the terminal status, matching pg.
        if let Some(inv) = inv.as_mut()
            && self
                .written_off
                .lock()
                .map_err(|e| CommerceError::Storage(format!("written_off lock: {e}")))?
                .contains(id)
        {
            inv.status = crate::types::InvoiceStatus::WRITTEN_OFF.into();
        }
        Ok(inv)
    }

    async fn create_invoice_at(
        &self,
        invoice: &Invoice,
        _now: chrono::DateTime<chrono::Utc>,
        stamp: &EventStamp,
    ) -> Result<Invoice, CommerceError> {
        if invoice.line_items.is_empty() {
            return Err(CommerceError::Storage(format!(
                "invoice {} has no line items",
                invoice.id
            )));
        }
        let sum: i64 = invoice.line_items.iter().map(|l| l.amount_cents).sum();
        if sum != invoice.amount_cents {
            return Err(CommerceError::Storage(format!(
                "invoice {} amount_cents={} but line items sum to {}",
                invoice.id, invoice.amount_cents, sum
            )));
        }
        if invoice
            .line_items
            .iter()
            .any(|l| l.currency != invoice.currency)
        {
            return Err(CommerceError::Storage(format!(
                "invoice {} line items disagree on currency with header {}",
                invoice.id, invoice.currency
            )));
        }
        // In-memory impl has no FG inventory to draw down — return
        // the invoice unchanged. Tests that depend on enrichment
        // use the postgres impl.
        self.record(stamp.event(
            crate::events::INVOICE_CREATED,
            crate::events::invoice_created_payload(invoice),
        ));
        Ok(invoice.clone())
    }

    async fn mark_invoice_paid_at(
        &self,
        id: &str,
        _paid_on: chrono::NaiveDate,
        stamp: &EventStamp,
    ) -> Result<(), CommerceError> {
        let Some(inv) = self.invoices.iter().find(|i| i.id == id) else {
            return Err(CommerceError::NotFound(format!("invoice {id}")));
        };
        self.record(stamp.event(
            crate::events::INVOICE_PAID,
            serde_json::to_value(inv).unwrap_or_default(),
        ));
        Ok(())
    }

    async fn mark_invoice_past_due(
        &self,
        id: &str,
        stamp: &EventStamp,
    ) -> Result<(), CommerceError> {
        let Some(inv) = self.invoices.iter().find(|i| i.id == id) else {
            return Err(CommerceError::NotFound(format!("invoice {id}")));
        };
        self.record(stamp.event(
            crate::events::INVOICE_PAST_DUE,
            serde_json::to_value(inv).unwrap_or_default(),
        ));
        Ok(())
    }

    async fn mark_invoice_written_off(
        &self,
        id: &str,
        stamp: &EventStamp,
    ) -> Result<bool, CommerceError> {
        use crate::types::InvoiceStatus;
        let Some(inv) = self.invoices.iter().find(|i| i.id == id) else {
            return Err(CommerceError::NotFound(format!("invoice {id}")));
        };
        if inv.status.as_str() == InvoiceStatus::PAID {
            return Err(CommerceError::Conflict(format!(
                "invoice {id} is 'paid': only outstanding or past-due \
                 invoices write off"
            )));
        }
        let mut flipped = self
            .written_off
            .lock()
            .map_err(|e| CommerceError::Storage(format!("written_off lock: {e}")))?;
        if inv.status.as_str() == InvoiceStatus::WRITTEN_OFF || flipped.contains(id) {
            return Ok(false);
        }
        flipped.insert(id.to_string());
        // Emit-once is structural: only the flip-winning call records.
        let mut written = inv.clone();
        written.status = InvoiceStatus::WRITTEN_OFF.into();
        self.record(stamp.event(
            crate::events::INVOICE_WRITTEN_OFF,
            serde_json::to_value(&written).unwrap_or_default(),
        ));
        Ok(true)
    }

    async fn invoice_summary(
        &self,
        today: chrono::NaiveDate,
    ) -> Result<InvoiceSummary, CommerceError> {
        // Revenue here is GL-sourced on Pg and has no in-memory
        // analogue; the AR aging is invoices alone, so it is computed
        // with the same owed filter and bucketing rule as Pg.
        let ar_aging = ArAgingBucket::age(
            self.owed_invoices()?
                .into_iter()
                .map(|i| ((today - i.due_on).num_days(), 1, i.amount_cents)),
        );
        let total_outstanding_cents = ar_aging.iter().map(|b| b.total_cents).sum();
        Ok(InvoiceSummary {
            revenue_ttm: Vec::new(),
            total_revenue_ttm_cents: 0,
            total_cogs_ttm_cents: 0,
            total_gross_margin_ttm_cents: 0,
            ar_aging,
            total_outstanding_cents,
            total_invoice_count: self.invoices.len() as i64,
            revenue_by_month: Vec::new(),
            currency: "USD".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;

    fn test_invoice(id: &str) -> Invoice {
        Invoice {
            id: id.to_string(),
            account_id: "account-001".to_string(),
            issued_on: chrono::NaiveDate::from_ymd_opt(2025, 3, 15).unwrap(),
            due_on: chrono::NaiveDate::from_ymd_opt(2025, 4, 15).unwrap(),
            paid_on: None,
            status: InvoiceStatus::OUTSTANDING.into(),
            amount_cents: 1_200_000,
            currency: "USD".to_string(),
            tax_cents: 0,
            tax_jurisdiction: None,
            payment_method: None,
            line_items: vec![InvoiceLineItem {
                id: format!("{id}-l1"),
                invoice_id: id.to_string(),
                revenue_category: RevenueCategory::from("wholesale"),
                amount_cents: 1_200_000,
                currency: "USD".to_string(),
                description: "Test line".to_string(),
                ref_id: None,
                sku: None,
                qty: None,
                cost_basis_cents: None,
                cost_total_cents: None,
            }],
        }
    }

    fn test_repo() -> InMemoryCommerce {
        InMemoryCommerce::new(vec![
            test_invoice("inv-001"),
            test_invoice("inv-002"),
            test_invoice("inv-003"),
        ])
    }

    #[tokio::test]
    async fn all_invoices_returns_all() {
        let repo = test_repo();
        assert_eq!(repo.all_invoices().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn invoice_by_id_found() {
        let repo = test_repo();
        let inv = repo.invoice_by_id("inv-002").await.unwrap();
        assert!(inv.is_some());
        assert_eq!(inv.unwrap().id, "inv-002");
    }

    #[tokio::test]
    async fn invoice_by_id_not_found() {
        let repo = test_repo();
        assert!(repo.invoice_by_id("inv-999").await.unwrap().is_none());
    }

    /// The summary's AR aging is the owed invoices and nothing else
    /// (backlog 926d64a3): paid is out, and so is written-off — both a
    /// row that arrives written off and one flipped by
    /// `mark_invoice_written_off`, which this adapter keeps as an
    /// overlay. Same definition, same answer as the Pg adapter's
    /// `summary_ar_aging_excludes_written_off_invoices`.
    #[tokio::test]
    async fn summary_ar_aging_counts_only_owed_invoices() {
        let day = |y, m, d| chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap();
        let inv = |id: &str, cents: i64, status: &str, due_on: chrono::NaiveDate| {
            let mut i = test_invoice(id);
            i.amount_cents = cents;
            i.line_items[0].amount_cents = cents;
            i.status = status.into();
            i.due_on = due_on;
            i
        };
        let repo = InMemoryCommerce::new(vec![
            inv(
                "inv-1",
                125_000,
                InvoiceStatus::OUTSTANDING,
                day(2025, 6, 10),
            ),
            inv("inv-2", 2_550, InvoiceStatus::PAST_DUE, day(2025, 4, 1)),
            inv("inv-3", 5_000, InvoiceStatus::PAID, day(2025, 4, 1)),
            inv("inv-4", 9_900, InvoiceStatus::WRITTEN_OFF, day(2025, 4, 1)),
            inv("inv-5", 800, InvoiceStatus::PAST_DUE, day(2025, 1, 1)),
        ]);
        let stamp = EventStamp::new(
            "commerce",
            boss_core::actor::ActorId::Automation("test".into()),
        );
        assert!(
            repo.mark_invoice_written_off("inv-5", &stamp)
                .await
                .unwrap()
        );

        let summary = repo.invoice_summary(day(2025, 6, 1)).await.unwrap();
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
        assert_eq!(summary.total_invoice_count, 5, "the count is every status");
    }
}
