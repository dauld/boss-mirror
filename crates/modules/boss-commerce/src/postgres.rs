//! Postgres adapter for `CommerceRepository`.
//!
//! Queries `opportunities` and `invoices` tables and assembles into
//! domain structs.

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use crate::port::{CommerceError, CommerceRepository};
use crate::types::*;

pub struct PgCommerce {
    pool: PgPool,
}

impl PgCommerce {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Load line items for every invoice in the provided slice in a
    /// single query and stitch them back into the parent invoices.
    /// Keeps list endpoints O(1 query) instead of O(N) per page.
    async fn attach_line_items(&self, invoices: &mut [Invoice]) -> Result<(), CommerceError> {
        if invoices.is_empty() {
            return Ok(());
        }
        let ids: Vec<String> = invoices.iter().map(|i| i.id.clone()).collect();
        let lines: Vec<LineItemRow> = sqlx::query_as(
            "SELECT id, invoice_id, revenue_category, amount_cents, currency, description, ref_id, \
                    sku, qty, cost_basis_cents, cost_total_cents \
             FROM invoice_line_items WHERE invoice_id = ANY($1) ORDER BY id",
        )
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CommerceError::Storage(e.to_string()))?;

        let mut by_invoice: std::collections::HashMap<String, Vec<InvoiceLineItem>> =
            std::collections::HashMap::new();
        for row in lines {
            by_invoice
                .entry(row.invoice_id.clone())
                .or_default()
                .push(row.into_line_item());
        }
        for inv in invoices {
            if let Some(lines) = by_invoice.remove(&inv.id) {
                inv.line_items = lines;
            }
        }
        Ok(())
    }
}

#[async_trait]
impl CommerceRepository for PgCommerce {
    async fn all_revenue(&self) -> Result<Vec<RevenueLine>, CommerceError> {
        // Derive monthly revenue from invoice line items. Each line
        // already carries its own `revenue_category`, so the rollup
        // is a direct GROUP BY without any category mapping. The
        // month comes from the parent invoice's `issued_on` since
        // line items don't have their own issue date — a revenue
        // recognition schedule (ASC 606) comes with the later GL
        // track; for now we recognize at invoice-issue time.
        let rows: Vec<RevenueLineRow> = sqlx::query_as(
            "SELECT \
                date_trunc('month', i.issued_on)::date AS month, \
                l.revenue_category AS category, \
                SUM(l.amount_cents)::bigint AS amount_cents \
             FROM invoice_line_items l \
             JOIN invoices i ON i.id = l.invoice_id \
             GROUP BY month, l.revenue_category \
             ORDER BY month DESC, l.revenue_category",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CommerceError::Storage(e.to_string()))?;

        Ok(rows.into_iter().map(|r| r.into_revenue_line()).collect())
    }

    async fn all_invoices(&self) -> Result<Vec<Invoice>, CommerceError> {
        let rows: Vec<InvoiceRow> =
            sqlx::query_as("SELECT * FROM invoices ORDER BY issued_on DESC")
                .fetch_all(&self.pool)
                .await
                .map_err(|e| CommerceError::Storage(e.to_string()))?;

        let mut invoices: Vec<Invoice> = rows.into_iter().map(|r| r.into_invoice()).collect();
        self.attach_line_items(&mut invoices).await?;
        Ok(invoices)
    }

    async fn list_invoices(
        &self,
        limit: i64,
        offset: i64,
        account_id: Option<&str>,
    ) -> Result<(Vec<Invoice>, i64), CommerceError> {
        let (total,): (i64,) = match account_id {
            Some(cid) => {
                sqlx::query_as("SELECT count(*) FROM invoices WHERE account_id = $1")
                    .bind(cid)
                    .fetch_one(&self.pool)
                    .await
            }
            None => {
                sqlx::query_as("SELECT count(*) FROM invoices")
                    .fetch_one(&self.pool)
                    .await
            }
        }
        .map_err(|e| CommerceError::Storage(e.to_string()))?;

        let rows: Vec<InvoiceRow> = match account_id {
            Some(cid) => {
                sqlx::query_as(
                    "SELECT * FROM invoices WHERE account_id = $1 \
                 ORDER BY issued_on DESC LIMIT $2 OFFSET $3",
                )
                .bind(cid)
                .bind(limit)
                .bind(offset)
                .fetch_all(&self.pool)
                .await
            }
            None => {
                sqlx::query_as("SELECT * FROM invoices ORDER BY issued_on DESC LIMIT $1 OFFSET $2")
                    .bind(limit)
                    .bind(offset)
                    .fetch_all(&self.pool)
                    .await
            }
        }
        .map_err(|e| CommerceError::Storage(e.to_string()))?;

        let mut invoices: Vec<Invoice> = rows.into_iter().map(|r| r.into_invoice()).collect();
        self.attach_line_items(&mut invoices).await?;
        Ok((invoices, total))
    }

