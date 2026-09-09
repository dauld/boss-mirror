//! Layer-1 of the Job-completeness validator
//! (`docs/design/correctness-protocol.md`): every WorkflowSpec in the
//! brewery seed bundle must pass the static lint. New Workflows that bind
//! a side-effect-bearing StepType without filling required metadata fail
//! this test before publish — catching the bypass at authoring time
//! instead of waiting for a runtime "metadata X missing" log.
//!
//! Step-completion volumes, metadata shapes, and side-effect wiring are
//! validated end-to-end by the live 365-day regen, where the server
//! materializes steps and the workforce executor drives them through the
//! public API — not in-process here.

use std::path::PathBuf;

fn brewery_seeds_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("examples/brewery/seeds")
}

#[test]
fn brewery_workflows_pass_layer1_lint() {
    use boss_jobs::seed_loader::load_workflows_with_owning_team;
    use boss_jobs::step_registry::StepRegistry;
    use boss_jobs::workflow_lint::validate_all;

    let kinds_path = brewery_seeds_dir().join("workflows.toml");
    let kinds = load_workflows_with_owning_team(&kinds_path, "brewery")
        .expect("brewery workflows.toml parses");
    let registry = StepRegistry::v1();
    let errs = validate_all(&kinds, &registry);
    if !errs.is_empty() {
        let mut msg = String::from("brewery Workflows failed layer-1 lint:\n");
        for e in &errs {
            msg.push_str(&format!("  {}\n", e));
        }
        panic!("{msg}");
    }
}

/// A wholesale keg order must reference ONE consistent SKU set across its
/// whole lifecycle: confirm-order → stock-check → pull-and-stage →
/// deliver (shipment) → invoice (billing). If the shipment step's
/// `line_items` drift from the others, the brewery invoices customers for
/// kegs it never ships (revenue without fulfillment) and the unshipped
/// styles never draw down finished-goods stock — the demand-mix gap
/// behind the finished-goods glut. This guards the whole class of drift,
/// not just the instance we found (deliver omitting 3 of 5 SKUs).
#[test]
fn wholesale_order_references_one_consistent_sku_set() {
    use boss_jobs::seed_loader::load_workflows_with_owning_team;
    use std::collections::BTreeSet;

    let kinds_path = brewery_seeds_dir().join("workflows.toml");
    let kinds = load_workflows_with_owning_team(&kinds_path, "brewery")
        .expect("brewery workflows.toml parses");

    let order = kinds
        .iter()
        .find(|k| k.kind == "wholesale-keg-order")
        .expect("wholesale-keg-order Workflow present");

    // Each step that carries `line_items` → (step title, its SKU set).
    let per_step: Vec<(String, BTreeSet<String>)> = order
        .steps
        .iter()
        .filter_map(|s| {
            let skus: BTreeSet<String> = s
                .metadata_defaults
                .get("line_items")
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|it| it.get("sku").and_then(|x| x.as_str()).map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            (!skus.is_empty()).then_some((s.title.clone(), skus))
        })
        .collect();

    assert!(
        per_step.len() >= 2,
        "expected several line_items-bearing steps in wholesale-keg-order, found {}",
        per_step.len()
    );

    let (ref_title, ref_skus) = &per_step[0];
    for (title, skus) in &per_step[1..] {
        assert_eq!(
            skus, ref_skus,
            "wholesale-keg-order step `{title}` references SKUs {skus:?} but step \
             `{ref_title}` references {ref_skus:?} — every step of an order must carry \
             the same SKU set so the brewery ships exactly what it bills (and every \
             style draws down stock)"
        );
    }
}

