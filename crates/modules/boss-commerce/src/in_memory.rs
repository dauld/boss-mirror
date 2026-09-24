//! In-memory adapter for `CommerceRepository`.

use async_trait::async_trait;
use boss_core::publisher::EventStamp;

use crate::port::{CommerceError, CommerceRepository};
use crate::types::{Invoice, InvoiceSummary, RevenueLine};

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
        _today: chrono::NaiveDate,
    ) -> Result<InvoiceSummary, CommerceError> {
        Ok(InvoiceSummary {
            revenue_ttm: Vec::new(),
            total_revenue_ttm_cents: 0,
            total_cogs_ttm_cents: 0,
            total_gross_margin_ttm_cents: 0,
            ar_aging: Vec::new(),
            total_outstanding_cents: 0,
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
}