    async fn open_ar_by_account(&self) -> Result<Vec<AccountOpenAr>, CommerceError> {
        // Summed here, over every row, so the answer is exact at any
        // volume (backlog 5257bfa9). The not-owed statuses are bound
        // from the one Rust list rather than spelled in the SQL.
        let not_owed: Vec<String> = InvoiceStatus::NOT_OWED
            .iter()
            .map(|s| s.to_string())
            .collect();
        let rows: Vec<(String, i64, i64)> = sqlx::query_as(
            "SELECT account_id, \
                    COALESCE(SUM(amount_cents), 0)::bigint, \
                    COUNT(*)::bigint \
             FROM invoices \
             WHERE status <> ALL($1) \
             GROUP BY account_id \
             ORDER BY account_id",
        )
        .bind(&not_owed)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CommerceError::Storage(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|(account_id, open_ar_cents, open_count)| AccountOpenAr {
                account_id,
                open_ar_cents,
                open_count,
            })
            .collect())
    }

    async fn invoice_by_id(&self, id: &str) -> Result<Option<Invoice>, CommerceError> {
        let row: Option<InvoiceRow> = sqlx::query_as("SELECT * FROM invoices WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| CommerceError::Storage(e.to_string()))?;

        let Some(row) = row else { return Ok(None) };
        let mut invoice = row.into_invoice();
        let lines: Vec<LineItemRow> = sqlx::query_as(
            "SELECT id, invoice_id, revenue_category, amount_cents, currency, description, ref_id, \
                    sku, qty, cost_basis_cents, cost_total_cents \
             FROM invoice_line_items WHERE invoice_id = $1 ORDER BY id",
        )
        .bind(&invoice.id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CommerceError::Storage(e.to_string()))?;
        invoice.line_items = lines.into_iter().map(|l| l.into_line_item()).collect();
        Ok(Some(invoice))
    }

    async fn create_invoice_at(
        &self,
        inv: &Invoice,
        now: chrono::DateTime<chrono::Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<Invoice, CommerceError> {
        // Invariant: line-item revenue + sales tax must equal the
        // header rollup. Enforce in the adapter so a buggy caller
        // can't persist a document whose total lies about its
        // contents. An invoice with tax_cents=0 reduces this to
        // `line_sum == amount_cents`; a tax-bearing invoice adds
        // tax_cents to the RHS.
        let line_sum: i64 = inv.line_items.iter().map(|l| l.amount_cents).sum();
        if inv.line_items.is_empty() {
            return Err(CommerceError::Storage(format!(
                "invoice {} has no line items",
                inv.id
            )));
        }
        if inv.tax_cents < 0 {
            return Err(CommerceError::Storage(format!(
                "invoice {} tax_cents={} must be non-negative",
                inv.id, inv.tax_cents
            )));
        }
        if inv.tax_cents > 0 && inv.tax_jurisdiction.is_none() {
            return Err(CommerceError::Storage(format!(
                "invoice {} has tax_cents={} but no tax_jurisdiction",
                inv.id, inv.tax_cents
            )));
        }
        if line_sum + inv.tax_cents != inv.amount_cents {
            return Err(CommerceError::Storage(format!(
                "invoice {} amount_cents={} but line items ({}) + tax ({}) sum to {}",
                inv.id,
                inv.amount_cents,
                line_sum,
                inv.tax_cents,
                line_sum + inv.tax_cents
            )));
        }
        if inv.line_items.iter().any(|l| l.currency != inv.currency) {
            return Err(CommerceError::Storage(format!(
                "invoice {} line items disagree on currency with header {}",
                inv.id, inv.currency
            )));
        }

        // Transparent newtype — the bare kebab code the column stores.
        let status = inv.status.as_str();

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| CommerceError::Storage(e.to_string()))?;

        // Identity write-through (subject-model R1, Q1): the invoice's
        // identity row lands in the same transaction as its domain row.
        boss_subject_kinds::subjects::record_subject_in_tx(&mut tx, "invoice", &inv.id, None)
            .await
            .map_err(CommerceError::Storage)?;

        sqlx::query(
            "INSERT INTO invoices (id, account_id, issued_on, due_on, paid_on, status, \
                                   amount_cents, currency, tax_cents, tax_jurisdiction, \
                                   payment_method, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) \
             ON CONFLICT (id) DO UPDATE SET \
                account_id = EXCLUDED.account_id, \
                issued_on = EXCLUDED.issued_on, \
                due_on = EXCLUDED.due_on, \
                status = EXCLUDED.status, \
                paid_on = EXCLUDED.paid_on, \
                amount_cents = EXCLUDED.amount_cents, \
                currency = EXCLUDED.currency, \
                tax_cents = EXCLUDED.tax_cents, \
                tax_jurisdiction = EXCLUDED.tax_jurisdiction, \
                payment_method = EXCLUDED.payment_method",
        )
        .bind(&inv.id)
        .bind(&inv.account_id)
        .bind(inv.issued_on)
        .bind(inv.due_on)
        .bind(inv.paid_on)
        .bind(status)
        .bind(inv.amount_cents)
        .bind(&inv.currency)
        .bind(inv.tax_cents)
        .bind(inv.tax_jurisdiction.as_deref())
        .bind(inv.payment_method.as_deref())
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|e| CommerceError::Storage(e.to_string()))?;

        // On re-emission of an existing invoice, wipe its old line
        // items before re-inserting so the document stays consistent.
        // Cheaper than diffing and replay is the only path that
        // re-emits today.
        sqlx::query("DELETE FROM invoice_line_items WHERE invoice_id = $1")
            .bind(&inv.id)
            .execute(&mut *tx)
            .await
            .map_err(|e| CommerceError::Storage(e.to_string()))?;

        // The FG drawdown + COGS moved OUT of the invoice tx (Q2,
        // docs/architecture-decisions.md §Finance & ledger, 6b): the
        // dispatcher's `products-consume-on-invoice-created` rule
        // reacts to the `commerce.invoice.created` event this issue
        // emits and drives `/api/products/{sku}/inventory/consume` per
        // FG line — the consume drains the row's conserved value and
        // posts `finance.cogs.recognized` at exactly the drained
        // cents. Commerce posts revenue / AR / tax only; it no longer
        // touches another module's projection, and an invoice no
        // longer 409s on insufficient FG (the consume NAKs until
        // production lands — a visible backorder, not a blocked sale).
        // Lines keep sku + qty (the consume rule reads them);
        // cost_basis/cost_total stay NULL — per-category margin comes
        // from the consume's tagged COGS facts.
        let enriched_lines: Vec<InvoiceLineItem> = inv.line_items.to_vec();
        for enriched in &enriched_lines {
            sqlx::query(
                "INSERT INTO invoice_line_items \
                    (id, invoice_id, revenue_category, amount_cents, currency, description, ref_id, \
                     sku, qty, cost_basis_cents, cost_total_cents, created_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
            )
            .bind(&enriched.id)
            .bind(&inv.id)
            .bind(enriched.revenue_category.as_str())
            .bind(enriched.amount_cents)
            .bind(&enriched.currency)
            .bind(&enriched.description)
            .bind(&enriched.ref_id)
            .bind(&enriched.sku)
            .bind(enriched.qty)
            .bind(enriched.cost_basis_cents)
            .bind(enriched.cost_total_cents)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(|e| CommerceError::Storage(e.to_string()))?;
        }

        // Assemble the invoice (line_items carry sku + qty for the
        // consume rule; no cost stamping — see above), then build the
        // finance.invoice.issued fact from the SAME shared helper the
        // commerce.invoice.created event uses (events::invoice_created_payload),
        // off this SAME value — so the live fact and the fact rebuilt from
        // the event are byte-identical (the event carries the full Invoice
        // struct because it also rebuilds the invoices projection). Idempotent
        // via the unique (kind, source) index; replay re-emission is a no-op.
        let mut enriched_invoice = inv.clone();
        enriched_invoice.line_items = enriched_lines;
        let issued_payload = crate::events::invoice_created_payload(&enriched_invoice);
        insert_fact(
            &mut tx,
            "finance.invoice.issued",
            inv.issued_on,
            &issued_payload,
            "invoices",
            &inv.id,
        )
        .await?;

        // OUTBOX (phase 2): the commerce.invoice.created event records
        // in THIS transaction — same enriched payload as the fact above
        // — so the state change and its provenance commit or abort
        // together, and the subject_edges trigger rejects a ghost
        // account_id BEFORE it becomes state (the 2026-07-13 incident
        // class, closed at the source). boss-event-relay moves it to
        // audit_log + NATS after commit.
        let event = stamp.event(crate::events::INVOICE_CREATED, issued_payload.clone());
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(CommerceError::Storage)?;

        // If the invoice is being created already-paid (replay path), emit
        // the paid fact too — but ONLY when `payment_method` is unset.
        // When the caller supplies a method (sim's two-phase bank-clearing
        // flow), the bank-settlement POST is responsible for emitting
        // `finance.payment.received` + creating the projection row, so
        // double-posting `finance.invoice.paid` here would credit A/R
        // twice.
        // The unique index handles the double-emission case where
        // mark_invoice_paid also fires later.
        if inv.status.is_paid()
            && let Some(paid_on) = inv.paid_on
            && inv.payment_method.is_none()
        {
            let paid_payload = serde_json::json!({
                "invoice_id": inv.id,
                "account_id": inv.account_id,
                "amount_cents": inv.amount_cents,
                "currency": inv.currency,
                "paid_on": paid_on,
            });
            insert_fact(
                &mut tx,
                "finance.invoice.paid",
                paid_on,
                &paid_payload,
                "invoices",
                &inv.id,
            )
            .await?;
        }

        tx.commit()
            .await
            .map_err(|e| CommerceError::Storage(e.to_string()))?;

        // enriched_invoice (assembled above, before the fact write) is
        // exactly what the handler emits as the commerce.invoice.created
        // event — the same shape the finance.invoice.issued fact persists,
        // so audit_log replay reconstructs the identical fact (incl. COGS
        // legs).
        Ok(enriched_invoice)
    }

    async fn mark_invoice_paid_at(
        &self,
        id: &str,
        paid_on: chrono::NaiveDate,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), CommerceError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| CommerceError::Storage(e.to_string()))?;

        // Status flip + paid_on stamp. We DON'T emit a
        // `finance.invoice.paid` fact from this path: the
        // canonical AR drain runs through the two-phase
        // bank-clearing chain — `ledger.payment.received` debits
        // 1010 Cash-in-Transit + credits 1100 AR; settled flips
        // 1010 → 1000. Emitting finance.invoice.paid here would
        // ALSO credit AR (via BossRuleSet::invoice_paid's
        // DR Cash / CR AR shortcut), and the live brewery hit
        // exactly this — AR went structurally negative -$6.5M
        // because every payment got AR-credited twice.
        //
        // The create-already-paid path keeps emitting the fact
        // (single-shot tenants who don't model bank float) but only
        // when `payment_method` is unset; once a tenant uses the
        // two-phase chain, neither path emits and the bank-settlement
        // is the sole AR drain.
        //
        // Tenants that need the single-shot DR Cash CR AR rule
        // can still author the fact directly via
        // `record_fact_in_tx`; we just don't auto-emit one for
        // every PUT /paid.
        //
        // Guarded by the transition rule: an already-paid invoice keeps
        // its first paid_on and records nothing, and a written-off one
        // is refused — it re-stamped both before (backlog 203ef806).
        if !transition_in_tx(&mut tx, id, InvoiceStatus::PAID, Some(paid_on)).await? {
            return Ok(());
        }

        // OUTBOX (phase 2): record commerce.invoice.paid with the full
        // post-update row state (the shape rebuild.rs consumes via
        // from_value::<Invoice>) in the SAME transaction as the flip.
        let invoice = fetch_invoice_in_tx(&mut tx, id).await?;
        let event = stamp.event(
            crate::events::INVOICE_PAID,
            serde_json::to_value(&invoice).unwrap_or_default(),
        );
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(CommerceError::Storage)?;

        tx.commit()
            .await
            .map_err(|e| CommerceError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn mark_invoice_past_due(
        &self,
        id: &str,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), CommerceError> {
        // No financial fact / journal entry on past-due — it's a
        // status flip used for reporting + collections workflow,
        // not a posting event. The original revenue accrual on
        // INVOICE_CREATED already debited A/R; past-due just
        // ages it. The commerce.invoice.past_due EVENT still records
        // (the rebuild path flips the projection from it) — in the
        // same transaction as the flip, per outbox phase 2.
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| CommerceError::Storage(e.to_string()))?;
        if !transition_in_tx(&mut tx, id, InvoiceStatus::PAST_DUE, None).await? {
            return Ok(());
        }
        let invoice = fetch_invoice_in_tx(&mut tx, id).await?;
        let event = stamp.event(
            crate::events::INVOICE_PAST_DUE,
            serde_json::to_value(&invoice).unwrap_or_default(),
        );
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(CommerceError::Storage)?;
        tx.commit()
            .await
            .map_err(|e| CommerceError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn mark_invoice_written_off(
        &self,
        id: &str,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<bool, CommerceError> {
        // Flip to terminal `written-off` AND record the
        // `finance.invoice.written_off` fact that posts DR 6700 Bad Debt
        // Expense / CR 1100 A/R. The fact is written LIVE here — exactly
        // like the `finance.invoice.issued` fact written at creation —
        // because live facts come only from explicit `insert_fact` calls;
        // `gl_fact_projection_rules` is the REBUILD path. A previous
        // version posted nothing here on the theory that "the projection
        // path owns it," but that path runs only in rebuild-all, so the
        // live ledger silently dropped every write-off (live 6700 = 0)
        // while the rebuild reconstructed them — a determinism gap.
        //
        // `happened_on` is the invoice's `issued_on`, matching the
        // projection rule's `/issued_on` happened_on path, so the live
        // and rebuilt postings land on the same date and reconcile to
        // the cent.
        //
        // The transition gate makes double delivery converge: the
        // write-off drive arrives once per past-due copy the
        // counterparty received, and only the copy that wins the
        // UPDATE writes the fact — the row lock serializes the rest
        // into `transition_in_tx`'s already-written-off answer.
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| CommerceError::Storage(e.to_string()))?;
        if !transition_in_tx(&mut tx, id, InvoiceStatus::WRITTEN_OFF, None).await? {
            return Ok(false);
        }
        // Fetch the full written-off invoice in-tx so the live fact payload
        // is byte-identical to the commerce.invoice.written_off event the
        // handler emits (`to_value` of the same invoice fetched post-flip).
        // The write-off rule reads only amount_cents, but the event must
        // carry the full Invoice (rebuild.rs `from_value::<Invoice>` flips
        // the invoices projection), so the fact grows to match it.
        let row: InvoiceRow = sqlx::query_as("SELECT * FROM invoices WHERE id = $1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| CommerceError::Storage(e.to_string()))?;
        let mut invoice = row.into_invoice();
        let lines: Vec<LineItemRow> = sqlx::query_as(
            "SELECT id, invoice_id, revenue_category, amount_cents, currency, description, ref_id, \
                    sku, qty, cost_basis_cents, cost_total_cents \
             FROM invoice_line_items WHERE invoice_id = $1 ORDER BY id",
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| CommerceError::Storage(e.to_string()))?;
        invoice.line_items = lines.into_iter().map(|l| l.into_line_item()).collect();
        let payload = serde_json::to_value(&invoice).unwrap_or_default();
        insert_fact(
            &mut tx,
            "finance.invoice.written_off",
            invoice.issued_on,
            &payload,
            "invoices",
            &invoice.id,
        )
        .await?;
        // OUTBOX (phase 2): same payload as the fact, recorded in the
        // same transaction, structurally gated on the flip winning —
        // the emit-exactly-once contract the handler used to enforce
        // by checking the returned bool.
        let event = stamp.event(crate::events::INVOICE_WRITTEN_OFF, payload.clone());
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(CommerceError::Storage)?;
        tx.commit()
            .await
            .map_err(|e| CommerceError::Storage(e.to_string()))?;
        Ok(true)
    }

    async fn invoice_summary(
        &self,
        today: chrono::NaiveDate,
    ) -> Result<InvoiceSummary, CommerceError> {
        // Trailing-12-months revenue grouped by revenue account.
        // Source: gl_journal_lines × gl_accounts. Revenue accounts
        // are `kind = 'revenue'`; their credit_cents sums to the
        // recognized revenue for the period. Sourcing from the
        // ledger (not invoice_line_items) protects against drift —
        // any manual journal entry or adjusting entry to a revenue
        // account shows up here, and a malformed invoice that
        // failed to post to the GL doesn't double-count.
        //
        // The category label IS the GL account name. We used to
        // run `revenue_category_for_account_code` to invert the
        // ledger's mapping into a category code, but that
        // hardcoded the device-shop's category vocabulary
        // (new-sales / used-sales / contracts) — when the brewery
        // posts to the same chart codes, it showed up under the
        // wrong names. Reading the account name keeps the label
        // tenant-shaped without any inverse mapping.
        // Exclude `finance.period.closed` lines from the revenue
        // tally. Those entries DR-revenue / CR-retained-earnings at
        // year-end — bookkeeping movement, not negative revenue.
        // Without this filter, TTM windows that straddle a close-out
        // date can flip a low-volume category (e.g. Seasonal &
        // Specialty in the brewery) net-negative because the close
        // debit exceeds the post-close credits. Trial balance + the
        // income statement aggregate over the full ledger and don't
        // hit this because they don't filter by a sliding window.
        let revenue_rows: Vec<(String, String, i64)> = sqlx::query_as(
            "SELECT a.code, a.name, COALESCE(SUM(l.credit_cents - l.debit_cents), 0)::bigint \
             FROM gl_journal_lines l \
             JOIN gl_accounts a ON l.account_id = a.id \
             JOIN gl_journal_entries e ON l.journal_entry_id = e.id \
             JOIN financial_facts f ON e.fact_id = f.id \
             WHERE a.kind = 'revenue' \
               AND e.posted_on >= $1::date - INTERVAL '12 months' \
               AND f.kind <> 'finance.period.closed' \
             GROUP BY a.code, a.name \
             ORDER BY a.code",
        )
        .bind(today)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CommerceError::Storage(e.to_string()))?;

        // Real COGS for the TTM window.
        //
        // The `finance.cogs.recognized` payload carries the
        // originating `revenue_category` (e.g. "wholesale", "retail"),
        // tagged at consume time by the shipment step's
        // `consumes_products` array (see boss-products consume() +
        // boss-inventory-sim-bridge `ProductsConsumeEmitter`). We sum
        // tagged COGS exactly per category and pro-rate only the
        // untagged remainder by revenue share, so a category gets an
        // exact margin whenever its COGS facts carry the tag.
        let total_revenue_ttm_cents: i64 = revenue_rows.iter().map(|(_, _, r)| *r).sum();
        let total_cogs_ttm_cents: i64 = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(SUM(l.debit_cents - l.credit_cents), 0)::bigint \
             FROM gl_journal_lines l \
             JOIN gl_accounts a ON l.account_id = a.id \
             JOIN gl_journal_entries e ON l.journal_entry_id = e.id \
             JOIN financial_facts f ON e.fact_id = f.id \
             WHERE a.code LIKE '5%' \
               AND e.posted_on >= $1::date - INTERVAL '12 months' \
               AND f.kind <> 'finance.period.closed'",
        )
        .bind(today)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| CommerceError::Storage(e.to_string()))?;

        // Tagged COGS — sum cents from `finance.cogs.recognized`
        // facts whose payload carries a `revenue_category`.
        // GROUP BY the tag so each category gets exact COGS.
        let tagged_cogs_rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT \
                f.payload->>'revenue_category' AS revenue_category, \
                COALESCE(SUM(((f.payload->>'total_cost_cents')::bigint)), 0)::bigint \
             FROM financial_facts f \
             WHERE f.kind = 'finance.cogs.recognized' \
               AND f.happened_on >= $1::date - INTERVAL '12 months' \
               AND f.payload ? 'revenue_category' \
             GROUP BY f.payload->>'revenue_category'",
        )
        .bind(today)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CommerceError::Storage(e.to_string()))?;

        // Invert the ledger's category → account map so we can find,
        // for each revenue account row, which category tags belong
        // to it ("retail" + "merchandise" both → 4110).
        let category_to_account = boss_ledger::revenue_accounts_map();
        let mut account_to_categories: std::collections::HashMap<&str, Vec<&str>> =
            std::collections::HashMap::new();
        for (category, account_code) in category_to_account.iter() {
            account_to_categories
                .entry(account_code)
                .or_default()
                .push(category.as_str());
        }

        let tagged_cogs_by_category: std::collections::HashMap<String, i64> =
            tagged_cogs_rows.into_iter().collect();
        let total_tagged_cogs_cents: i64 = tagged_cogs_by_category.values().copied().sum();
        let untagged_cogs_cents: i64 = (total_cogs_ttm_cents - total_tagged_cogs_cents).max(0);
        // Untagged remainder is the revenue total whose categories
        // have no tagged COGS attached — only those revenue rows
        // share in the pro-rated pool. Revenue rows with any tagged
        // COGS get the exact amount and contribute zero to the
        // pro-rated denominator.
        let untagged_revenue_total: i64 = revenue_rows
            .iter()
            .filter(|(code, _, _)| {
                account_to_categories
                    .get(code.as_str())
                    .map(|cats| {
                        !cats
                            .iter()
                            .any(|c| tagged_cogs_by_category.contains_key(*c))
                    })
                    .unwrap_or(true)
            })
            .map(|(_, _, r)| *r)
            .sum();

        let revenue_ttm: Vec<CategoryMargin> = revenue_rows
            .into_iter()
            .map(|(code, name, revenue_cents)| {
                let category = name;
                // Per-row COGS = (tagged COGS for any category mapped
                // to this account) + (pro-rated share of untagged
                // COGS by revenue mix). If the account has at least
                // one tagged category, the pro-rated share is zero.
                let categories = account_to_categories
                    .get(code.as_str())
                    .cloned()
                    .unwrap_or_default();
                let tagged_for_row: i64 = categories
                    .iter()
                    .filter_map(|c| tagged_cogs_by_category.get(*c).copied())
                    .sum();
                let has_tagged = categories
                    .iter()
                    .any(|c| tagged_cogs_by_category.contains_key(*c));
                let pro_rated_cogs = if !has_tagged && untagged_revenue_total > 0 {
                    ((untagged_cogs_cents as i128) * (revenue_cents as i128)
                        / (untagged_revenue_total as i128)) as i64
                } else {
                    0
                };
                let cogs_cents = tagged_for_row + pro_rated_cogs;
                let gross_margin_cents = revenue_cents - cogs_cents;
                let margin_pct = if revenue_cents > 0 {
                    (gross_margin_cents as f64 / revenue_cents as f64 * 100.0).round() as i64
                } else {
                    0
                };
                CategoryMargin {
                    category,
                    revenue_cents,
                    cogs_cents,
                    gross_margin_cents,
                    margin_pct,
                }
            })
            .collect();

        let total_gross_margin_ttm_cents: i64 = total_revenue_ttm_cents - total_cogs_ttm_cents;

        // AR aging: every OWED invoice, summed here per days-past-due
        // and bucketed by `ArAgingBucket::age`. It filtered
        // `status <> 'paid'` and so aged written-off invoices as
        // outstanding (backlog 926d64a3); the not-owed statuses are now
        // bound from the one Rust list, as `open_ar_by_account` binds
        // them. One row per distinct due date, so the sum stays exact
        // at any volume.
        let not_owed: Vec<String> = InvoiceStatus::NOT_OWED
            .iter()
            .map(|s| s.to_string())
            .collect();
        let aging_rows: Vec<(i64, i64, i64)> = sqlx::query_as(
            "SELECT \
                ($1::date - due_on)::bigint AS days_past_due, \
                COUNT(*)::bigint, \
                COALESCE(SUM(amount_cents), 0)::bigint \
             FROM invoices \
             WHERE status <> ALL($2) \
             GROUP BY 1",
        )
        .bind(today)
        .bind(&not_owed)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CommerceError::Storage(e.to_string()))?;
        let ar_aging = ArAgingBucket::age(aging_rows);

        let total_outstanding_cents: i64 = ar_aging.iter().map(|b| b.total_cents).sum();

        let total_invoice_count: i64 = sqlx::query_scalar("SELECT COUNT(*)::bigint FROM invoices")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| CommerceError::Storage(e.to_string()))?;

        // Monthly revenue for the last 12 months, oldest-first. The Pulse
        // panel uses the last entry for MTD and the prior entry for pace
        // comparison. Sourced from the GL — each invoice-issued fact
        // maps to exactly one journal entry, so `COUNT(DISTINCT e.fact_id)`
        // in revenue-producing entries matches invoice count without
        // double-counting adjusting entries. `SUM(credit_cents -
        // debit_cents)` on revenue accounts handles refunds cleanly
        // (a refund debits revenue, reducing the month's net).
        let monthly_rows: Vec<(chrono::NaiveDate, i64, i64)> = sqlx::query_as(
            "SELECT \
                date_trunc('month', e.posted_on)::date as month, \
                COALESCE(SUM(l.credit_cents - l.debit_cents), 0)::bigint as revenue_cents, \
                COUNT(DISTINCT e.fact_id)::bigint as invoice_count \
             FROM gl_journal_lines l \
             JOIN gl_accounts a ON l.account_id = a.id \
             JOIN gl_journal_entries e ON l.journal_entry_id = e.id \
             WHERE a.kind = 'revenue' \
               AND e.posted_on >= $1::date - INTERVAL '12 months' \
             GROUP BY 1 \
             ORDER BY 1 ASC",
        )
        .bind(today)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CommerceError::Storage(e.to_string()))?;
        let revenue_by_month: Vec<MonthlyRevenue> = monthly_rows
            .into_iter()
            .map(|(month, revenue_cents, invoice_count)| MonthlyRevenue {
                month: month.to_string(),
                revenue_cents,
                invoice_count,
            })
            .collect();

        Ok(InvoiceSummary {
            revenue_ttm,
            total_revenue_ttm_cents,
            total_cogs_ttm_cents,
            total_gross_margin_ttm_cents,
            ar_aging,
            total_outstanding_cents,
            total_invoice_count,
            revenue_by_month,
            currency: "USD".to_string(),
        })
    }
}

