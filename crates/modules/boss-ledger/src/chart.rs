//! The chart of accounts as TENANT DATA (backlog 41af5195; design
//! 18cf4272, David 2026-09-17).
//!
//! WHY. `gl_accounts` was seeded by 40-ledger.sql with the demo
//! tenant's chart (1000 Cash … 2200 Deferred Revenue and its
//! industry's tax accounts) and `GET /api/ledger/accounts` was the
//! only door: any other tenant had no way to say what its books are
//! made of short of a migration to the product. The chart is the tenant's — the starter
//! chart in 40-ledger.sql stays as the OSS default the demo tenant
//! runs on — so the tenant declares it in `seeds/chart_of_accounts.toml`
//! and `boss tenant publish` sends it through `POST
//! /api/ledger/accounts/batch`, insert-if-absent by code, the shape the
//! classes / locations / agents doors landed on (#418).
//!
//! A CODE THAT COLLIDES WITH THE STARTER CHART IS THE SAME ACCOUNT. The
//! code is the identity every posting rule and journal line points at
//! (`1000` is debited by name), so a tenant row `1000 Bank` and the
//! starter row `1000 Cash` cannot be two accounts. Insert-if-absent
//! keeps the registered row under the starter's name and the outcome
//! NAMES the difference (`kept: [{code: "1000", differs: ["name"]}]`);
//! the tenant then either adopts the code as it stands or chooses
//! another. A rename in place would silently re-label every entry
//! already posted — which is why there is no upsert here.
//!
//! ONE LOADER, TWO DOORS. `boss tenant check` judges the file with
//! [`AccountInput`] + [`validate`] — this module, not a second parser
//! — so a row the door would refuse is INVALID at check time, in the
//! door's own words.

use serde::{Deserialize, Serialize};

/// The fact one INSERTED row leaves (the #418 shape): the row as
/// inserted plus `declared_by`, the actor the request signed with.
/// Never per kept row — a kept row changed no state and is already
/// named in the batch's answer — and never per batch. Declared in
/// migration 20260917170551.
pub const ACCOUNT_DECLARED: &str = "ledger.account.declared";

/// `gl_accounts.kind`'s CHECK constraint, in the table's order.
pub const ACCOUNT_KINDS: [&str; 5] = ["asset", "liability", "equity", "revenue", "expense"];

/// `gl_accounts.normal_side`'s CHECK constraint.
pub const NORMAL_BALANCES: [&str; 2] = ["debit", "credit"];

/// One row in a `POST /api/ledger/accounts/batch` body and one
/// `[[account]]` row in `seeds/chart_of_accounts.toml`. The table's
/// authorable columns and nothing else — `is_active` / `created_at` /
/// `retired_at` are the table's own — so an unknown field is refused
/// by name rather than dropped in silence (the agents door's lesson,
/// f56155f0).
///
/// `normal_balance` is the design's spelling (18cf4272); the column
/// and `GET /api/ledger/accounts` say `normal_side`, and a row written
/// from what the GET returned must not be refused for it, so the
/// column's spelling is accepted as an alias.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountInput {
    pub code: String,
    pub name: String,
    pub kind: String,
    #[serde(alias = "normal_side")]
    pub normal_balance: String,
    /// The parent account's CODE (the table holds `parent_id`; the
    /// door resolves it). Must be declared earlier in the same file or
    /// batch — the chart is self-contained.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

/// What the registry already holds for a code, in the declaration's
/// terms, for naming how a declaration differs from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredAccount {
    pub code: String,
    pub name: String,
    pub kind: String,
    pub normal_balance: String,
    pub parent: Option<String>,
}

impl RegisteredAccount {
    /// The declared fields this registered row disagrees on, by the
    /// input's field names, empty when identical.
    pub fn differs_from(&self, declared: &AccountInput) -> Vec<String> {
        let mut out = Vec::new();
        if self.name != declared.name {
            out.push("name".to_string());
        }
        if self.kind != declared.kind {
            out.push("kind".to_string());
        }
        if self.normal_balance != declared.normal_balance {
            out.push("normal_balance".to_string());
        }
        if self.parent != declared.parent {
            out.push("parent".to_string());
        }
        out
    }
}

/// A declared row the registry already held: kept as registered, with
/// the fields the declaration disagrees on named (empty = identical).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptAccount {
    pub code: String,
    pub differs: Vec<String>,
}

/// What a batch did: rows received, rows inserted, and the rows kept
/// as the registry already had them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChartBatchOutcome {
    pub received: usize,
    pub inserted: usize,
    pub kept: Vec<KeptAccount>,
}

