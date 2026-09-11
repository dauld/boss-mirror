//! leaked_policy — count the code branches CLAUDE.md §9 names, over an
//! AST rather than a regex.
//!
//! WHY THIS EXISTS. `infra/codebase-metrics.sh` files a daily row
//! measuring David's medium-term trajectory — "shift more work to data".
//! It counts the REGISTRY half of §9 exactly (239 rows across five
//! registries, 2026-09-11) and deliberately recorded the other half as
//! `null` with a written reason, because a regex cannot tell a leaked
//! workflow policy from the dispatcher's own handler table, a serde
//! round-trip, or an HTTP route — and a number nobody can interpret is
//! worse than a blank, because it gets quoted. This is the pass that
//! replaces the blank.
//!
//! THE CLASSIFICATION RULE IS THE DELIVERABLE. The integer is a
//! consequence of it, so the rule is written out here in the order that
//! defines it, and every rung is pinned by a test in
//! `tests/leaked_policy.rs` whose fixture is COPIED from the tree this
//! measures.
//!
//! TWO GATES, and neither alone is enough — which is exactly why a regex
//! fails:
//!
//!   1. THE SCRUTINEE NAMES A KIND. The trailing identifier of
//!      `match <expr>` is `kind` or ends `_kind` (`step_kind`,
//!      `subject_kind`, `ev.kind.as_str()`, `subject.kind()`). That is
//!      literally §9's `match kind { "refurb-used" => … }`.
//!   2. AN ARM LITERAL IS A REGISTRY-DECLARED KIND, read LIVE from the
//!      registry seeds of the measured tree — never a list kept here
//!      (CLAUDE.md §9a: a fact that lives twice drifts, so this one
//!      lives once, in the registries).
//!
//! Gate 1 alone is what excuses the gateway's MIME table, whose `"html"`
//! arm collides with a registry-declared kind in this very tree. Gate 2
//! alone is what excuses every rebuilder's fold over event kinds, whose
//! scrutinee is `ev.kind.as_str()`. A count keyed on either half is
//! confidently wrong in one direction or the other.
//!
//! THE LADDER, in order. Order IS the definition, the way
//! `codebase-metrics.sh`'s own `bucket()` is ordered:
//!
//!   1. [`Class::RoundTrip`] — every arm body is a unit enum path
//!      (`Self::Draft`, `Ok(Self::SignOff)`). A string and a closed Rust
//!      enum, the inverse of `as_str`. First, so that a `FromStr` whose
//!      vocabulary happens to collide with a registry kind (policy
//!      `Action::SignOff` vs. the `sign-off` step kind) cannot be read
//!      as a leak.
//!   2. [`Class::HandlerTable`] — the match sits in an `impl` of a
//!      resolver/handler trait, or in a `call`/`dispatch`/`handle` whose
//!      scrutinee is a helper NAME. This is the table that EXECUTES
//!      registry rows; deleting it would mean the registry does nothing.
//!   3. [`Class::EventFold`] — gate 1 holds and every literal is an
//!      event topic (`jobs.job.created`). A projection's fold over the
//!      log: the log's vocabulary, not a registry's.
//!   4. [`Class::LeakedPolicy`] — gates 1 and 2. THE NUMBER.
//!   5. [`Class::ExternalVocabulary`] — gate 1 fails and no literal is a
//!      registry kind: file extensions, MIME types, JSON-Schema
//!      primitives, HTTP methods, CLI verbs. Nothing in BOSS owns these
//!      names, so no registry row could replace the branch.
//!   6. The two `Unclassified*` rungs — reported as their own number
//!      with their own reason, never forced into a bucket above. A count
//!      with `unclassified: 7` is usable; a count that silently guesses
//!      is the thing this pass exists to avoid.
//!
//! WHAT IS NOT MEASURED: test code, in both its shapes — a `tests/` or
//! `benches/` directory and an inline `#[cfg(test)]` item, which is most
//! of this repo's Rust tests (29% of non-`tests/` Rust lines, measured
//! 2026-09-11). `no-step-kind-match.sh` exempts tests for the same
//! reason: a test PINS a fixture, it does not decide behaviour.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Where a site landed on the ladder. One site, one class — the
/// conservation law the real-tree test asserts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    /// Gates 1 and 2: core code that must be edited to add a registry
    /// row. The number the daily row carries.
    LeakedPolicy,
    /// A string and a closed Rust enum — the inverse of `as_str`.
    RoundTrip,
    /// The table that executes registry rows.
    HandlerTable,
    /// A projection folding over event kinds.
    EventFold,
    /// A vocabulary nothing in BOSS owns.
    ExternalVocabulary,
    /// Gate 1 holds, gate 2 does not, and the literals are not event
    /// topics: a kind-shaped match over names no registry declares.
    UnclassifiedKindUnknownVocabulary,
    /// Gate 2 holds, gate 1 does not: a registry kind under a scrutinee
    /// that does not name one. A leak can hide behind a variable name,
    /// and saying so is cheaper than guessing either way.
    UnclassifiedRegistryLiteralOffKind,
}

