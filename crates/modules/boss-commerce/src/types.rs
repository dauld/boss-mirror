//! Commerce domain types — invoices and revenue lines.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

pub type InvoiceId = String;
pub type AccountId = String;

fn default_currency() -> String {
    "USD".to_string()
}

/// Where an invoice sits in its AR lifecycle. Free-text wrapper around
/// a kebab-case string; the four platform statuses are seeded as Class
/// rows under `(subject_kind='invoice', member_attribute='status')` and
/// a tenant extends the lifecycle by adding a row, not forking core.
/// The commerce API validates an incoming status against the active
/// Class set at the invoice-create boundary (fail-loud → 400).
/// Serializes transparently to the bare string; the `invoices.status`
/// column stores it directly. See docs/design/class-registry.md.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InvoiceStatus(pub String);

impl InvoiceStatus {
    pub const PAID: &'static str = "paid";
    pub const OUTSTANDING: &'static str = "outstanding";
    pub const PAST_DUE: &'static str = "past-due";
    /// Account never paid + uncollectable. The receivable was
    /// written off via `commerce.invoice.written_off` which posts
    /// DR 6700 Bad Debt Expense / CR 1100 A/R. Terminal state — no
    /// further transitions.
    pub const WRITTEN_OFF: &'static str = "written-off";

    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True when the invoice has settled. Drives the
    /// AR-aging-excludes-paid split and the sim's collections branch.
    pub fn is_paid(&self) -> bool {
        self.0 == Self::PAID
    }

    /// The statuses that are no longer owed: paid, and written off —
    /// the write-off credits 1100 A/R, so the receivable is gone even
    /// though no cash came in. Every other status, a tenant's own
    /// included, is still owed. One list, read by both adapters of
    /// `open_ar_by_account` (backlog 5257bfa9) and of the summary's
    /// AR aging (backlog 926d64a3) — the SPA's Finance InvoicesTab
    /// draws "outstanding" with the same two out.
    pub const NOT_OWED: [&'static str; 2] = [Self::PAID, Self::WRITTEN_OFF];

    /// True while the invoice is still a receivable.
    pub fn is_owed(&self) -> bool {
        !Self::NOT_OWED.contains(&self.0.as_str())
    }

    /// The invoice transition rule, stated once (backlog 203ef806): what
    /// a status write to `to` does to an invoice at this status. An
    /// invoice leaves the receivable once — every owed status, a
    /// tenant's own included, may move to any other; a status in
    /// `NOT_OWED` is terminal, because paid and written-off have both
    /// already taken the amount out of 1100 A/R and a flip back would
    /// count it owed a second time. Writing the status it already has
    /// is a redelivered drive and converges. Both adapters of the three
    /// status verbs enforce this — Pg in the UPDATE's own WHERE — and
    /// `tests/invoice_transitions.rs` holds both to the same table:
    ///
    /// | from \ to    | paid    | past-due | written-off |
    /// |--------------|---------|----------|-------------|
    /// | outstanding  | flip    | flip     | flip        |
    /// | past-due     | flip    | already  | flip        |
    /// | paid         | already | REFUSED  | REFUSED     |
    /// | written-off  | REFUSED | REFUSED  | already     |
    /// | (tenant, owed) | flip  | flip     | flip        |
    pub fn transition_to(&self, to: &str) -> InvoiceTransition {
        if self.0 == to {
            InvoiceTransition::Already
        } else if self.is_owed() {
            InvoiceTransition::Flip
        } else {
            InvoiceTransition::Refused
        }
    }
}

/// What `InvoiceStatus::transition_to` decides for one status write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvoiceTransition {
    /// The invoice moves, and the move records its event.
    Flip,
    /// Already at the target: Ok, nothing changes, no event.
    Already,
    /// The source is terminal: refused by name, nothing changes.
    Refused,
}

