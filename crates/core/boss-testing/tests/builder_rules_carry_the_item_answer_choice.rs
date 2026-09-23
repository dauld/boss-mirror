//! Rule 7 of the builder rules must carry the WHOLE item-answer choice
//! the gate enforces, not one arm of it.
//!
//! MEASURED (backlog 0906495a, 2026-09-22). `--park-design` appeared 25
//! times in `crates/orchestrators/boss-cli/src/gate.rs` and ZERO times
//! in `infra/platform/documents/builder-rules.md`, whose rule 7 hands
//! every dispatched builder a gate command carrying
//! `--park-backlog-item` and nothing else. A builder dispatched onto a
//! design's plan typed what the template showed. The one other place
//! the word could be SEEN was the shared refusal at `boss car open`,
//! and 19a2aca3 correctly stopped that door offering a flag it does not
//! have — which left no path to the flag at all.
//!
//! CLAUDE.md §9a: the set is not retyped here. `require_item_answer`'s
//! `given` list is the definition — the exclusive set the gate refuses
//! to launch without, shared with `boss car open` — and this test reads
//! the flags out of it with `syn`, so the next answer added to the gate
//! and not to rule 7 fails HERE, by name, rather than as a car linked
//! to the wrong packet.

use boss_testing::repo_root;
use syn::visit::Visit;

const GATE: &str = "crates/orchestrators/boss-cli/src/gate.rs";
const RULES: &str = "infra/platform/documents/builder-rules.md";

/// Every string literal inside the initializer of `let given` in
/// `fn require_item_answer` — the flags the refusal counts.
fn item_answer_flags() -> Vec<String> {
    struct Find {
        in_fn: bool,
        flags: Vec<String>,
    }
    struct Lits<'a>(&'a mut Vec<String>);
    impl<'ast> Visit<'ast> for Lits<'_> {
        fn visit_lit_str(&mut self, l: &'ast syn::LitStr) {
            self.0.push(l.value());
        }
    }
    impl<'ast> Visit<'ast> for Find {
        fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
            let was = self.in_fn;
            self.in_fn = f.sig.ident == "require_item_answer";
            syn::visit::visit_impl_item_fn(self, f);
            self.in_fn = was;
        }
        fn visit_local(&mut self, l: &'ast syn::Local) {
            let named_given = match &l.pat {
                syn::Pat::Type(t) => matches!(&*t.pat, syn::Pat::Ident(i) if i.ident == "given"),
                syn::Pat::Ident(i) => i.ident == "given",
                _ => false,
            };
            if self.in_fn
                && named_given
                && let Some(init) = &l.init
            {
                Lits(&mut self.flags).visit_expr(&init.expr);
            }
            syn::visit::visit_local(self, l);
        }
    }
    let src = std::fs::read_to_string(repo_root().join(GATE)).expect("gate.rs is readable");
    let file = syn::parse_file(&src).expect("gate.rs parses");
    let mut find = Find {
        in_fn: false,
        flags: Vec::new(),
    };
    find.visit_file(&file);
    let flags: Vec<String> = find
        .flags
        .into_iter()
        .filter(|f| f.starts_with("--park-"))
        .collect();
    // A derivation that found nothing is a broken scan, not an empty
    // set — and one that found a single flag would pass on the very
    // template this test exists to refuse.
    assert!(
        flags.len() >= 2 && flags.iter().any(|f| f == "--park-backlog-item"),
        "could not read the item-answer flags out of `let given` in \
         `require_item_answer` ({GATE}); found {flags:?}"
    );
    flags
}

/// Rule 7's own text: from its number to the next rule's.
fn rule_seven() -> String {
    let text = std::fs::read_to_string(repo_root().join(RULES)).expect("the builder rules");
    let start = text.find("\n7. ").expect("the builder rules have a rule 7");
    let rest = &text[start + 1..];
    let end = rest.find("\n8. ").expect("the builder rules have a rule 8");
    rest[..end].to_string()
}

#[test]
fn rule_seven_names_every_item_answer_the_gate_accepts() {
    let rule = rule_seven();
    for flag in item_answer_flags() {
        assert!(
            rule.contains(&flag),
            "rule 7 of {RULES} must name `{flag}` — the gate accepts it as an item \
             answer, and a builder who never sees it links the car to the wrong \
             packet or to none (backlog 0906495a)"
        );
    }
}