impl Class {
    /// The key this class carries in the JSON `by_class` object and on
    /// the daily packet. Stable: a reader queries these names.
    pub fn key(self) -> &'static str {
        match self {
            Class::LeakedPolicy => "leaked_policy",
            Class::RoundTrip => "round_trip",
            Class::HandlerTable => "handler_table",
            Class::EventFold => "event_fold",
            Class::ExternalVocabulary => "external_vocabulary",
            Class::UnclassifiedKindUnknownVocabulary => "unclassified_kind_unknown_vocabulary",
            Class::UnclassifiedRegistryLiteralOffKind => "unclassified_registry_literal_off_kind",
        }
    }

    /// Whether this class is a thing the pass declined to decide. Both
    /// rungs are reported; neither is counted as leaked policy.
    pub fn is_unclassified(self) -> bool {
        matches!(
            self,
            Class::UnclassifiedKindUnknownVocabulary | Class::UnclassifiedRegistryLiteralOffKind
        )
    }
}

/// One `match` expression with at least one string-literal arm pattern.
/// A site names its file and line so the number is auditable by hand —
/// the calibration this pass was built against was five files read this
/// way, and the next reader deserves the same door.
#[derive(Debug, Clone)]
pub struct Site {
    pub file: String,
    pub line: usize,
    /// The scrutinee's trailing identifier, after unwrapping `&`, `*`,
    /// `?`, parens and the string-ifying calls (`as_str`, `as_deref`,
    /// `trim`, `to_lowercase`). `""` when the scrutinee is not a path,
    /// field or method call (a tuple, say).
    pub scrutinee: String,
    /// Every string literal in the arm patterns, in source order.
    pub literals: Vec<String>,
    /// The subset of `literals` a registry declares — the evidence for
    /// gate 2, in the order they appear.
    pub registry_literals: Vec<String>,
    pub class: Class,
}

/// The kind vocabulary of the measured tree, read from the registries
/// themselves. Never a list kept in this file: the registries are the
/// one definition, and a second copy here is the drift §9a is about.
#[derive(Debug, Clone, Default)]
pub struct Vocabulary {
    kinds: BTreeSet<String>,
    /// `(path relative to the repo, kinds contributed)`, for the row's
    /// method string — a vocabulary whose provenance is unstated is a
    /// number whose denominator is unstated.
    sources: Vec<(String, usize)>,
    /// Sources that EXIST and could not be read or parsed. Kept rather
    /// than swallowed: a seed file this pass silently skipped would
    /// shrink the vocabulary, and a shrunken vocabulary under-reports
    /// leaked policy while looking exactly as confident. [`scan`]
    /// refuses on a non-empty list.
    errors: Vec<(String, String)>,
}

/// The registry seeds the vocabulary is read from, relative to the
/// measured tree, paired with the TOML keys that hold a kind name.
///
/// These are the same files `codebase-metrics.sh` counts rows in, which
/// is the point: the count of registry rows and the count of code
/// branches are measured against ONE vocabulary. A glob segment `*`
/// matches one directory level.
const VOCABULARY_SOURCES: &[(&str, &[&str])] = &[
    // Step kinds — the alphabet of legal transitions.
    ("crates/core/boss-jobs/seeds/step_types.toml", &["kind"]),
    // Workflow kinds, and the step kinds their specs name.
    ("infra/platform/workflows/*.toml", &["kind"]),
    ("examples/*/seeds/workflows.toml", &["kind"]),
    // Subject kinds — what a Job can be about.
    (
        "crates/core/boss-subject-kinds/seeds/subject_identity_sources.toml",
        &["subject_kind"],
    ),
];

impl Vocabulary {
    /// A vocabulary stated outright. Tests use this so their assertions
    /// are about the RULE rather than about today's registry contents.
    pub fn from_kinds<I, S>(kinds: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let kinds: BTreeSet<String> = kinds.into_iter().map(Into::into).collect();
        let n = kinds.len();
        Vocabulary {
            kinds,
            sources: vec![("(stated inline)".to_string(), n)],
            errors: Vec::new(),
        }
    }

