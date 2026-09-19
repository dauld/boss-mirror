//! Tax kinds and sales-tax rates as TENANT DATA (backlog 7f163e58;
//! design e187198f — the instance is the truth, the tenant contract
//! declares its chart; follows the chart's move, 41af5195).
//!
//! WHY. `tax_kinds` and `sales_tax_rate_by_state` were seeded by
//! 40-ledger.sql with the demo tenant's regime — five filing kinds and
//! the 27 states it ships into — and nothing else could declare a
//! kind: `POST /api/ledger/tax-accruals` refuses an unregistered one
//! with "register it in tax_kinds first", and the only way to register
//! one was a migration to the product. Worse, measured 2026-09-17:
//! each kind's FK pins a GL account (2150 / 2300 / 2310 / 2320 / 6500),
//! so the eviction that clears the demo chart off a real instance had
//! to KEEP those five under the demo's names, on every instance, for
//! ever. The regime is the tenant's — which kinds it files, against
//! which of ITS accounts, and which states it collects in — so the
//! tenant declares it in `seeds/tax.toml`, `boss tenant publish` sends
//! it through `POST /api/ledger/tax/batch` insert-if-absent, the demo
//! tenant carries the migration's rows in its own seed, and the five
//! accounts become candidates like the rest of the demo chart.
//!
//! A KIND NAMES AN ACCOUNT THE CHART MUST DECLARE. The table's FK holds
//! at the door; `boss tenant check` holds it in the directory, before
//! anything is sent: a kind naming a code `seeds/chart_of_accounts.toml`
//! does not declare is INVALID naming the kind and the code
//! ([`validate_against_chart`]). An empty file is valid — a tenant with
//! nothing sellable yet files no sales tax (design 18cf4272: the first
//! sellable SKU comes first) — and `boss tenant scaffold` writes one.
//!
//! ONE LOADER, TWO DOORS: check judges the file with [`parse_tax_toml`]
//! + [`validate`]; the batch door runs [`validate`] again on the wire
//! rows and refuses before any row is written. `derive_basis` is free
//! text here — the accrual door names an unknown basis loudly at use,
//! and the set it understands is that door's, not this loader's.

use std::collections::BTreeSet;

use boss_core::publish::KeptRow;
use serde::{Deserialize, Serialize};

/// One `tax_kinds` row a tenant batch inserted, plus `declared_by` —
/// the #418 shape: never per kept row, never per batch. Declared in
/// migration 20260918213157.
pub const TAX_KIND_DECLARED: &str = "ledger.tax_kind.declared";
/// One `sales_tax_rate_by_state` row a tenant batch inserted, the
/// same shape.
pub const SALES_TAX_RATE_DECLARED: &str = "ledger.sales_tax_rate.declared";

/// `sales_tax_rate_by_state.rate_bps`'s CHECK constraint, named here
/// so the refusal reads as a bound, not as a Postgres constraint name.
pub const RATE_BPS_MAX: i32 = 2000;

/// One `[[tax_kind]]` row: a filing kind, the liability account it
/// drains on remit, the expense account it accrues against (only a
/// kind that accrues has one), and how the accrual door derives its
/// amount. The table's authorable columns and nothing else — an
/// unknown field is refused by name rather than dropped in silence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaxKindInput {
    pub kind: String,
    pub liability_account: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expense_account: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derive_basis: Option<String>,
}

/// One `[[sales_tax_rate]]` row: the two-letter state, its filing
/// jurisdiction (`US-CA`), the flat rate in basis points.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SalesTaxRateInput {
    pub state: String,
    pub jurisdiction: String,
    pub rate_bps: i32,
}

/// `seeds/tax.toml` and the batch body: both tables, either may be
/// empty. A top-level table under any other name is refused naming
/// the two this reads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaxSeed {
    #[serde(default)]
    pub tax_kind: Vec<TaxKindInput>,
    #[serde(default)]
    pub sales_tax_rate: Vec<SalesTaxRateInput>,
}

impl TaxSeed {
    pub fn is_empty(&self) -> bool {
        self.tax_kind.is_empty() && self.sales_tax_rate.is_empty()
    }

    /// Every account code the kinds name, each once.
    pub fn account_codes(&self) -> BTreeSet<String> {
        self.tax_kind
            .iter()
            .flat_map(|k| {
                std::iter::once(k.liability_account.clone()).chain(k.expense_account.clone())
            })
            .collect()
    }
}