impl std::fmt::Display for InvoiceStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for InvoiceStatus {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for InvoiceStatus {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Per-line revenue bucket. Open-string newtype so tenants can
/// declare their own categories as data without recompiling the
/// platform. Values are kebab-case ("wholesale", "taproom",
/// "event-package"); the postgres column is `TEXT NOT NULL` and
/// the wire JSON is the bare string (`#[serde(transparent)]`).
///
/// Downstream consumers (ledger posting rules in
/// `boss_ledger::rules`) decide how to bucket each value into
/// chart-of-accounts codes; they fail-open on unknown values
/// rather than rejecting at the type system. That's the
/// trade-off vs the prior closed enum — broader tenant support
/// at the cost of "compiler caught my typo." The typo is instead
/// caught at the write boundary: `create_invoice`/`batch_invoices`
/// validate every line's category against the Class registry keyed
/// `(subject_kind='invoice', code)`, so an unregistered category is a
/// 400, not a silent uncategorized bucket. The `invoice` Classes are
/// tenant-curated (the brewery's live in classes.json, mirroring
/// tenant.toml's `[labels]` for display).
///
/// Construct from string literals via
/// `RevenueCategory::from("wholesale")` or
/// `RevenueCategory("wholesale".into())`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RevenueCategory(pub String);

impl RevenueCategory {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for RevenueCategory {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for RevenueCategory {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl std::fmt::Display for RevenueCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One billable item on an invoice. Each line carries its own
/// revenue category so a single invoice can mix new-device sale +
/// service contract + training visit in one document, which is how
/// real Boss billing works. `ref_id` links the line back to the
/// source (opportunity, work order, agreement) that triggered it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvoiceLineItem {
    pub id: String,
    pub invoice_id: InvoiceId,
    pub revenue_category: RevenueCategory,
    pub amount_cents: i64,
    #[serde(default = "default_currency")]
    pub currency: String,
    pub description: String,
    pub ref_id: Option<String>,
    /// FG SKU this line bills. When set, the commerce
    /// HTTP handler looks up the SKU's weighted cost basis and
    /// embeds it as `cost_basis_cents` so the `invoice_issued`
    /// posting rule can draw matching COGS in the same JE.
    /// `None` for non-FG lines (service work, contracts,
    /// reimbursements) — revenue without COGS is the correct
    /// shape there.
    #[serde(default)]
    pub sku: Option<String>,
    /// Units billed for this line. Required when `sku` is set;
    /// drives both the FG drawdown qty and the cost_basis × qty
    /// COGS computation.
    #[serde(default)]
    pub qty: Option<i32>,
    /// Per-unit production cost basis at invoice-creation time —
    /// display only under value-primary costing (derived
    /// drain / qty, rounded). The exact COGS amount is
    /// `cost_total_cents`; the posting rule prefers it. `None`
    /// for non-FG lines.
    #[serde(default)]
    pub cost_basis_cents: Option<i64>,
    /// The line's EXACT COGS total in cents — the value this
    /// invoice's FG drawdown drained from the row (PR 6a,
    /// value-primary). The `invoice_issued` posting rule sizes the
    /// DR 5100 / CR 1320 leg from this, so the GL credit equals the
    /// physical value delta to the cent (qty × per-unit rounding
    /// re-introduced the leak this workstream closes). `None` for
    /// non-FG lines.
    #[serde(default)]
    pub cost_total_cents: Option<i64>,
}

impl From<&InvoiceLineItem> for boss_core::primitives::Part {
    /// Line items are AttributeParts of their parent Invoice Subject.
    /// No separate identity / event stream — the line belongs to
    /// exactly one invoice for its lifetime.
    fn from(line: &InvoiceLineItem) -> Self {
        boss_core::primitives::Part::attribute(
            "line_item",
            serde_json::to_value(line).expect("InvoiceLineItem serialises"),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Invoice {
    pub id: InvoiceId,
    pub account_id: AccountId,
    pub issued_on: NaiveDate,
    pub due_on: NaiveDate,
    pub paid_on: Option<NaiveDate>,
    pub status: InvoiceStatus,
    /// Total charged to the account, revenue + sales tax. Invariant
    /// enforced in the adapter: `sum(line_items.amount_cents) +
    /// tax_cents == amount_cents`. A tax-free invoice has tax_cents=0,
    /// so the invariant reduces to `line_sum == amount_cents`.
    pub amount_cents: i64,
    #[serde(default = "default_currency")]
    pub currency: String,
    /// Sales tax charged on this invoice — additive to the line-item
    /// revenue total. Zero when the account's state has no sales tax
    /// (OR/NH/MT/DE/AK) or when the invoice is exempt.
    #[serde(default)]
    pub tax_cents: i64,
    /// Filing jurisdiction the tax was collected under (`US-CA`,
    /// `US-TX`, ...). `None` on zero-tax invoices.
    #[serde(default)]
    pub tax_jurisdiction: Option<String>,
    /// How the account paid this invoice (`ach` / `wire` / `check` /
    /// `card`). `None` until the invoice flips to Paid. When set, the
    /// commerce adapter skips the `finance.invoice.paid` shortcut and
    /// defers fact emission to the ledger-side bank-settlement POST
    /// so the two-phase flow (`payment.received` → `payment.settled`)
    /// is the only journal path.
    #[serde(default)]
    pub payment_method: Option<String>,
    /// Non-empty on writes that came through the line-item API.
    /// Serialized inline so callers see the full document.
    #[serde(default)]
    pub line_items: Vec<InvoiceLineItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RevenueLine {
    pub month: NaiveDate,
    pub category: RevenueCategory,
    pub amount_cents: i64,
    #[serde(default = "default_currency")]
    pub currency: String,
}

// ---------------------------------------------------------------------------
// Aggregated summary types for the Finance dashboard. Returned by the
// /api/commerce/summary endpoint so the UI can render headline stats
// without downloading every invoice.
// ---------------------------------------------------------------------------

/// One account's open receivables, summed by the service over every
/// invoice it holds — `GET /api/commerce/open-ar`. /ux/accounts read
/// the invoice LIST and summed it in the browser, and the list hands
/// over at most 1,000 rows, so past that the figure was short with
/// nothing on the page saying so (backlog 5257bfa9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountOpenAr {
    pub account_id: AccountId,
    /// Sum of `amount_cents` over the account's owed invoices.
    pub open_ar_cents: i64,
    /// How many invoices that sum covers.
    pub open_count: i64,
}

/// One AR aging bucket: how many unpaid invoices and how much outstanding
/// within a days-past-due range.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArAgingBucket {
    /// Bucket label: `current`, `1-30`, `31-60`, `61-90`, `90+`.
    pub label: String,
    pub count: i64,
    pub total_cents: i64,
}

impl ArAgingBucket {
    /// Every label, in the order the summary emits them — all five
    /// even when empty, so the frontend always sees the same shape.
    pub const LABELS: [&'static str; 5] = ["current", "1-30", "31-60", "61-90", "90+"];

    /// The bucket an owed invoice falls in, by days past its due date.
    /// `current` is due in the future or due today.
    pub fn label_for(days_past_due: i64) -> &'static str {
        match days_past_due {
            ..=0 => "current",
            1..=30 => "1-30",
            31..=60 => "31-60",
            61..=90 => "61-90",
            _ => "90+",
        }
    }

    /// Age `(days_past_due, count, total_cents)` rows into the five
    /// buckets. The one bucketing rule both adapters of
    /// `invoice_summary` read (backlog 926d64a3): the Pg adapter used
    /// to spell the thresholds in a SQL `CASE` while the in-memory one
    /// returned no aging at all. The rows are the OWED invoices only —
    /// `InvoiceStatus::is_owed` decides which, never a status literal.
    pub fn age(rows: impl IntoIterator<Item = (i64, i64, i64)>) -> Vec<ArAgingBucket> {
        let sums = rows.into_iter().fold(
            std::collections::HashMap::<&str, (i64, i64)>::new(),
            |mut acc, (days, count, cents)| {
                let e = acc.entry(Self::label_for(days)).or_default();
                *e = (e.0 + count, e.1 + cents);
                acc
            },
        );
        Self::LABELS
            .iter()
            .map(|label| {
                let (count, total_cents) = sums.get(label).copied().unwrap_or_default();
                ArAgingBucket {
                    label: label.to_string(),
                    count,
                    total_cents,
                }
            })
            .collect()
    }
}

/// Per-category revenue + COGS + margin rollup. COGS percentages are
/// applied on the server so the Finance page doesn't have to hardcode
/// them client-side.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CategoryMargin {
    pub category: String,
    pub revenue_cents: i64,
    pub cogs_cents: i64,
    pub gross_margin_cents: i64,
    pub margin_pct: i64,
}

/// Revenue for a single calendar month. Used by the Exec dashboard's
/// Pulse panel to compute MTD pace vs prior-month totals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonthlyRevenue {
    /// ISO `YYYY-MM-01` string — first day of the month.
    pub month: String,
    pub revenue_cents: i64,
    pub invoice_count: i64,
}

/// Finance summary returned by `GET /api/commerce/summary`. All amounts
/// are SQL-aggregated across the full invoices table, so the numbers are
/// correct regardless of any pagination on the invoices list endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvoiceSummary {
    /// Trailing-12-months revenue sliced by category with COGS applied.
    /// Frontend shows this in the margin table and top-line revenue card.
    pub revenue_ttm: Vec<CategoryMargin>,
    /// Rollup totals across every `revenue_ttm` row, for the hero card.
    pub total_revenue_ttm_cents: i64,
    pub total_cogs_ttm_cents: i64,
    pub total_gross_margin_ttm_cents: i64,
    /// AR aging on every owed invoice in the system (not just the last
    /// 12 months) — `InvoiceStatus::is_owed`, so paid and written-off
    /// are out. Drives the receivables card and the overview table.
    pub ar_aging: Vec<ArAgingBucket>,
    pub total_outstanding_cents: i64,
    /// Total invoice count across all statuses. Lets the list view show
    /// "Showing N of {total_invoice_count}" instead of guessing.
    pub total_invoice_count: i64,
    /// Trailing 12 months of revenue bucketed by calendar month,
    /// ordered oldest to newest. Enables MTD and month-over-month
    /// comparisons without another round trip.
    pub revenue_by_month: Vec<MonthlyRevenue>,
    /// Reporting currency for all `*_cents` fields in this summary.
    #[serde(default = "default_currency")]
    pub currency: String,
}

#[cfg(test)]
mod transition_tests {
    use super::*;