    /// Read every registry seed in [`VOCABULARY_SOURCES`] out of `repo`.
    ///
    /// A MISSING SOURCE IS NOT AN ERROR HERE — it contributes zero and
    /// says so in `sources`. The refusal belongs one level up, in
    /// [`scan`]: a tree with NO registry vocabulary at all cannot be
    /// measured, and reporting `0` for it would be a measurement of the
    /// machine dressed as a measurement of the codebase.
    pub fn read(repo: &Path) -> Self {
        let mut kinds = BTreeSet::new();
        let mut sources = Vec::new();
        let mut errors = Vec::new();
        for (pattern, keys) in VOCABULARY_SOURCES {
            for path in expand_one_star(repo, pattern) {
                let before = kinds.len();
                let rel = path
                    .strip_prefix(repo)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                match std::fs::read_to_string(&path) {
                    // `Table`, not `Value`: `FromStr for toml::Value`
                    // parses a VALUE expression, so handing it a document
                    // fails at line 1 column 1 with "unexpected content,
                    // expected nothing". The first cut of this swallowed
                    // that error and reported an empty vocabulary, which
                    // is the reduction-before-the-record defect
                    // (CLAUDE.md §Diagnosis) in miniature — the refusal
                    // above is what turned it into one legible line.
                    Ok(text) => match text.parse::<toml::Table>() {
                        Ok(table) => {
                            collect_keys(&toml::Value::Table(table), keys, &mut kinds);
                        }
                        Err(e) => errors.push((rel.clone(), e.to_string())),
                    },
                    Err(e) => errors.push((rel.clone(), e.to_string())),
                }
                sources.push((rel, kinds.len() - before));
            }
        }
        Vocabulary {
            kinds,
            sources,
            errors,
        }
    }

    pub fn len(&self) -> usize {
        self.kinds.len()
    }

    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }

    pub fn contains(&self, kind: &str) -> bool {
        self.kinds.contains(kind)
    }

    /// `(source path, kinds contributed)` in read order.
    pub fn sources(&self) -> &[(String, usize)] {
        &self.sources
    }

    /// `(source path, why)` for sources that exist and could not be
    /// read. A non-empty list is an infrastructure refusal, never a
    /// smaller vocabulary.
    pub fn errors(&self) -> &[(String, String)] {
        &self.errors
    }
}

/// Walk a parsed TOML value collecting every string at any of `keys`, at
/// any depth. Depth matters: a workflow file holds its own `kind` at the
/// top level and one per step inside `[[workflow.steps]]`, and both are
/// registry-declared kinds.
fn collect_keys(value: &toml::Value, keys: &[&str], out: &mut BTreeSet<String>) {
    match value {
        toml::Value::Table(table) => {
            for (k, v) in table {
                if keys.contains(&k.as_str())
                    && let Some(s) = v.as_str()
                {
                    out.insert(s.to_string());
                }
                collect_keys(v, keys, out);
            }
        }
        toml::Value::Array(items) => {
            for v in items {
                collect_keys(v, keys, out);
            }
        }
        _ => {}
    }
}

/// Expand a pattern with at most one `*` segment into real paths, sorted.
/// A whole glob crate for `infra/platform/workflows/*.toml` would be a
/// dependency carrying more than it is asked for.
fn expand_one_star(repo: &Path, pattern: &str) -> Vec<PathBuf> {
    let Some((before, after)) = pattern.split_once('*') else {
        // A source that is not there contributes nothing and is not an
        // error: a tree may predate a registry, or be a fixture with one
        // of them. Only a source that EXISTS and cannot be read is a
        // refusal, which is the distinction the glob branch below gets
        // for free from `is_file()`.
        let path = repo.join(pattern);
        return if path.is_file() {
            vec![path]
        } else {
            Vec::new()
        };
    };
    // `before` ends at a directory boundary; `after` is either an
    // extension (`.toml`) or a remaining path (`/seeds/workflows.toml`).
    let dir = repo.join(before.trim_end_matches('/'));
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if after.starts_with('/') {
                let candidate = dir.join(&name).join(after.trim_start_matches('/'));
                candidate.is_file().then_some(candidate)
            } else if name.ends_with(after) {
                let candidate = dir.join(&name);
                candidate.is_file().then_some(candidate)
            } else {
                None
            }
        })
        .collect();
    out.sort();
    out
}