/// What a batch did: rows received across both tables, rows inserted,
/// and the rows kept as the registry already had them — each named
/// `tax_kind <kind>` / `sales_tax_rate <ST>` with the declared fields
/// it differs on, in the one shape every door answers in
/// (`boss_core::publish::KeptRow`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaxBatchOutcome {
    pub received: usize,
    pub inserted: usize,
    pub kept: Vec<KeptRow>,
}

fn bare(s: &str) -> bool {
    !s.trim().is_empty() && s.trim() == s
}

/// The door's own validation, run BEFORE any row is written and by
/// `boss tenant check` on the file: every kind and account code
/// non-empty with no surrounding whitespace, kinds unique, an expense
/// account never blank when given; every state two upper-case letters,
/// unique, with a non-empty jurisdiction and a rate inside the table's
/// CHECK (0 ..= [`RATE_BPS_MAX`]).
pub fn validate(seed: &TaxSeed) -> Result<(), String> {
    let mut kinds: Vec<&str> = Vec::with_capacity(seed.tax_kind.len());
    for (i, k) in seed.tax_kind.iter().enumerate() {
        let at = format!("tax kind #{} ({})", i + 1, k.kind);
        if !bare(&k.kind) {
            return Err(format!(
                "tax kind #{}: kind is empty or has surrounding whitespace",
                i + 1
            ));
        }
        if !bare(&k.liability_account) {
            return Err(format!(
                "{at}: liability_account is empty or has surrounding whitespace"
            ));
        }
        if let Some(e) = k.expense_account.as_deref()
            && !bare(e)
        {
            return Err(format!(
                "{at}: expense_account is empty or has surrounding whitespace — omit it for a kind that drains an existing liability"
            ));
        }
        if kinds.contains(&k.kind.as_str()) {
            return Err(format!("{at}: kind is declared twice"));
        }
        kinds.push(k.kind.as_str());
    }
    let mut states: Vec<&str> = Vec::with_capacity(seed.sales_tax_rate.len());
    for (i, r) in seed.sales_tax_rate.iter().enumerate() {
        let at = format!("sales tax rate #{} ({})", i + 1, r.state);
        if r.state.len() != 2 || !r.state.bytes().all(|b| b.is_ascii_uppercase()) {
            return Err(format!(
                "{at}: state must be a two-letter upper-case code (`CA`, `TX`, ...)"
            ));
        }
        if !bare(&r.jurisdiction) {
            return Err(format!(
                "{at}: jurisdiction is empty or has surrounding whitespace (the filing jurisdiction, `US-CA`)"
            ));
        }
        if r.rate_bps < 0 || r.rate_bps > RATE_BPS_MAX {
            return Err(format!(
                "{at}: rate_bps {} is outside 0..={RATE_BPS_MAX} (basis points: 725 = 7.25%)",
                r.rate_bps
            ));
        }
        if states.contains(&r.state.as_str()) {
            return Err(format!("{at}: state is declared twice"));
        }
        states.push(r.state.as_str());
    }
    Ok(())
}

/// The contract's cross-check: every account a kind names must be a
/// code the tenant's chart declares. `chart` is `None` when the
/// directory has no readable `seeds/chart_of_accounts.toml`, in which
/// case any kind naming an account is refused — the chart is the
/// tenant's to declare, and a kind cannot drain an account the tenant
/// has not said it has.
pub fn validate_against_chart(
    seed: &TaxSeed,
    chart: Option<&BTreeSet<String>>,
) -> Result<(), String> {
    for (i, k) in seed.tax_kind.iter().enumerate() {
        for (field, code) in [
            ("liability_account", Some(k.liability_account.as_str())),
            ("expense_account", k.expense_account.as_deref()),
        ] {
            let Some(code) = code else { continue };
            let declared = chart.is_some_and(|c| c.contains(code));
            if !declared {
                let chart_says = match chart {
                    Some(_) => "seeds/chart_of_accounts.toml does not declare".to_string(),
                    None => "no seeds/chart_of_accounts.toml declares".to_string(),
                };
                return Err(format!(
                    "tax kind #{} ({}): {field} names account `{code}`, which {chart_says} — the chart is the tenant's; declare the account there first",
                    i + 1,
                    k.kind
                ));
            }
        }
    }
    Ok(())
}