    /// The rule's three answers, including a tenant's own status: owed,
    /// so it moves (backlog 203ef806). The whole table, on both
    /// adapters, is `tests/invoice_transitions.rs`.
    #[test]
    fn transition_to_refuses_only_out_of_a_terminal_status() {
        let from = |s: &str| InvoiceStatus::new(s);
        let t = |f: &str, to: &str| from(f).transition_to(to);
        use InvoiceTransition::*;
        assert_eq!(t(InvoiceStatus::OUTSTANDING, InvoiceStatus::PAST_DUE), Flip);
        assert_eq!(t("disputed", InvoiceStatus::WRITTEN_OFF), Flip);
        assert_eq!(t(InvoiceStatus::PAST_DUE, InvoiceStatus::PAST_DUE), Already);
        assert_eq!(t(InvoiceStatus::PAID, InvoiceStatus::PAST_DUE), Refused);
        assert_eq!(t(InvoiceStatus::WRITTEN_OFF, InvoiceStatus::PAID), Refused);
        assert_eq!(
            t(InvoiceStatus::WRITTEN_OFF, InvoiceStatus::WRITTEN_OFF),
            Already
        );
    }
}

#[cfg(test)]
mod ar_aging_tests {
    use super::*;

    /// The boundaries the Pg `CASE` used to spell (`<= 0`, `<= 30`,
    /// `<= 60`, `<= 90`, else), now the one Rust rule.
    #[test]
    fn label_for_matches_the_bucket_boundaries() {
        let cases = [
            (-5, "current"),
            (0, "current"),
            (1, "1-30"),
            (30, "1-30"),
            (31, "31-60"),
            (60, "31-60"),
            (61, "61-90"),
            (90, "61-90"),
            (91, "90+"),
        ];
        for (days, label) in cases {
            assert_eq!(ArAgingBucket::label_for(days), label, "{days} days");
        }
    }