/// What a scan found. Every number on it is a function of the tree, so a
/// disagreement is reproducible rather than arguable.
#[derive(Debug, Clone)]
pub struct Report {
    /// The scopes scanned, relative to the repo.
    pub scopes: Vec<String>,
    /// `.rs` files read (test directories already excluded).
    pub files_parsed: usize,
    /// Files `syn` could not parse. Non-zero means the count is short by
    /// an unknown amount, so [`scan`] refuses rather than reporting.
    pub unparsable_files: usize,
    /// Sites found: `match` expressions with at least one string-literal
    /// arm pattern, outside test code.
    pub matches: usize,
    /// Gates 1 and 2 — the integer the daily row carries.
    pub leaked_policy: usize,
    /// Sites the pass declined to decide, summed across both rungs.
    pub unclassified: usize,
    pub by_class: BTreeMap<String, usize>,
    /// Round trips whose ENTIRE vocabulary is registry-declared — a
    /// closed Rust enum over registry kinds. Not leaked policy (it names
    /// values, it does not branch behaviour) but worth a number of its
    /// own, because §9's taxonomy half is exactly "a closed enum forces
    /// every tenant to fork core to add a value".
    pub round_trip_over_registry_kinds: usize,
    pub vocabulary_kinds: usize,
    pub vocabulary_sources: Vec<(String, usize)>,
    /// Every site, in file order. The audit trail.
    pub sites: Vec<Site>,
}