/// The absorption contract is rules data on both sides: the
/// `inventory.overhead.absorb` dos capitalize `rate_cents_per_bbl ×
/// batch bbl` per driver at runtime, and the produce rule's
/// `overhead_accounts` arg names the same accounts so the
/// drain-actual-wip basis reconstructs exactly those facts. The seed
/// multiplies nothing — this test is the enforcement that replaced the
/// old hand-stamped `overhead_absorbed` amounts:
///   (a) the production-consume rule carries exactly the model's three
///       canonical drivers at the canonical rates (derivation in the
///       pale-ale mash-in comment, workflows.toml);
///   (b) capitalize-set == drain-set (the produce rule's
///       `overhead_accounts` names the same accounts);
///   (c) no Workflow stamps `overhead_absorbed` amounts anymore; and
///   (d) every producing kind still states the batch size the runtime
///       multiplication reads (`batch_bbl` on a step, else the summed
///       `excise_bbl` of its produce steps).
/// Change a rate → change it here + in the rule's own file under
/// infra/dispatcher/rules/ (plus a timestamped seed migration).
#[test]
fn overhead_absorption_rules_agree() {
    use boss_jobs::seed_loader::load_workflows_with_owning_team;

    const DRIVERS: [(&str, i64, &str); 3] = [
        ("6100", 3660, "Direct labor"),
        ("6300", 560, "Process utilities"),
        ("6900", 860, "Production depreciation"),
    ];

    // Rule args are expr strings: ints as digits, strings quoted.
    fn unquote(s: &str) -> &str {
        s.trim().trim_matches('"')
    }

    // --- the rule registry: both halves of the contract -------------
    // One file per rule, named for it (infra/dispatcher/rules/README.md).
    let rules_dir = brewery_seeds_dir()
        .join("..")
        .join("..")
        .join("..")
        .join("infra/dispatcher/rules");
    let read_rule = |name: &str| -> toml::Value {
        let path = rules_dir.join(format!("{name}.toml"));
        let doc: toml::Value = toml::from_str(
            &std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display())),
        )
        .unwrap_or_else(|e| panic!("{name}.toml parses: {e}"));
        let rules = doc
            .get("rule")
            .and_then(|v| v.as_array())
            .unwrap_or_else(|| panic!("{name}.toml has no [[rule]]"));
        assert_eq!(rules.len(), 1, "{name}.toml holds one rule");
        rules[0].clone()
    };

    // (a) capitalize-set: exactly the three canonical drivers.
    let consume_rule = &read_rule("parts-consume-on-production-consume-step-done");
    let absorb_dos: Vec<&toml::Value> = consume_rule
        .get("do")
        .and_then(|v| v.as_array())
        .expect("consume rule has do entries")
        .iter()
        .filter(|d| d.get("handler").and_then(|v| v.as_str()) == Some("inventory.overhead.absorb"))
        .collect();
    assert_eq!(
        absorb_dos.len(),
        DRIVERS.len(),
        "expected the {} model drivers as inventory.overhead.absorb dos, found {}",
        DRIVERS.len(),
        absorb_dos.len()
    );
    for (account, rate, driver) in DRIVERS {
        let d = absorb_dos
            .iter()
            .find(|d| {
                d.get("args")
                    .and_then(|a| a.get("credit_account"))
                    .and_then(|v| v.as_str())
                    .map(unquote)
                    == Some(account)
            })
            .unwrap_or_else(|| {
                panic!("no inventory.overhead.absorb do credits {account} ({driver})")
            });
        let args = d.get("args").expect("absorb do has args");
        let got_rate: i64 = args
            .get("rate_cents_per_bbl")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| panic!("CR {account}: rate_cents_per_bbl missing or not an int"));
        assert_eq!(
            got_rate, rate,
            "CR {account} ({driver}): rate {got_rate}¢/bbl != canonical {rate}¢/bbl"
        );
        let got_driver = args
            .get("driver")
            .and_then(|v| v.as_str())
            .map(unquote)
            .unwrap_or("");
        assert_eq!(
            got_driver, driver,
            "CR {account}: driver label `{got_driver}` != canonical `{driver}`"
        );
    }
    // Distinct accounts — two dos crediting one account would collide
    // on the endpoint's (step, account) idempotency key and silently
    // drop the second amount.
    let mut accounts: Vec<&str> = absorb_dos
        .iter()
        .filter_map(|d| {
            d.get("args")
                .and_then(|a| a.get("credit_account"))
                .and_then(|v| v.as_str())
                .map(unquote)
        })
        .collect();
    accounts.sort_unstable();
    assert!(
        accounts.windows(2).all(|w| w[0] != w[1]),
        "absorb dos credit a duplicate account — amounts would collide on idempotency"
    );

    // (b) drain-set == capitalize-set.
    let produce_rule = &read_rule("products-produce-on-production-produce-step-done");
    let produce_do = produce_rule
        .get("do")
        .and_then(|v| v.as_array())
        .expect("produce rule has do entries")
        .iter()
        .find(|d| d.get("handler").and_then(|v| v.as_str()) == Some("products.produce"))
        .expect("produce rule has a products.produce do");
    let mut drain_accounts: Vec<&str> = produce_do
        .get("args")
        .and_then(|a| a.get("overhead_accounts"))
        .and_then(|v| v.as_str())
        .map(unquote)
        .expect("products.produce do carries overhead_accounts")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    drain_accounts.sort_unstable();
    assert_eq!(
        drain_accounts, accounts,
        "drain-set (produce overhead_accounts) != capitalize-set (absorb dos) — \
         the drain would reconstruct facts the absorb side never posts (NAK loop) \
         or strand capitalized drivers in WIP"
    );

    // --- seed: no stamps; batch size derivable ---------------------
    let kinds_path = brewery_seeds_dir().join("workflows.toml");
    let kinds = load_workflows_with_owning_team(&kinds_path, "brewery")
        .expect("brewery workflows.toml parses");

    let mut producing_kinds = 0;
    for kind in &kinds {
        // (c) the stamp path is retired — amounts live in rules args.
        for step in &kind.steps {
            assert!(
                step.metadata_defaults.get("overhead_absorbed").is_none(),
                "Workflow `{}` step `{}` stamps overhead_absorbed — absorption amounts are \
                 rules data now (inventory.overhead.absorb args), not seed stamps",
                kind.kind,
                step.title
            );
        }

        // (d) the runtime multiplication's other input must exist:
        // batch_bbl on any step, else summed excise_bbl of the produce
        // steps — the same derivation inventory.overhead.absorb runs.
        let produces = kind.steps.iter().any(|s| s.kind == "production-produce");
        if produces && kind.steps.iter().any(|s| s.kind == "production-consume") {
            let batch_bbl: i64 = kind
                .steps
                .iter()
                .find_map(|s| {
                    s.metadata_defaults
                        .get("batch_bbl")
                        .and_then(|v| v.as_i64())
                })
                .unwrap_or_else(|| {
                    kind.steps
                        .iter()
                        .filter(|s| s.kind == "production-produce")
                        .filter_map(|s| {
                            s.metadata_defaults
                                .get("excise_bbl")
                                .and_then(|v| v.as_i64())
                        })
                        .sum()
                });
            assert!(
                batch_bbl > 0,
                "Workflow `{}` brews (production-consume → production-produce) but states no \
                 batch_bbl / excise_bbl — the absorb handler would no-op and the kind would \
                 cost out materials-only while sibling kinds carry the per-bbl burden",
                kind.kind
            );
            producing_kinds += 1;
        }
    }
    // The bundle ships 5 morning-brew styles + seasonal-release; a
    // collapse to 0 here means the walk silently matched nothing.
    assert!(
        producing_kinds >= 6,
        "expected ≥6 brewing Workflows with a derivable batch size, found {producing_kinds}"
    );
}