impl ChartBatchOutcome {
    /// One line for a publish report: counts, then each kept row that
    /// differs, by field — so a tenant whose `1000 Bank` met the
    /// starter's `1000 Cash` reads it in the plan line, never in
    /// silence. The agents door's line shape.
    pub fn summary(&self) -> String {
        let mut s = format!("received {}, inserted {}", self.received, self.inserted);
        let differing: Vec<String> = self
            .kept
            .iter()
            .filter(|k| !k.differs.is_empty())
            .map(|k| format!("{} ({} differs)", k.code, k.differs.join(", ")))
            .collect();
        let same = self.kept.len() - differing.len();
        if same > 0 {
            s.push_str(&format!(", {same} already registered as declared"));
        }
        if !differing.is_empty() {
            s.push_str(&format!(
                "; kept as registered, not as declared — adopt the code or choose another: {}",
                differing.join("; ")
            ));
        }
        s
    }
}

/// The door's own validation, run BEFORE any row is written and by
/// `boss tenant check` on the file: every code and name non-empty,
/// `kind` and `normal_balance` inside the table's CHECK constraints
/// (named here so the refusal reads as the enum, not as a Postgres
/// constraint name), codes unique within the batch, and a parent
/// declared EARLIER in the same batch — the chart is self-contained,
/// and the door inserts in file order, so "earlier" is what lets the
/// parent's id resolve when its child lands.
pub fn validate(rows: &[AccountInput]) -> Result<(), String> {
    let mut seen: Vec<&str> = Vec::with_capacity(rows.len());
    for (i, r) in rows.iter().enumerate() {
        let at = format!("account #{} ({})", i + 1, r.code);
        if r.code.trim().is_empty() {
            return Err(format!("account #{}: code is empty", i + 1));
        }
        if r.code.trim() != r.code {
            return Err(format!("{at}: code has surrounding whitespace"));
        }
        if r.name.trim().is_empty() {
            return Err(format!("{at}: name is empty"));
        }
        if !ACCOUNT_KINDS.contains(&r.kind.as_str()) {
            return Err(format!(
                "{at}: kind `{}` is not one of {}",
                r.kind,
                ACCOUNT_KINDS.join("|")
            ));
        }
        if !NORMAL_BALANCES.contains(&r.normal_balance.as_str()) {
            return Err(format!(
                "{at}: normal_balance `{}` is not one of {}",
                r.normal_balance,
                NORMAL_BALANCES.join("|")
            ));
        }
        if seen.contains(&r.code.as_str()) {
            return Err(format!("{at}: code is declared twice"));
        }
        if let Some(p) = r.parent.as_deref() {
            if p == r.code {
                return Err(format!("{at}: parent is itself"));
            }
            if !seen.contains(&p) {
                return Err(format!(
                    "{at}: parent `{p}` is not declared earlier in the chart — \
                     declare a parent before its children"
                ));
            }
        }
        seen.push(r.code.as_str());
    }
    Ok(())
}

/// `seeds/chart_of_accounts.toml`: `[[account]]` rows, validated.
pub fn parse_chart_toml(text: &str) -> Result<Vec<AccountInput>, String> {
    #[derive(Deserialize)]
    struct Bundle {
        #[serde(default)]
        account: Vec<AccountInput>,
    }
    let b: Bundle = toml::from_str(text).map_err(|e| e.to_string())?;
    validate(&b.account)?;
    Ok(b.account)
}

pub fn load_chart_toml(path: &std::path::Path) -> Result<Vec<AccountInput>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    parse_chart_toml(&text)
}

#[cfg(feature = "postgres")]
pub use pg::declare_accounts;

#[cfg(feature = "postgres")]
mod pg {
    use super::*;
    use crate::error::LedgerError;
    use boss_core::publisher::EventStamp;
    use sqlx::PgPool;
    use uuid::Uuid;

    /// Build the `ledger.account.declared` event for one inserted row:
    /// the row as inserted (its minted id included) plus `declared_by`,
    /// read from the stamp so it is the same value `_actor` carries.
    fn declared_event(
        stamp: &EventStamp,
        id: Uuid,
        row: &AccountInput,
    ) -> Result<boss_core::event::Event, LedgerError> {
        let mut payload =
            serde_json::to_value(row).map_err(|e| LedgerError::Storage(e.to_string()))?;
        if let serde_json::Value::Object(map) = &mut payload {
            map.insert("id".to_string(), serde_json::Value::String(id.to_string()));
            map.insert(
                "declared_by".to_string(),
                serde_json::Value::String(stamp.actor().to_string()),
            );
        }
        Ok(stamp.event(ACCOUNT_DECLARED, payload))
    }