/// Why a scan could not answer. Distinct from "answered zero" on
/// purpose: the same distinction `infra/lint/lib/git-answer.sh` draws,
/// and the one four lints threw away on 2026-09-11.
#[derive(Debug)]
pub enum ScanError {
    /// No scope directory existed under the root.
    NoScope { root: PathBuf, scopes: Vec<String> },
    /// The tree declares no registry kinds, so gate 2 can never hold and
    /// every site would classify as if BOSS had no registries. A
    /// measurement of the machine, not of the codebase.
    NoVocabulary { root: PathBuf },
    /// At least one `.rs` file could not be read or parsed.
    Unparsable { files: Vec<(String, String)> },
    /// A registry seed exists and could not be read. Swallowing it would
    /// shrink the vocabulary, and a shrunken vocabulary under-reports
    /// leaked policy while looking exactly as confident.
    UnreadableVocabulary { sources: Vec<(String, String)> },
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScanError::NoScope { root, scopes } => write!(
                f,
                "none of the scopes {} exist under {} — nothing was read, so there is \
                 no count (an infrastructure refusal, not a codebase with no branches in it)",
                scopes.join(", "),
                root.display()
            ),
            ScanError::NoVocabulary { root } => write!(
                f,
                "{} declares no registry kinds (looked in {}) — gate 2 could never hold, \
                 so every site would classify as if this tree had no registries. \
                 Refusing rather than reporting 0 leaked branches.",
                root.display(),
                VOCABULARY_SOURCES
                    .iter()
                    .map(|(p, _)| *p)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            ScanError::Unparsable { files } => {
                write!(
                    f,
                    "{} file(s) could not be parsed, so the count would be short by an \
                     unknown amount:",
                    files.len()
                )?;
                for (path, why) in files {
                    write!(f, "\n  {path}: {why}")?;
                }
                Ok(())
            }
            ScanError::UnreadableVocabulary { sources } => {
                write!(
                    f,
                    "{} registry seed(s) exist and could not be read, so the kind \
                     vocabulary would be short and the leaked count with it:",
                    sources.len()
                )?;
                for (path, why) in sources {
                    write!(f, "\n  {path}: {why}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ScanError {}

/// Scan `scopes` under `repo`, classifying every string-literal `match`.
///
/// The vocabulary is read from `repo`, not from the ambient checkout, so
/// the answer is a function of the tree handed in — which is what lets
/// `codebase-metrics.sh` run this against an extracted `git archive` and
/// get a number that belongs to the sha it is measuring.
pub fn scan(repo: &Path, scopes: &[&str]) -> Result<Report, ScanError> {
    let vocabulary = Vocabulary::read(repo);
    if !vocabulary.errors().is_empty() {
        return Err(ScanError::UnreadableVocabulary {
            sources: vocabulary.errors().to_vec(),
        });
    }
    if vocabulary.is_empty() {
        return Err(ScanError::NoVocabulary {
            root: repo.to_path_buf(),
        });
    }

    let mut files: Vec<PathBuf> = Vec::new();
    let mut any_scope = false;
    for scope in scopes {
        let dir = repo.join(scope);
        if dir.is_dir() {
            any_scope = true;
            collect_rust_files(&dir, &mut files);
        }
    }
    if !any_scope {
        return Err(ScanError::NoScope {
            root: repo.to_path_buf(),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
        });
    }
    files.sort();

    let mut sites = Vec::new();
    let mut unparsable = Vec::new();
    for file in &files {
        let rel = file
            .strip_prefix(repo)
            .unwrap_or(file)
            .to_string_lossy()
            .into_owned();
        let source = match std::fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => {
                unparsable.push((rel, e.to_string()));
                continue;
            }
        };
        match classify_source(&rel, &source, &vocabulary) {
            Ok(found) => sites.extend(found),
            Err(e) => unparsable.push((rel, e.to_string())),
        }
    }
    if !unparsable.is_empty() {
        return Err(ScanError::Unparsable { files: unparsable });
    }

    let mut by_class: BTreeMap<String, usize> = BTreeMap::new();
    for class in [
        Class::LeakedPolicy,
        Class::RoundTrip,
        Class::HandlerTable,
        Class::EventFold,
        Class::ExternalVocabulary,
        Class::UnclassifiedKindUnknownVocabulary,
        Class::UnclassifiedRegistryLiteralOffKind,
    ] {
        // Every key, always, zeros included: a stable row shape is what
        // makes a series queryable without the reader guessing which
        // keys a given day happened to have.
        by_class.insert(class.key().to_string(), 0);
    }
    for site in &sites {
        *by_class.entry(site.class.key().to_string()).or_insert(0) += 1;
    }

    let round_trip_over_registry_kinds = sites
        .iter()
        .filter(|s| {
            s.class == Class::RoundTrip
                && !s.literals.is_empty()
                && s.registry_literals.len() == s.literals.len()
        })
        .count();

    Ok(Report {
        scopes: scopes.iter().map(|s| s.to_string()).collect(),
        files_parsed: files.len(),
        unparsable_files: 0,
        matches: sites.len(),
        leaked_policy: sites
            .iter()
            .filter(|s| s.class == Class::LeakedPolicy)
            .count(),
        unclassified: sites.iter().filter(|s| s.class.is_unclassified()).count(),
        by_class,
        round_trip_over_registry_kinds,
        vocabulary_kinds: vocabulary.len(),
        vocabulary_sources: vocabulary.sources().to_vec(),
        sites,
    })
}

/// `.rs` files under `dir`, excluding test code by PATH. The other half
/// of the test exclusion — inline `#[cfg(test)]` items — is done in the
/// visitor, because it cannot be done by path at all.
fn collect_rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if name == "tests" || name == "benches" || name == "target" {
                continue;
            }
            collect_rust_files(&path, out);
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
}

/// Classify every string-literal `match` in one Rust source.
///
/// Takes the source as text rather than a path so the tests can state a
/// fixture inline — the snippets in `tests/leaked_policy.rs` are copied
/// from real files, and a fixture that has to be written to disk is a
/// fixture nobody keeps faithful to its original.
pub fn classify_source(
    file: &str,
    source: &str,
    vocabulary: &Vocabulary,
) -> Result<Vec<Site>, syn::Error> {
    let parsed = syn::parse_file(source)?;
    let mut visitor = Visitor {
        file: file.to_string(),
        vocabulary,
        impl_traits: Vec::new(),
        fn_names: Vec::new(),
        sites: Vec::new(),
    };
    syn::visit::Visit::visit_file(&mut visitor, &parsed);
    Ok(visitor.sites)
}

struct Visitor<'a> {
    file: String,
    vocabulary: &'a Vocabulary,
    /// Trait names of the `impl Trait for Type` blocks currently open.
    impl_traits: Vec<String>,
    /// Names of the functions currently open.
    fn_names: Vec<String>,
    sites: Vec<Site>,
}

/// `#[cfg(test)]` in any of its shapes — the attribute's tokens
/// mentioning `test` is enough, and deliberately so: `cfg(test)`,
/// `cfg(any(test, feature = "…"))` and `cfg_attr(test, …)` are all test
/// code, and a pass that only recognised the first would measure the
/// others.
fn has_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        let Some(ident) = attr.path().get_ident() else {
            return false;
        };
        if ident != "cfg" && ident != "cfg_attr" {
            return false;
        }
        attr.meta
            .require_list()
            .map(|list| {
                list.tokens
                    .to_string()
                    .split(|c: char| !c.is_alphanumeric() && c != '_')
                    .any(|t| t == "test")
            })
            .unwrap_or(false)
    })
}