/// Q6 (subject-model design, resolved 2026-07-15): org-level work is
/// about the COMPANY, not the brewhouse location it happened to be
/// parked on. The org-level kinds — payroll, tax filings, AP runs,
/// facility overhead, the daily production heartbeat and its QC/PM
/// satellites — declare `subject_kinds = ["company"]`; `ad-hoc` keeps
/// its menu but gains company for org-level one-offs. The location
/// kind remains for genuinely place-bound work.
#[test]
fn org_level_kinds_are_about_the_company() {
    use boss_jobs::seed_loader::load_workflows_with_owning_team;

    let kinds_path = brewery_seeds_dir().join("workflows.toml");
    let kinds = load_workflows_with_owning_team(&kinds_path, "brewery").unwrap();

    let org_level = [
        "payroll-run",
        "payroll-941-filing",
        "sales-tax-filing",
        "income-tax-filing",
        "excise-tax-filing",
        "ap-payment-run",
        "facility-overhead",
        "equipment-preventive-maintenance",
        "morning-brew",
        "morning-brew-ipa",
        "morning-brew-stout",
        "morning-brew-lager",
        "morning-brew-hazy",
        "batch-qc-hold",
    ];
    for kind in org_level {
        let spec = kinds
            .iter()
            .find(|k| k.kind == kind)
            .unwrap_or_else(|| panic!("{kind} missing from workflows.toml"));
        assert_eq!(
            spec.subject_kinds,
            vec!["company".to_string()],
            "{kind} is org-level work — its subject is the company (Q6)"
        );
    }

    let ad_hoc = kinds.iter().find(|k| k.kind == "ad-hoc").unwrap();
    assert!(
        ad_hoc.subject_kinds.contains(&"company".to_string()),
        "ad-hoc must allow company subjects for org-level one-offs"
    );
}