    /// Land a chart: insert-if-absent by code, one transaction, in
    /// file order (a parent's id resolves for the child that follows
    /// it). A code the table already holds is KEPT — never renamed —
    /// and the outcome names which declared fields differ. Each row
    /// INSERTED stages one [`ACCOUNT_DECLARED`] on the transactional
    /// outbox in the same transaction (#418's shape), so the row and
    /// its fact commit or roll back together.
    ///
    /// `parent_id` is resolved by code against the table inside the
    /// transaction; [`validate`] has already required the parent to
    /// be declared earlier, so a miss here is a storage inconsistency,
    /// not a caller error.
    pub async fn declare_accounts(
        pool: &PgPool,
        rows: &[AccountInput],
        stamp: &EventStamp,
    ) -> Result<ChartBatchOutcome, LedgerError> {
        validate(rows).map_err(LedgerError::InvalidChart)?;
        let mut tx = pool
            .begin()
            .await
            .map_err(|e| LedgerError::Storage(e.to_string()))?;
        let mut inserted = 0usize;
        let mut kept = Vec::new();
        for r in rows {
            // The registered row for this code, joined to its parent's
            // code so `differs_from` compares in the declaration's terms.
            let existing: Option<(String, String, String, Option<String>)> = sqlx::query_as(
                "SELECT a.name, a.kind, a.normal_side, p.code \
                 FROM gl_accounts a LEFT JOIN gl_accounts p ON p.id = a.parent_id \
                 WHERE a.code = $1",
            )
            .bind(&r.code)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| LedgerError::Storage(e.to_string()))?;
            if let Some((name, kind, normal_balance, parent)) = existing {
                let have = RegisteredAccount {
                    code: r.code.clone(),
                    name,
                    kind,
                    normal_balance,
                    parent,
                };
                kept.push(KeptAccount {
                    code: r.code.clone(),
                    differs: have.differs_from(r),
                });
                continue;
            }
            let parent_id: Option<Uuid> = match r.parent.as_deref() {
                None => None,
                Some(p) => {
                    let found: Option<(Uuid,)> =
                        sqlx::query_as("SELECT id FROM gl_accounts WHERE code = $1")
                            .bind(p)
                            .fetch_optional(&mut *tx)
                            .await
                            .map_err(|e| LedgerError::Storage(e.to_string()))?;
                    Some(
                        found
                            .ok_or_else(|| {
                                LedgerError::Storage(format!(
                                    "parent `{p}` of `{}` was declared earlier but is not in gl_accounts",
                                    r.code
                                ))
                            })?
                            .0,
                    )
                }
            };
            let id = Uuid::new_v4();
            let result = sqlx::query(
                "INSERT INTO gl_accounts (id, code, name, kind, normal_side, parent_id) \
                 VALUES ($1, $2, $3, $4, $5, $6) \
                 ON CONFLICT (code) DO NOTHING",
            )
            .bind(id)
            .bind(&r.code)
            .bind(&r.name)
            .bind(&r.kind)
            .bind(&r.normal_balance)
            .bind(parent_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| LedgerError::Storage(e.to_string()))?;
            if result.rows_affected() == 1 {
                boss_events::outbox::record_event_in_tx(&mut tx, &declared_event(stamp, id, r)?)
                    .await
                    .map_err(LedgerError::Storage)?;
                inserted += 1;
            }
        }
        tx.commit()
            .await
            .map_err(|e| LedgerError::Storage(e.to_string()))?;
        Ok(ChartBatchOutcome {
            received: rows.len(),
            inserted,
            kept,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(code: &str, name: &str, kind: &str, nb: &str, parent: Option<&str>) -> AccountInput {
        AccountInput {
            code: code.into(),
            name: name.into(),
            kind: kind.into(),
            normal_balance: nb.into(),
            parent: parent.map(str::to_string),
        }
    }

    /// The real tenant's chart (design 18cf4272), in the file's shape.
    const FILE: &str = r#"
[[account]]
code = "1000"
name = "Bank"
kind = "asset"
normal_balance = "debit"

[[account]]
code = "1010"
name = "Stripe balance"
kind = "asset"
normal_balance = "debit"
parent = "1000"

[[account]]
code = "4100"
name = "Sponsorship revenue"
kind = "revenue"
normal_balance = "credit"
"#;

    #[test]
    fn the_tenant_shape_parses_and_validates() {
        let rows = parse_chart_toml(FILE).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].parent.as_deref(), Some("1000"));
        assert_eq!(rows[2].kind, "revenue");
    }