impl<'ast> syn::visit::Visit<'ast> for Visitor<'_> {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        let attrs: &[syn::Attribute] = match item {
            syn::Item::Mod(i) => &i.attrs,
            syn::Item::Fn(i) => &i.attrs,
            syn::Item::Impl(i) => &i.attrs,
            syn::Item::Struct(i) => &i.attrs,
            syn::Item::Enum(i) => &i.attrs,
            syn::Item::Trait(i) => &i.attrs,
            syn::Item::Const(i) => &i.attrs,
            syn::Item::Static(i) => &i.attrs,
            _ => &[],
        };
        if has_cfg_test(attrs) {
            return;
        }
        syn::visit::visit_item(self, item);
    }

    fn visit_impl_item(&mut self, item: &'ast syn::ImplItem) {
        let attrs: &[syn::Attribute] = match item {
            syn::ImplItem::Fn(i) => &i.attrs,
            syn::ImplItem::Const(i) => &i.attrs,
            syn::ImplItem::Type(i) => &i.attrs,
            _ => &[],
        };
        if has_cfg_test(attrs) {
            return;
        }
        syn::visit::visit_impl_item(self, item);
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        // `impl HelperResolver for InventoryHelpers` — the trait name is
        // what marks a handler table, so it has to be in scope when the
        // match inside is classified.
        let trait_name = node
            .trait_
            .as_ref()
            .and_then(|(_, path, _)| path.segments.last())
            .map(|seg| seg.ident.to_string())
            .unwrap_or_default();
        self.impl_traits.push(trait_name);
        syn::visit::visit_item_impl(self, node);
        self.impl_traits.pop();
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.fn_names.push(node.sig.ident.to_string());
        syn::visit::visit_item_fn(self, node);
        self.fn_names.pop();
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.fn_names.push(node.sig.ident.to_string());
        syn::visit::visit_impl_item_fn(self, node);
        self.fn_names.pop();
    }

    fn visit_expr_match(&mut self, node: &'ast syn::ExprMatch) {
        let mut literals = Vec::new();
        for arm in &node.arms {
            collect_pattern_literals(&arm.pat, &mut literals);
        }
        if !literals.is_empty() {
            let scrutinee = scrutinee_name(&node.expr);
            let registry_literals: Vec<String> = literals
                .iter()
                .filter(|l| self.vocabulary.contains(l))
                .cloned()
                .collect();
            let class = classify(
                &scrutinee,
                &literals,
                &registry_literals,
                node,
                self.impl_traits.last().map(String::as_str).unwrap_or(""),
                self.fn_names.last().map(String::as_str).unwrap_or(""),
            );
            self.sites.push(Site {
                file: self.file.clone(),
                line: node.match_token.span.start().line,
                scrutinee,
                literals,
                registry_literals,
                class,
            });
        }
        // Nested matches are their own sites — an arm body holding a
        // second match is two decisions, not one.
        syn::visit::visit_expr_match(self, node);
    }
}

/// THE LADDER. Order is the definition; each rung is pinned by a test.
fn classify(
    scrutinee: &str,
    literals: &[String],
    registry_literals: &[String],
    node: &syn::ExprMatch,
    impl_trait: &str,
    fn_name: &str,
) -> Class {
    // 1. A string and a closed Rust enum. First, so a `FromStr` whose
    //    vocabulary collides with a registry kind by coincidence
    //    (policy `Action::SignOff` vs. the `sign-off` step kind) is not
    //    read as a leak.
    //
    //    Only the arms that MATCH A STRING are examined. The catch-all
    //    is how a conversion reports failure — `_ => Err(format!("unknown
    //    action {s}"))` — and building that error is not a branch on a
    //    kind. Judging the catch-all too would call half the tree's
    //    round trips something else.
    if node
        .arms
        .iter()
        .filter(|arm| {
            let mut lits = Vec::new();
            collect_pattern_literals(&arm.pat, &mut lits);
            !lits.is_empty()
        })
        .all(|arm| is_unit_variant_body(&arm.body))
    {
        return Class::RoundTrip;
    }

    // 2. The table that EXECUTES registry rows. Either the enclosing
    //    `impl` is of a resolver/handler trait, or the enclosing fn is a
    //    dispatch entry point over a helper NAME.
    let trait_is_handler = impl_trait.ends_with("Resolver")
        || impl_trait.ends_with("Handler")
        || impl_trait.ends_with("Handlers")
        || impl_trait.ends_with("Helpers");
    let fn_is_dispatch = matches!(fn_name, "call" | "dispatch" | "handle" | "invoke");
    let scrutinee_is_a_name = matches!(scrutinee, "name" | "helper" | "handler" | "verb" | "op");
    if trait_is_handler || (fn_is_dispatch && scrutinee_is_a_name) {
        return Class::HandlerTable;
    }

    // GATE 1 — the scrutinee names a kind. Literally §9's `match kind`.
    let kind_like = scrutinee == "kind" || scrutinee.ends_with("_kind");

    // GATE 2 — the match ENUMERATES registry kinds: at least one arm
    // literal is registry-declared, and at least half of them are.
    //
    // THE HALF IS LOAD-BEARING and it is the one judgement in this rule.
    // A branch that genuinely dispatches on a kind enumerates kinds, so
    // nearly all of its literals are registry rows. A lone registry
    // spelling among foreign ones is a COLLISION: the gateway's
    // `guess_content_type` has an `"html"` arm, and `html` is also a
    // workflow kind in this tree. Membership alone would call a MIME
    // table leaked policy, and a reviewer never would.
    let gate_two = !registry_literals.is_empty() && registry_literals.len() * 2 >= literals.len();

    // 3. A projection folding over the log. Gate 1 holds and every
    //    literal is a dotted event topic — the log's own vocabulary.
    if kind_like && literals.iter().all(|l| l.contains('.')) {
        return Class::EventFold;
    }

    // 4. BOTH GATES. The number.
    if kind_like && gate_two {
        return Class::LeakedPolicy;
    }

    // 5. Neither gate: a vocabulary nothing in BOSS owns, so no registry
    //    row could replace the branch.
    if !kind_like && !gate_two {
        return Class::ExternalVocabulary;
    }

    // 6. One gate each. Reported, not guessed.
    if kind_like {
        Class::UnclassifiedKindUnknownVocabulary
    } else {
        Class::UnclassifiedRegistryLiteralOffKind
    }
}

