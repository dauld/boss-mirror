//! The product's rule directory carries the PLATFORM's reactors and
//! nothing a tenant would declare for itself.
//!
//! WHY (design e2580840 car 4, backlog 105fb702, 2026-09-17). A
//! read-only audit of the real instance found 31 of the 73 rules under
//! `infra/dispatcher/rules/` were the brewery's: eight brewery-domain
//! reactors (keg-return settle, tasting-panel spawn, excise accrual at
//! 350c/bbl, packaging allocate, production produce/consume, ingredient
//! restock), nine `webhook.notify` forwards to `BOSS_EVENT_WEBHOOK_URL`
//! — the parked simulator's callback, a port nothing listens on — and
//! fourteen company-module reactors (`step.done.billing` → issue the
//! invoice, `commerce.invoice.created` → drain finished goods and book
//! COGS, payroll, bills, hire/terminate) that fire on step kinds NO
//! platform workflow declares (`infra/platform/workflows/` names none
//! of the 22 kinds; only `examples/*/seeds/workflows.toml` do). Every
//! one was enforced on an instance whose tenant declares no such
//! protocol, and `products-consume-on-invoice-created` would have fired
//! on the tenant's first real invoice. Since #430 a tenant's reactors
//! ride its own `seeds/rules.toml` (docs/tenant-contract.md), so they
//! moved to `examples/brewery/seeds/rules.toml` and the tree's directory
//! kept only the reactors that run the platform itself.
//!
//! THE RATCHET. `dispatcher-rules-ratchet.sh` asks every rule to say
//! WHY it exists; this asks every rule WHOSE it is. A handler prefix on
//! the list below is a business-domain effect (inventory, products,
//! commerce, shipping, the ledger, the people registry, the external
//! webhook), and a rule that names one is a tenant's protocol data by
//! construction: the platform runs trains, gates, sweeps, credentials
//! and the estate, and none of those posts to a ledger or drains stock.
//! A future PLATFORM handler under one of these prefixes (a
//! tenant-agnostic `ledger.project_fact`, say) edits this list in a
//! reviewed diff — which is the whole point of a ratchet.

use std::path::Path;

use boss_dispatcher::rules::registry::parse_raw_path;
use boss_testing::dispatcher_rules_dir;

/// Handler prefixes a tenant's rule file owns. Measured from the 31
/// rules moved on 2026-09-17: `inventory.*` (11 uses), `products.*` (4),
/// `ledger.*` (6), `webhook.notify` (9), `commerce.*` (1),
/// `shipping.*` (1), `packaging.*` (1), `people.*` (2).
const TENANT_HANDLER_PREFIXES: &[&str] = &[
    "packaging.",
    "inventory.",
    "products.",
    "commerce.",
    "shipping.",
    "ledger.",
    "people.",
    "webhook.notify",
];

/// Words a brewery rule's trigger or predicate carries — a job kind, a
/// step kind or a subject the example tenant's `workflows.toml` declares
/// and no platform bundle does. Matched against `on_event`, `when` and
/// every `do` arg, case-insensitively.
const TENANT_WORDS: &[&str] = &[
    "keg",
    "brew",
    "tasting",
    "excise",
    "packaging",
    "production-",
    "ingredient",
    "taproom",
    "invoice",
    "payroll",
    "bill-",
    "hr-hire",
    "hr-terminate",
    "procurement",
    "receiving",
    "shipment",
    "repair",
    "billing",
];

fn offences(dir: &Path) -> Vec<String> {
    let raw = parse_raw_path(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    let mut out = Vec::new();
    for rule in &raw.rules {
        for step in &rule.do_steps {
            if let Some(p) = TENANT_HANDLER_PREFIXES
                .iter()
                .find(|p| step.handler.starts_with(*p))
            {
                out.push(format!(
                    "{}: handler `{}` (a tenant's `{p}*` effect)",
                    rule.name, step.handler
                ));
            }
        }
        let mut text: Vec<String> = Vec::new();
        text.extend(rule.on_event.iter().cloned());
        text.extend(rule.when.iter().cloned());
        for step in &rule.do_steps {
            text.extend(step.args.values().cloned());
        }
        let text = text.join(" ").to_ascii_lowercase();
        for w in TENANT_WORDS {
            if text.contains(w) {
                out.push(format!(
                    "{}: trigger/predicate/args mention `{w}` (a tenant's kind)",
                    rule.name
                ));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// The property, against the real directory.
#[test]
fn no_product_rule_names_a_tenants_handler_or_kind() {
    let offences = offences(&dispatcher_rules_dir());
    assert!(
        offences.is_empty(),
        "{} rule(s) under infra/dispatcher/rules/ are a tenant's, not the platform's — \
         a reactor on a business step or a webhook forward belongs in that tenant's \
         seeds/rules.toml (docs/tenant-contract.md), not in the product:\n  {}",
        offences.len(),
        offences.join("\n  ")
    );
}

/// The check is not vacuous: a copy of the directory with one brewery
/// rule dropped back in is refused, naming the rule, the handler and the
/// word. The fixture is the shape the keg-deposit settlement had.
#[test]
fn a_brewery_rule_dropped_into_the_product_directory_is_named() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("rules");
    std::fs::create_dir_all(&dir).expect("fixture dir");
    for entry in std::fs::read_dir(dispatcher_rules_dir()).expect("read the authored registry") {
        let src = entry.expect("dir entry").path();
        if src.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        std::fs::copy(&src, dir.join(src.file_name().expect("file name"))).expect("copy");
    }
    std::fs::write(
        dir.join("a-fixture-keg-rule.toml"),
        r#"[[rule]]
name = "a-fixture-keg-rule"
why = """
A fixture: the shape a brewery reactor has, dropped into the product.
"""
on_event = "jobs.job.closed"
when = "kind = \"keg-return\" AND outcome = \"completed\""
[[rule.do]]
handler = "ledger.keg_deposit.settle"
"#,
    )
    .expect("write the fixture rule");

    let offences = offences(&dir);
    assert!(
        offences
            .iter()
            .any(|o| o.contains("a-fixture-keg-rule") && o.contains("ledger.keg_deposit.settle")),
        "the fixture's handler must be named: {offences:?}"
    );
    assert!(
        offences
            .iter()
            .any(|o| o.contains("a-fixture-keg-rule") && o.contains("`keg`")),
        "the fixture's kind must be named: {offences:?}"
    );
}