    #[test]
    fn the_columns_spelling_is_accepted_as_an_alias() {
        // A row written from what GET /api/ledger/accounts returned.
        let rows = parse_chart_toml(
            "[[account]]\ncode = \"1000\"\nname = \"Bank\"\nkind = \"asset\"\nnormal_side = \"debit\"\n",
        )
        .unwrap();
        assert_eq!(rows[0].normal_balance, "debit");
        // Serialized, it is the design's spelling, and parent is
        // absent rather than null.
        let v = serde_json::to_value(&rows[0]).unwrap();
        assert_eq!(v["normal_balance"], "debit");
        assert!(v.get("parent").is_none());
    }

    #[test]
    fn a_field_the_table_cannot_hold_is_refused_by_name() {
        let err = parse_chart_toml(
            "[[account]]\ncode = \"1000\"\nname = \"Bank\"\nkind = \"asset\"\nnormal_balance = \"debit\"\ncurrency = \"USD\"\n",
        )
        .unwrap_err();
        assert!(
            err.contains("unknown field") && err.contains("currency"),
            "{err}"
        );
    }

    #[test]
    fn validate_refuses_each_constraint_by_row() {
        let ok = row("1000", "Bank", "asset", "debit", None);
        assert_eq!(validate(std::slice::from_ref(&ok)), Ok(()));

        let err = validate(&[row("1000", "Bank", "cash", "debit", None)]).unwrap_err();
        assert!(
            err.contains("account #1 (1000)")
                && err.contains("asset|liability|equity|revenue|expense"),
            "{err}"
        );
        let err = validate(&[row("1000", "Bank", "asset", "dr", None)]).unwrap_err();
        assert!(err.contains("debit|credit"), "{err}");
        let err = validate(&[ok.clone(), row("1000", "Cash", "asset", "debit", None)]).unwrap_err();
        assert!(
            err.contains("account #2 (1000)") && err.contains("twice"),
            "{err}"
        );
        let err = validate(&[row("", "Bank", "asset", "debit", None)]).unwrap_err();
        assert!(err.contains("code is empty"), "{err}");
        let err = validate(&[row("1000", "  ", "asset", "debit", None)]).unwrap_err();
        assert!(err.contains("name is empty"), "{err}");
        // A parent must come BEFORE its child: the door inserts in
        // order and resolves the parent's id as it goes.
        let err = validate(&[
            row("1010", "Stripe balance", "asset", "debit", Some("1000")),
            ok.clone(),
        ])
        .unwrap_err();
        assert!(
            err.contains("account #1 (1010)")
                && err.contains("parent `1000`")
                && err.contains("earlier"),
            "{err}"
        );
        let err = validate(&[row("1000", "Bank", "asset", "debit", Some("1000"))]).unwrap_err();
        assert!(err.contains("itself"), "{err}");
        assert_eq!(
            validate(&[
                ok,
                row("1010", "Stripe balance", "asset", "debit", Some("1000"))
            ]),
            Ok(())
        );
    }

    #[test]
    fn a_kept_row_names_what_differs_and_the_summary_reads_it() {
        let starter = RegisteredAccount {
            code: "1000".into(),
            name: "Cash".into(),
            kind: "asset".into(),
            normal_balance: "debit".into(),
            parent: None,
        };
        assert_eq!(
            starter.differs_from(&row("1000", "Bank", "asset", "debit", None)),
            ["name"]
        );
        assert!(
            starter
                .differs_from(&row("1000", "Cash", "asset", "debit", None))
                .is_empty()
        );
        assert_eq!(
            starter.differs_from(&row("1000", "Bank", "liability", "credit", Some("2000"))),
            ["name", "kind", "normal_balance", "parent"]
        );
        let out = ChartBatchOutcome {
            received: 3,
            inserted: 1,
            kept: vec![
                KeptAccount {
                    code: "1000".into(),
                    differs: vec!["name".into()],
                },
                KeptAccount {
                    code: "2100".into(),
                    differs: vec![],
                },
            ],
        };
        assert_eq!(
            out.summary(),
            "received 3, inserted 1, 1 already registered as declared; kept as registered, \
             not as declared — adopt the code or choose another: 1000 (name differs)"
        );
        let out = ChartBatchOutcome {
            received: 1,
            inserted: 1,
            kept: vec![],
        };
        assert_eq!(out.summary(), "received 1, inserted 1");
    }
}