/// Every string literal in an arm pattern, including `|` alternatives
/// and the `&"x"` / `Some("x")` shapes. A pattern that binds or wilds
/// contributes nothing, which is how the `_ =>` arm stays invisible.
fn collect_pattern_literals(pat: &syn::Pat, out: &mut Vec<String>) {
    match pat {
        syn::Pat::Lit(lit) => {
            if let syn::Lit::Str(s) = &lit.lit {
                out.push(s.value());
            }
        }
        syn::Pat::Or(or) => {
            for case in &or.cases {
                collect_pattern_literals(case, out);
            }
        }
        syn::Pat::Reference(r) => collect_pattern_literals(&r.pat, out),
        syn::Pat::Paren(p) => collect_pattern_literals(&p.pat, out),
        syn::Pat::TupleStruct(ts) => {
            for elem in &ts.elems {
                collect_pattern_literals(elem, out);
            }
        }
        syn::Pat::Tuple(t) => {
            for elem in &t.elems {
                collect_pattern_literals(elem, out);
            }
        }
        _ => {}
    }
}

/// An arm body that names a variant rather than doing anything:
/// `Self::Draft`, `JobStatus::Open`, `Ok(Self::SignOff)`,
/// `Some(Self::Pto)`. Also a bare `return None` / `None`, which is how
/// the catch-all arm of a conversion is written.
fn is_unit_variant_body(expr: &syn::Expr) -> bool {
    match expr {
        syn::Expr::Path(path) => {
            let segments = &path.path.segments;
            // `Self::Draft` / `JobStatus::Open` / `None`.
            segments.len() >= 2
                || segments
                    .first()
                    .is_some_and(|s| s.ident.to_string().starts_with(char::is_uppercase))
        }
        syn::Expr::Call(call) => {
            // `Ok(Self::Read)` / `Some(Self::Pto)` — a one-argument
            // wrapper around a variant, and nothing else.
            let wrapper = match &*call.func {
                syn::Expr::Path(p) => p
                    .path
                    .segments
                    .last()
                    .map(|s| s.ident.to_string())
                    .unwrap_or_default(),
                _ => String::new(),
            };
            matches!(wrapper.as_str(), "Ok" | "Some" | "Err")
                && call.args.len() == 1
                && call.args.iter().all(is_unit_variant_body)
        }
        // `_ => return None` closes a conversion without doing work.
        syn::Expr::Return(ret) => ret
            .expr
            .as_ref()
            .is_none_or(|inner| is_unit_variant_body(inner)),
        syn::Expr::Macro(mac) => {
            // `unreachable!()` / `todo!()` — still not work.
            let name = mac
                .mac
                .path
                .segments
                .last()
                .map(|s| s.ident.to_string())
                .unwrap_or_default();
            matches!(name.as_str(), "unreachable" | "todo" | "unimplemented")
        }
        syn::Expr::Paren(p) => is_unit_variant_body(&p.expr),
        _ => false,
    }
}