    #[test]
    fn age_sums_rows_into_all_five_buckets_in_order() {
        let aging = ArAgingBucket::age([(0, 1, 100), (-3, 2, 50), (45, 1, 7), (400, 3, 9)]);
        let got: Vec<(&str, i64, i64)> = aging
            .iter()
            .map(|b| (b.label.as_str(), b.count, b.total_cents))
            .collect();
        assert_eq!(
            got,
            vec![
                ("current", 3, 150),
                ("1-30", 0, 0),
                ("31-60", 1, 7),
                ("61-90", 0, 0),
                ("90+", 3, 9),
            ]
        );
    }
}

#[cfg(test)]
mod part_conversion_tests {
    use super::*;
    use boss_core::primitives::Part;

    #[test]
    fn line_item_converts_to_attribute_part() {
        let line = InvoiceLineItem {
            id: "LINE-001".into(),
            invoice_id: "INV-2026-0042".into(),
            revenue_category: RevenueCategory::from("wholesale"),
            amount_cents: 125_000,
            currency: "USD".into(),
            description: "Networking switch base unit".into(),
            ref_id: Some("OPP-123".into()),
            sku: None,
            qty: None,
            cost_basis_cents: None,
            cost_total_cents: None,
        };
        let part: Part = (&line).into();
        match part {
            Part::Attribute { key, value } => {
                assert_eq!(key, "line_item");
                assert_eq!(value.get("id").and_then(|v| v.as_str()), Some("LINE-001"));
                assert_eq!(
                    value.get("amount_cents").and_then(|v| v.as_i64()),
                    Some(125_000),
                );
            }
            Part::Subject { .. } => panic!("line item should be AttributePart"),
        }
    }
}