/// The shape-driven engine picks a Job's subject_id from the rate's
/// `subject_distribution` (or `subject_cadence`) but its subject_KIND
/// from the Workflow's declared `subject_kinds[0]` — two different
/// files. A distribution still pinning the brewhouse location under a
/// company-subjected kind builds `(company, loc-brewery-brewhouse)`
/// pairs, which the existence gate rejected 18 times in Q6's first
/// live hours (morning brews starved). Every rate under a
/// company-subjected kind must target the company id.
#[test]
fn company_kind_rate_distributions_target_the_company() {
    use boss_jobs::seed_loader::load_workflows_with_owning_team;
    use std::collections::HashSet;

    let kinds_path = brewery_seeds_dir().join("workflows.toml");
    let kinds = load_workflows_with_owning_team(&kinds_path, "brewery").unwrap();
    let company_kinds: HashSet<&str> = kinds
        .iter()
        .filter(|k| k.subject_kinds == vec!["company".to_string()])
        .map(|k| k.kind.as_str())
        .collect();
    assert!(!company_kinds.is_empty());

    let tenant: toml::Value =
        toml::from_str(&std::fs::read_to_string(brewery_seeds_dir().join("tenant.toml")).unwrap())
            .unwrap();
    let Some(rates) = tenant.get("job_rates").and_then(|r| r.as_table()) else {
        panic!("tenant.toml has no job_rates");
    };
    for (kind, rate) in rates {
        if !company_kinds.contains(kind.as_str()) {
            continue;
        }
        if let Some(dist) = rate.get("subject_distribution").and_then(|d| d.as_table()) {
            for id in dist.keys() {
                assert_eq!(
                    id, "brewery",
                    "{kind}: subject_distribution pins `{id}` but the kind is company-subjected"
                );
            }
        }
        if let Some(cadence) = rate.get("subject_cadence").and_then(|c| c.as_array()) {
            for entry in cadence {
                let id = entry
                    .get("subject_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                assert_eq!(
                    id, "brewery",
                    "{kind}: subject_cadence pins `{id}` but the kind is company-subjected"
                );
            }
        }
    }
}

/// Q7: every Job names a responsible human owner, resolved from the
/// kind's `metadata.owner_role` or (fallback) its first role-bearing
/// step's `authority_role`. A kind with NEITHER can only produce
/// unownable Jobs — the create gate rejects them at runtime, so the
/// seed must fail here first, at test time.
#[test]
fn every_kind_can_resolve_a_human_owner() {
    use boss_jobs::seed_loader::load_workflows_with_owning_team;

    let kinds_path = brewery_seeds_dir().join("workflows.toml");
    let kinds = load_workflows_with_owning_team(&kinds_path, "brewery").unwrap();

    let mut unresolvable = Vec::new();
    for k in &kinds {
        let has_owner_role = k
            .metadata
            .get("owner_role")
            .and_then(|v| v.as_str())
            .is_some_and(|r| !r.is_empty());
        let has_role_bearing_step = k
            .steps
            .iter()
            .any(|s| s.authority_role.as_deref().is_some_and(|r| !r.is_empty()));
        if !has_owner_role && !has_role_bearing_step {
            unresolvable.push(k.kind.clone());
        }
    }
    assert!(
        unresolvable.is_empty(),
        "kinds with no owner_role and no role-bearing step — their Jobs \
         would be rejected at the Q7 gate: {unresolvable:?}"
    );
}