/// The scrutinee's trailing identifier — what the code CALLS the thing
/// it is matching on. `step_kind`, `ev.kind.as_str()` and
/// `subject.kind()` all answer `kind`-ish, because the string-ifying
/// calls are peeled and a method's own name is what a field access would
/// have been.
fn scrutinee_name(expr: &syn::Expr) -> String {
    /// Calls that turn a thing into the string being matched. They say
    /// nothing about WHAT is matched, so they are peeled.
    const PEELED: &[&str] = &[
        "as_str",
        "as_deref",
        "as_ref",
        "trim",
        "to_lowercase",
        "to_ascii_lowercase",
        "to_uppercase",
        "unwrap_or",
        "unwrap_or_default",
        "unwrap",
        "clone",
        "borrow",
        "deref",
    ];
    match expr {
        syn::Expr::Path(p) => p
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default(),
        syn::Expr::Field(f) => match &f.member {
            syn::Member::Named(ident) => ident.to_string(),
            syn::Member::Unnamed(_) => String::new(),
        },
        syn::Expr::MethodCall(m) => {
            let method = m.method.to_string();
            if PEELED.contains(&method.as_str()) {
                scrutinee_name(&m.receiver)
            } else {
                method
            }
        }
        syn::Expr::Reference(r) => scrutinee_name(&r.expr),
        syn::Expr::Unary(u) => scrutinee_name(&u.expr),
        syn::Expr::Paren(p) => scrutinee_name(&p.expr),
        syn::Expr::Group(g) => scrutinee_name(&g.expr),
        syn::Expr::Try(t) => scrutinee_name(&t.expr),
        syn::Expr::Await(a) => scrutinee_name(&a.base),
        _ => String::new(),
    }
}

/// The report as the JSON `codebase-metrics.sh` consumes.
///
/// Hand-written rather than `serde_json` derives: the key names ARE the
/// contract with the shell script and the packet, so they are spelled
/// out where a reader of either can find them. `sites` carries only the
/// two buckets a human goes and reads — the leaked ones and the
/// undecided ones — because a full dump of 200 sites on a daily packet
/// is noise, and the pass can always be re-run for the rest.
impl Report {
    pub fn to_json(&self) -> serde_json::Value {
        let site_json = |site: &Site| {
            serde_json::json!({
                "file": site.file,
                "line": site.line,
                "scrutinee": site.scrutinee,
                "literals": site.literals,
                "registry_literals": site.registry_literals,
            })
        };
        let of_class = |class: Class| -> Vec<serde_json::Value> {
            self.sites
                .iter()
                .filter(|s| s.class == class)
                .map(site_json)
                .collect()
        };
        serde_json::json!({
            "scopes": self.scopes,
            "files_parsed": self.files_parsed,
            "matches": self.matches,
            "leaked_policy": self.leaked_policy,
            "unclassified": self.unclassified,
            "by_class": self.by_class,
            "round_trip_over_registry_kinds": self.round_trip_over_registry_kinds,
            "vocabulary": {
                "kinds": self.vocabulary_kinds,
                "sources": self.vocabulary_sources.iter()
                    .map(|(p, n)| serde_json::json!({"path": p, "kinds": n}))
                    .collect::<Vec<_>>(),
            },
            "sites": {
                "leaked_policy": of_class(Class::LeakedPolicy),
                "unclassified_kind_unknown_vocabulary":
                    of_class(Class::UnclassifiedKindUnknownVocabulary),
                "unclassified_registry_literal_off_kind":
                    of_class(Class::UnclassifiedRegistryLiteralOffKind),
            },
            "method": METHOD,
        })
    }
}

/// How it was counted, on the same row as the number — the sentence that
/// replaces `code_branches_not_counted_why`. An approximation whose
/// limits are unwritten is indistinguishable from a precise number that
/// is wrong.
pub const METHOD: &str = "A `syn` AST pass over the scopes named in `scopes` (test code excluded \
by path AND by `#[cfg(test)]` attribute). A site is one `match` expression with at least one \
string-literal arm pattern. `leaked_policy` counts sites passing BOTH gates: the scrutinee's \
trailing identifier is `kind` or ends `_kind`, AND at least one arm literal is a kind declared by \
a registry seed in the measured tree (step_types.toml, the platform and tenant workflow rows, \
subject identity sources) — so the vocabulary cannot drift from the registries. Earlier rungs \
claim a site first, in this order: a string/closed-enum round trip (every arm body a unit \
variant); the resolver or handler table that EXECUTES registry rows; a projection folding over \
dotted event kinds. A site passing only one gate is reported under `unclassified` with which gate \
it passed, never forced into a bucket. Neither gate alone would do: the gateway's MIME table has \
an `\"html\"` arm that IS a registry-declared kind, and every rebuilder's fold has a `kind` \
scrutinee.";