/// `seeds/tax.toml`: `[[tax_kind]]` + `[[sales_tax_rate]]` rows,
/// validated. An empty file (comments only) parses to an empty seed.
pub fn parse_tax_toml(text: &str) -> Result<TaxSeed, String> {
    let seed: TaxSeed = toml::from_str(text).map_err(|e| e.to_string())?;
    validate(&seed)?;
    Ok(seed)
}

pub fn load_tax_toml(path: &std::path::Path) -> Result<TaxSeed, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    parse_tax_toml(&text)
}

#[cfg(feature = "postgres")]
pub(crate) use pg::{check_liability_account_in_tx, names_a_tax_liability};
#[cfg(feature = "postgres")]
pub use pg::{declare_tax, list_sales_tax_rates, list_tax_kinds};

#[cfg(feature = "postgres")]
mod pg {
    use super::*;
    use crate::error::LedgerError;
    use boss_core::publisher::EventStamp;
    use serde_json::Value;
    use sqlx::PgPool;

    fn storage(e: sqlx::Error) -> LedgerError {
        LedgerError::Storage(e.to_string())
    }

    fn declared_event(
        stamp: &EventStamp,
        kind: &str,
        row: &impl Serialize,
    ) -> Result<boss_core::event::Event, LedgerError> {
        let mut payload =
            serde_json::to_value(row).map_err(|e| LedgerError::Storage(e.to_string()))?;
        if let Value::Object(map) = &mut payload {
            map.insert(
                "declared_by".to_string(),
                Value::String(stamp.actor().to_string()),
            );
        }
        Ok(stamp.event(kind, payload))
    }

    /// Land a tenant's tax regime: insert-if-absent by kind and by
    /// state, one transaction, one fact per INSERTED row on the outbox
    /// in the same transaction. A row the table already holds is KEPT
    /// and the outcome names the declared fields that differ. An
    /// account a kind names that `gl_accounts` does not hold is a
    /// caller error naming the kind and the code, refused before any
    /// row is written — the FK would refuse it too, as a storage
    /// failure that names a constraint.
    pub async fn declare_tax(
        pool: &PgPool,
        seed: &TaxSeed,
        stamp: &EventStamp,
    ) -> Result<TaxBatchOutcome, LedgerError> {
        validate(seed).map_err(LedgerError::InvalidTaxSeed)?;
        let mut tx = pool.begin().await.map_err(storage)?;
        for k in &seed.tax_kind {
            for code in std::iter::once(&k.liability_account).chain(k.expense_account.iter()) {
                let held: Option<(String,)> =
                    sqlx::query_as("SELECT code FROM gl_accounts WHERE code = $1")
                        .bind(code)
                        .fetch_optional(&mut *tx)
                        .await
                        .map_err(storage)?;
                if held.is_none() {
                    return Err(LedgerError::InvalidTaxSeed(format!(
                        "tax kind `{}` names account `{code}`, which the chart does not hold — declare it in seeds/chart_of_accounts.toml first",
                        k.kind
                    )));
                }
            }
        }
        let mut inserted = 0usize;
        let mut kept = Vec::new();
        for k in &seed.tax_kind {
            let landed = sqlx::query(
                "INSERT INTO tax_kinds (kind, liability_account, expense_account, derive_basis) \
                 VALUES ($1, $2, $3, $4) ON CONFLICT (kind) DO NOTHING",
            )
            .bind(&k.kind)
            .bind(&k.liability_account)
            .bind(&k.expense_account)
            .bind(&k.derive_basis)
            .execute(&mut *tx)
            .await
            .map_err(storage)?
            .rows_affected();
            if landed == 1 {
                boss_events::outbox::record_event_in_tx(
                    &mut tx,
                    &declared_event(stamp, TAX_KIND_DECLARED, k)?,
                )
                .await
                .map_err(LedgerError::Storage)?;
                inserted += 1;
                continue;
            }
            let (liability, expense, basis): (String, Option<String>, Option<String>) =
                sqlx::query_as(
                    "SELECT liability_account, expense_account, derive_basis FROM tax_kinds WHERE kind = $1",
                )
                .bind(&k.kind)
                .fetch_one(&mut *tx)
                .await
                .map_err(storage)?;
            let mut differs = Vec::new();
            if liability != k.liability_account {
                differs.push("liability_account".to_string());
            }
            if expense != k.expense_account {
                differs.push("expense_account".to_string());
            }
            if basis != k.derive_basis {
                differs.push("derive_basis".to_string());
            }
            kept.push(KeptRow {
                id: format!("tax_kind {}", k.kind),
                differs,
            });
        }
        for r in &seed.sales_tax_rate {
            let landed = sqlx::query(
                "INSERT INTO sales_tax_rate_by_state (state, jurisdiction, rate_bps) \
                 VALUES ($1, $2, $3) ON CONFLICT (state) DO NOTHING",
            )
            .bind(&r.state)
            .bind(&r.jurisdiction)
            .bind(r.rate_bps)
            .execute(&mut *tx)
            .await
            .map_err(storage)?
            .rows_affected();
            if landed == 1 {
                boss_events::outbox::record_event_in_tx(
                    &mut tx,
                    &declared_event(stamp, SALES_TAX_RATE_DECLARED, r)?,
                )
                .await
                .map_err(LedgerError::Storage)?;
                inserted += 1;
                continue;
            }
            let (jurisdiction, rate_bps): (String, i32) = sqlx::query_as(
                "SELECT jurisdiction, rate_bps FROM sales_tax_rate_by_state WHERE state = $1",
            )
            .bind(&r.state)
            .fetch_one(&mut *tx)
            .await
            .map_err(storage)?;
            let mut differs = Vec::new();
            if jurisdiction != r.jurisdiction {
                differs.push("jurisdiction".to_string());
            }
            if rate_bps != r.rate_bps {
                differs.push("rate_bps".to_string());
            }
            kept.push(KeptRow {
                id: format!("sales_tax_rate {}", r.state),
                differs,
            });
        }
        tx.commit().await.map_err(storage)?;
        Ok(TaxBatchOutcome {
            received: seed.tax_kind.len() + seed.sales_tax_rate.len(),
            inserted,
            kept,
        })
    }