/// The status write all three status verbs run (backlog 203ef806):
/// `InvoiceStatus::transition_to`, applied in the UPDATE's own WHERE so
/// the check and the write are one statement under the row lock — the
/// past-due flip had no guard at all and moved paid and written-off
/// invoices back into the receivable. `true` when THIS call moved the
/// invoice (the caller then records its event); `false` when it was
/// already at `to` (a redelivered drive — no event); a terminal source
/// is refused by name, never answered as a silent 0-row update. The
/// not-owed statuses are bound from the one Rust list.
async fn transition_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: &str,
    to: &str,
    paid_on: Option<chrono::NaiveDate>,
) -> Result<bool, CommerceError> {
    let not_owed: Vec<String> = InvoiceStatus::NOT_OWED
        .iter()
        .map(|s| s.to_string())
        .collect();
    let updated = sqlx::query(
        "UPDATE invoices SET status = $2, paid_on = COALESCE($3, paid_on) \
         WHERE id = $1 AND status <> $2 AND status <> ALL($4)",
    )
    .bind(id)
    .bind(to)
    .bind(paid_on)
    .bind(&not_owed)
    .execute(&mut **tx)
    .await
    .map_err(|e| CommerceError::Storage(e.to_string()))?;
    if updated.rows_affected() > 0 {
        return Ok(true);
    }
    let from: Option<String> = sqlx::query_scalar("SELECT status FROM invoices WHERE id = $1")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| CommerceError::Storage(e.to_string()))?;
    let Some(from) = from else {
        return Err(CommerceError::NotFound(format!("invoice {id}")));
    };
    match InvoiceStatus::new(from.clone()).transition_to(to) {
        InvoiceTransition::Already => Ok(false),
        InvoiceTransition::Refused => Err(CommerceError::refused_transition(id, &from, to)),
        // Only a write between the UPDATE and this read lands here.
        InvoiceTransition::Flip => Err(CommerceError::Conflict(format!(
            "invoice {id} moved to '{from}' while being written '{to}'; retry"
        ))),
    }
}