    /// Every registered tax kind, by kind — what a reader of the
    /// registry sees, in the declaration's own shape.
    pub async fn list_tax_kinds(pool: &PgPool) -> Result<Vec<TaxKindInput>, LedgerError> {
        let rows: Vec<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT kind, liability_account, expense_account, derive_basis FROM tax_kinds ORDER BY kind",
        )
        .fetch_all(pool)
        .await
        .map_err(storage)?;
        Ok(rows
            .into_iter()
            .map(
                |(kind, liability_account, expense_account, derive_basis)| TaxKindInput {
                    kind,
                    liability_account,
                    expense_account,
                    derive_basis,
                },
            )
            .collect())
    }

    /// Every registered sales-tax rate, by state.
    pub async fn list_sales_tax_rates(
        pool: &PgPool,
    ) -> Result<Vec<SalesTaxRateInput>, LedgerError> {
        let rows: Vec<(String, String, i32)> = sqlx::query_as(
            "SELECT state, jurisdiction, rate_bps FROM sales_tax_rate_by_state ORDER BY state",
        )
        .fetch_all(pool)
        .await
        .map_err(storage)?;
        Ok(rows
            .into_iter()
            .map(|(state, jurisdiction, rate_bps)| SalesTaxRateInput {
                state,
                jurisdiction,
                rate_bps,
            })
            .collect())
    }

    /// The fact kinds whose payload names a tax liability account —
    /// the two the posting path holds to this registry.
    pub(crate) fn names_a_tax_liability(fact_kind: &str) -> bool {
        matches!(fact_kind, "finance.tax.accrued" | "finance.tax.remitted")
    }

    /// Hold a tax fact's `liability_account` to the row this instance
    /// holds — the tenant's declaration is the ONE definition of which
    /// account a kind hits (backlog e021be29). With a `kind` in the
    /// payload, the account must be the one that kind's row names, and a
    /// kind with no row cannot post; without one (the standalone accrual
    /// door, `POST /api/ledger/tax-accruals`, stamps none), the account
    /// must be one SOME row names as its liability. Either refusal is
    /// `TaxKindNotRegistered`, naming the kind and the accounts, so the
    /// remedy reads off the error: declare the kind in `seeds/tax.toml`
    /// and publish it, or fix the emitter's stamp.
    ///
    /// Runs inside the posting transaction, after the rule's own payload
    /// checks and only for an entry about to be written — an already-
    /// posted fact stays the idempotent no-op it is, whatever the row
    /// says today.
    pub(crate) async fn check_liability_account_in_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        fact_kind: &str,
        tax_kind: Option<&str>,
        account: &str,
    ) -> Result<(), LedgerError> {
        let refused = |reason: String| LedgerError::TaxKindNotRegistered {
            fact_kind: fact_kind.to_string(),
            reason,
        };
        match tax_kind {
            Some(kind) => {
                let named: Option<(String,)> =
                    sqlx::query_as("SELECT liability_account FROM tax_kinds WHERE kind = $1")
                        .bind(kind)
                        .fetch_optional(&mut **tx)
                        .await
                        .map_err(storage)?;
                match named {
                    None => Err(refused(format!(
                        "tax kind `{kind}` has no tax_kinds row on this instance — \
                         declare it in seeds/tax.toml and publish it before it can post"
                    ))),
                    Some((row,)) if row != account => Err(refused(format!(
                        "liability_account `{account}` is not the account tax_kinds names \
                         for `{kind}` (`{row}`)"
                    ))),
                    Some(_) => Ok(()),
                }
            }
            None => {
                let some_kind: Option<(String,)> =
                    sqlx::query_as("SELECT kind FROM tax_kinds WHERE liability_account = $1")
                        .bind(account)
                        .fetch_optional(&mut **tx)
                        .await
                        .map_err(storage)?;
                match some_kind {
                    None => Err(refused(format!(
                        "no tax_kinds row on this instance names `{account}` as its \
                         liability account, and the fact names no kind"
                    ))),
                    Some(_) => Ok(()),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(k: &str, liability: &str, expense: Option<&str>) -> TaxKindInput {
        TaxKindInput {
            kind: k.into(),
            liability_account: liability.into(),
            expense_account: expense.map(str::to_string),
            derive_basis: None,
        }
    }

    fn rate(state: &str, bps: i32) -> SalesTaxRateInput {
        SalesTaxRateInput {
            state: state.into(),
            jurisdiction: format!("US-{state}"),
            rate_bps: bps,
        }
    }

    /// The migration's regime, in the file's shape.
    const FILE: &str = r#"
[[tax_kind]]
kind = "sales"
liability_account = "2300"
derive_basis = "period-sales-tax"

[[tax_kind]]
kind = "income"
liability_account = "2310"
expense_account = "6500"
derive_basis = "prior-quarter-net-income"

[[sales_tax_rate]]
state = "CA"
jurisdiction = "US-CA"
rate_bps = 725

[[sales_tax_rate]]
state = "OR"
jurisdiction = "US-OR"
rate_bps = 0
"#;

    #[test]
    fn the_file_shape_parses_and_names_its_accounts() {
        let seed = parse_tax_toml(FILE).unwrap();
        assert_eq!(seed.tax_kind.len(), 2);
        assert_eq!(seed.sales_tax_rate.len(), 2);
        assert_eq!(seed.tax_kind[1].expense_account.as_deref(), Some("6500"));
        assert_eq!(seed.sales_tax_rate[1].rate_bps, 0);
        let codes: Vec<String> = seed.account_codes().into_iter().collect();
        assert_eq!(codes, ["2300", "2310", "6500"]);
        // Serialized, an absent expense account is absent, not null.
        let v = serde_json::to_value(&seed.tax_kind[0]).unwrap();
        assert!(v.get("expense_account").is_none());
    }

    #[test]
    fn an_empty_file_is_a_valid_empty_seed() {
        // A tenant with nothing sellable yet (design 18cf4272).
        let seed = parse_tax_toml("# filled after the first sellable SKU\n").unwrap();
        assert!(seed.is_empty());
        assert_eq!(validate_against_chart(&seed, None), Ok(()));
    }

    #[test]
    fn a_field_the_tables_cannot_hold_and_a_stray_table_are_refused_by_name() {
        let err = parse_tax_toml(
            "[[tax_kind]]\nkind = \"sales\"\nliability_account = \"2300\"\nrate = 7\n",
        )
        .unwrap_err();
        assert!(
            err.contains("unknown field") && err.contains("rate"),
            "{err}"
        );
        let err = parse_tax_toml("[[tax_kinds]]\nkind = \"sales\"\n").unwrap_err();
        assert!(
            err.contains("tax_kinds") && err.contains("tax_kind") && err.contains("sales_tax_rate"),
            "a stray table names the two this reads: {err}"
        );
    }

    #[test]
    fn validate_refuses_each_constraint_by_row() {
        let ok = TaxSeed {
            tax_kind: vec![kind("sales", "2300", None)],
            sales_tax_rate: vec![rate("CA", 725)],
        };
        assert_eq!(validate(&ok), Ok(()));

        let err = validate(&TaxSeed {
            tax_kind: vec![kind(" sales", "2300", None)],
            ..Default::default()
        })
        .unwrap_err();
        assert!(
            err.contains("tax kind #1") && err.contains("whitespace"),
            "{err}"
        );
        let err = validate(&TaxSeed {
            tax_kind: vec![kind("sales", "", None)],
            ..Default::default()
        })
        .unwrap_err();
        assert!(
            err.contains("tax kind #1 (sales)") && err.contains("liability_account"),
            "{err}"
        );
        let err = validate(&TaxSeed {
            tax_kind: vec![kind("income", "2310", Some(""))],
            ..Default::default()
        })
        .unwrap_err();
        assert!(
            err.contains("expense_account") && err.contains("omit"),
            "{err}"
        );
        let err = validate(&TaxSeed {
            tax_kind: vec![kind("sales", "2300", None), kind("sales", "2301", None)],
            ..Default::default()
        })
        .unwrap_err();
        assert!(
            err.contains("tax kind #2 (sales)") && err.contains("twice"),
            "{err}"
        );

        let err = validate(&TaxSeed {
            sales_tax_rate: vec![rate("ca", 725)],
            ..Default::default()
        })
        .unwrap_err();
        assert!(
            err.contains("sales tax rate #1 (ca)") && err.contains("two-letter"),
            "{err}"
        );
        let err = validate(&TaxSeed {
            sales_tax_rate: vec![rate("CA", 2001)],
            ..Default::default()
        })
        .unwrap_err();
        assert!(err.contains("2001") && err.contains("0..=2000"), "{err}");
        let err = validate(&TaxSeed {
            sales_tax_rate: vec![rate("CA", -1)],
            ..Default::default()
        })
        .unwrap_err();
        assert!(err.contains("-1"), "{err}");
        let err = validate(&TaxSeed {
            sales_tax_rate: vec![SalesTaxRateInput {
                state: "CA".into(),
                jurisdiction: "".into(),
                rate_bps: 725,
            }],
            ..Default::default()
        })
        .unwrap_err();
        assert!(err.contains("jurisdiction"), "{err}");
        let err = validate(&TaxSeed {
            sales_tax_rate: vec![rate("CA", 725), rate("CA", 700)],
            ..Default::default()
        })
        .unwrap_err();
        assert!(
            err.contains("sales tax rate #2 (CA)") && err.contains("twice"),
            "{err}"
        );
    }

    #[test]
    fn a_kind_naming_an_account_the_chart_does_not_declare_is_refused_naming_both() {
        let seed = parse_tax_toml(FILE).unwrap();
        let chart: BTreeSet<String> = ["2300", "2310", "6500"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(validate_against_chart(&seed, Some(&chart)), Ok(()));

        let smaller: BTreeSet<String> = ["2300", "2310"].iter().map(|s| s.to_string()).collect();
        let err = validate_against_chart(&seed, Some(&smaller)).unwrap_err();
        assert!(
            err.contains("tax kind #2 (income)")
                && err.contains("expense_account")
                && err.contains("`6500`")
                && err.contains("seeds/chart_of_accounts.toml does not declare"),
            "{err}"
        );
        let err = validate_against_chart(&seed, None).unwrap_err();
        assert!(
            err.contains("tax kind #1 (sales)")
                && err.contains("liability_account")
                && err.contains("`2300`")
                && err.contains("no seeds/chart_of_accounts.toml"),
            "{err}"
        );
        // Rates name no account: a rates-only file needs no chart.
        let rates_only = TaxSeed {
            sales_tax_rate: vec![rate("CA", 725)],
            ..Default::default()
        };
        assert_eq!(validate_against_chart(&rates_only, None), Ok(()));
    }
}