/// Fetch the full invoice (header + line items) INSIDE the caller's
/// transaction — the post-update row state the mark_* paths record as
/// their event payload (rebuild consumes it via from_value::<Invoice>).
/// The pool-based `invoice_by_id` can't see uncommitted updates; this
/// can, and that's the point.
async fn fetch_invoice_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: &str,
) -> Result<Invoice, CommerceError> {
    let row: InvoiceRow = sqlx::query_as("SELECT * FROM invoices WHERE id = $1")
        .bind(id)
        .fetch_one(&mut **tx)
        .await
        .map_err(|e| CommerceError::Storage(e.to_string()))?;
    let mut invoice = row.into_invoice();
    let lines: Vec<LineItemRow> = sqlx::query_as(
        "SELECT id, invoice_id, revenue_category, amount_cents, currency, description, ref_id,                 sku, qty, cost_basis_cents, cost_total_cents          FROM invoice_line_items WHERE invoice_id = $1 ORDER BY id",
    )
    .bind(id)
    .fetch_all(&mut **tx)
    .await
    .map_err(|e| CommerceError::Storage(e.to_string()))?;
    invoice.line_items = lines.into_iter().map(|l| l.into_line_item()).collect();
    Ok(invoice)
}

async fn insert_fact(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    kind: &str,
    happened_on: chrono::NaiveDate,
    payload: &serde_json::Value,
    source_table: &str,
    source_id: &str,
) -> Result<(), CommerceError> {
    // INSERT ... ON CONFLICT ... RETURNING only returns a row on new
    // inserts, so the conflict path gets an empty result. We upsert + then
    // look up the id separately so both paths produce the same fact_id.
    // created_by carries fact provenance at service granularity — the
    // same fallback the gl_fact_projection_rules engine uses when a
    // rule has no created_by_path (event.source). The old schema
    // DEFAULT 'system' silently swallowed the missing bind.
    sqlx::query(
        "INSERT INTO financial_facts \
            (id, kind, happened_on, payload, source_table, source_id, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, 'commerce') \
         ON CONFLICT (kind, source_table, source_id) DO NOTHING",
    )
    .bind(boss_ledger::deterministic_fact_id(
        kind,
        source_table,
        source_id,
    ))
    .bind(kind)
    .bind(happened_on)
    .bind(payload)
    .bind(source_table)
    .bind(source_id)
    .execute(&mut **tx)
    .await
    .map_err(|e| CommerceError::Storage(e.to_string()))?;

    let (fact_id,): (Uuid,) = sqlx::query_as(
        "SELECT id FROM financial_facts \
         WHERE kind = $1 AND source_table = $2 AND source_id = $3",
    )
    .bind(kind)
    .bind(source_table)
    .bind(source_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| CommerceError::Storage(e.to_string()))?;

    let fact_ref = boss_ledger::FactRef {
        id: fact_id,
        kind,
        happened_on,
        payload,
    };
    boss_ledger::post_fact_in_tx(tx, &fact_ref)
        .await
        .map_err(|e| CommerceError::Storage(format!("ledger post failed: {e}")))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Row types
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct InvoiceRow {
    id: String,
    account_id: String,
    issued_on: chrono::NaiveDate,
    due_on: chrono::NaiveDate,
    paid_on: Option<chrono::NaiveDate>,
    status: String,
    amount_cents: i64,
    currency: String,
    #[sqlx(default)]
    tax_cents: i64,
    #[sqlx(default)]
    tax_jurisdiction: Option<String>,
    #[sqlx(default)]
    payment_method: Option<String>,
}

impl InvoiceRow {
    fn into_invoice(self) -> Invoice {
        Invoice {
            id: self.id,
            account_id: self.account_id,
            issued_on: self.issued_on,
            due_on: self.due_on,
            paid_on: self.paid_on,
            // Free-text Class code; the column holds the kebab string,
            // so the newtype wraps it as-is.
            status: InvoiceStatus::new(self.status),
            amount_cents: self.amount_cents,
            currency: self.currency,
            tax_cents: self.tax_cents,
            tax_jurisdiction: self.tax_jurisdiction,
            payment_method: self.payment_method,
            line_items: Vec::new(),
        }
    }
}

#[derive(sqlx::FromRow)]
struct LineItemRow {
    id: String,
    invoice_id: String,
    revenue_category: String,
    amount_cents: i64,
    currency: String,
    description: String,
    ref_id: Option<String>,
    sku: Option<String>,
    qty: Option<i32>,
    cost_basis_cents: Option<i64>,
    cost_total_cents: Option<i64>,
}

impl LineItemRow {
    fn into_line_item(self) -> InvoiceLineItem {
        InvoiceLineItem {
            id: self.id,
            invoice_id: self.invoice_id,
            revenue_category: RevenueCategory::from(self.revenue_category),
            amount_cents: self.amount_cents,
            currency: self.currency,
            description: self.description,
            ref_id: self.ref_id,
            sku: self.sku,
            qty: self.qty,
            cost_basis_cents: self.cost_basis_cents,
            cost_total_cents: self.cost_total_cents,
        }
    }
}

#[derive(sqlx::FromRow)]
struct RevenueLineRow {
    month: chrono::NaiveDate,
    category: String,
    amount_cents: i64,
}

impl RevenueLineRow {
    fn into_revenue_line(self) -> RevenueLine {
        RevenueLine {
            month: self.month,
            category: RevenueCategory::from(self.category),
            amount_cents: self.amount_cents,
            currency: "USD".to_string(),
        }
    }
}

// InvoiceStatus and RevenueCategory are both newtypes around String
// accepting arbitrary values, so callers wrap the database column
// directly (`InvoiceStatus::new(self.status)` /
// `RevenueCategory::from(self.revenue_category)`) — no parse step.
// Status values are validated against the Class registry at the
// commerce API boundary, not in this storage adapter.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invoice_status_round_trips_through_serde() {
        // Transparent newtype serializes to the bare kebab code the
        // column stores and round-trips back.
        for code in [
            InvoiceStatus::PAID,
            InvoiceStatus::OUTSTANDING,
            InvoiceStatus::PAST_DUE,
            InvoiceStatus::WRITTEN_OFF,
        ] {
            let st = InvoiceStatus::new(code);
            assert_eq!(st.as_str(), code);
            let json = serde_json::to_string(&st).unwrap();
            assert_eq!(json, format!("\"{code}\""));
            let back: InvoiceStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(back, st);
        }
    }
}
