//! Tiny shared expression DSL.
//!
//! Operators:
//!   AND OR NOT
//!   =  !=  <  <=  >  >=
//!
//! Operands:
//!   - Literals: string "...", integer 123, boolean true/false, null
//!   - Identifiers: bareword — resolved against caller-supplied state
//!     (typically a JSON-shaped event payload, or a synthesized
//!     step-state bag for Workflow v2 predicates)
//!   - Function calls: name(arg, ...) — resolved against a helper-function
//!     table the caller registers
//!
//! Two consumers share this DSL today:
//!
//!   1. `boss-dispatcher` rule predicates (`rule.when`) and rule handler
//!      arg expressions (`do[].args`).
//!   2. `boss-jobs` `step.ready_when` predicates.
//!
//! The shared-DSL decision is recorded in
//! `docs/architecture-decisions.md` §Dispatcher — the event router.
//!
//! Hand-rolled recursive-descent parser + tree-walking evaluator. No
//! Turing-completeness, no recursion in the language itself, no loops —
//! the correctness of both consumers depends on expressions terminating,
//! and they do because the language can't express anything that wouldn't.

use std::fmt;
use thiserror::Error;

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

/// Runtime value produced by evaluating an expression.
///
/// The set of types is deliberately small. Payloads are JSON; this is what
/// the evaluator coerces into. Helper functions return Values; binary
/// operations consume them.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    /// A path that resolved to nothing — a missing key, a traversal into
    /// a non-object, or a present-but-unrepresentable array/object. It
    /// is FALSE in boolean position and UNEQUAL to every literal, so a
    /// predicate over optional metadata evaluates instead of raising
    /// (design 7b756357, David 2026-08-22): `job.metadata.x = "true"`
    /// over an absent `x` is a clean false, not an UnknownIdentifier
    /// that pins the step pending forever.
    Absent,
}

impl Value {
    /// Display kind for error messages — "string", "int", etc.
    pub fn kind(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::String(_) => "string",
            Value::Absent => "absent",
        }
    }

    /// True iff the value is a non-null bool. Used by AND/OR/NOT
    /// which require strict booleans (no truthy/falsy coercion — the
    /// DSL refuses to guess).
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            // Absent reads false in boolean position — `NOT job.metadata.x`
            // over an absent flag is true, `x AND y` is false — while
            // every other non-bool still refuses to coerce.
            Value::Absent => Some(false),
            _ => None,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => write!(f, "null"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Float(x) => write!(f, "{x}"),
            Value::String(s) => write!(f, "{s}"),
            Value::Absent => write!(f, "absent"),
        }
    }
}

// ---------------------------------------------------------------------------
// AST
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Literal(Value),
    /// Bareword resolved against the event payload at eval time.
    /// Supports dotted-path lookup: `subject.id` walks the JSON object.
    Identifier(Vec<String>),
    /// `name(arg1, arg2)`. Argument count + types are the helper's
    /// problem; the parser just collects them.
    FunctionCall(String, Vec<Expr>),
    BinaryOp(BinaryOp, Box<Expr>, Box<Expr>),
    UnaryOp(UnaryOp, Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    And,
    Or,
    Eq,
    Neq,
    Lt,
    Lte,
    Gt,
    Gte,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Not,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("unexpected end of input")]
    UnexpectedEnd,
    #[error("unexpected token {0:?} at position {1}")]
    UnexpectedToken(String, usize),
    #[error("unterminated string literal starting at position {0}")]
    UnterminatedString(usize),
    #[error("expected {expected}, found {found:?} at position {pos}")]
    Expected {
        expected: &'static str,
        found: String,
        pos: usize,
    },
}

#[derive(Debug, Error, PartialEq)]
pub enum EvalError {
    #[error("identifier {0:?} not found in payload")]
    UnknownIdentifier(String),
    #[error("helper function {0:?} not registered")]
    UnknownHelper(String),
    #[error("type error: expected {expected}, got {got}")]
    TypeError {
        expected: &'static str,
        got: &'static str,
    },
    #[error("helper {name:?} failed: {msg}")]
    HelperFailed { name: String, msg: String },
}

// ---------------------------------------------------------------------------
// Helper-function table
// ---------------------------------------------------------------------------

/// Resolves helper-function calls at evaluation time. The caller plugs in
/// a table of registered Rust functions; the evaluator never invents new
/// behavior. Adding a helper is a code change, as designed.
pub trait HelperResolver {
    fn call(&self, name: &str, args: &[Value]) -> Result<Value, EvalError>;
}

/// Empty helper table — useful for tests that only need literal/identifier/
/// operator coverage. Returns UnknownHelper on every call.
pub struct NoHelpers;

impl HelperResolver for NoHelpers {
    fn call(&self, name: &str, _args: &[Value]) -> Result<Value, EvalError> {
        Err(EvalError::UnknownHelper(name.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Evaluation context
// ---------------------------------------------------------------------------

/// What the evaluator needs at eval time. Payload supplies identifier
/// lookup; helpers supplies function-call resolution.
pub struct Context<'a> {
    pub payload: &'a serde_json::Value,
    pub helpers: &'a dyn HelperResolver,
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Parse a complete expression from source. Returns ParseError if the
/// input doesn't parse cleanly OR has trailing tokens after a valid
/// expression.
pub fn parse(src: &str) -> Result<Expr, ParseError> {
    let mut p = Parser::new(src);
    let expr = p.parse_or()?;
    p.skip_whitespace();
    if p.pos < p.src.len() {
        return Err(ParseError::UnexpectedToken(
            p.src[p.pos..].chars().take(20).collect(),
            p.pos,
        ));
    }
    Ok(expr)
}

/// Collect every identifier path referenced anywhere in the tree,
/// depth-first left-to-right. Duplicates are preserved — the caller
/// dedups if it cares.
///
/// This is what lets a predicate be read as a set of dependencies:
/// `boss-jobs` filters the returned paths for `steps.<title>.…` to
/// build the Workflow dependency index (D11) and the viability lint's
/// reachability graph; `subject.…` / `job.…` paths are inputs the
/// graph doesn't gate on. Pure structure walk — no evaluation, no
/// payload needed.
pub fn references(expr: &Expr) -> Vec<Vec<String>> {
    fn walk(expr: &Expr, out: &mut Vec<Vec<String>>) {
        match expr {
            Expr::Identifier(path) => out.push(path.clone()),
            Expr::FunctionCall(_, args) => args.iter().for_each(|a| walk(a, out)),
            Expr::BinaryOp(_, l, r) => {
                walk(l, out);
                walk(r, out);
            }
            Expr::UnaryOp(_, inner) => walk(inner, out),
            Expr::Literal(_) => {}
        }
    }
    let mut out = Vec::new();
    walk(expr, &mut out);
    out
}

struct Parser<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Self { src, pos: 0 }
    }

    fn skip_whitespace(&mut self) {
        while let Some(c) = self.peek_char() {
            if c.is_whitespace() {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
    }

    fn peek_char(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    /// Match a literal keyword (case-sensitive) — returns true and
    /// advances past it iff the keyword sits at `pos` and is followed
    /// by a non-identifier character (or end of input).
    fn match_keyword(&mut self, kw: &str) -> bool {
        let rest = &self.src[self.pos..];
        if !rest.starts_with(kw) {
            return false;
        }
        let after = &rest[kw.len()..];
        if let Some(c) = after.chars().next()
            && (c.is_alphanumeric() || c == '_')
        {
            return false;
        }
        self.pos += kw.len();
        true
    }

    /// Match an operator character sequence. Doesn't enforce word
    /// boundaries — operators are punctuation.
    fn match_punct(&mut self, p: &str) -> bool {
        if self.src[self.pos..].starts_with(p) {
            self.pos += p.len();
            true
        } else {
            false
        }
    }

    // Precedence climb: OR < AND < comparison < unary NOT < primary
    fn parse_or(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_and()?;
        loop {
            self.skip_whitespace();
            if self.match_keyword("OR") {
                let right = self.parse_and()?;
                left = Expr::BinaryOp(BinaryOp::Or, Box::new(left), Box::new(right));
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_cmp()?;
        loop {
            self.skip_whitespace();
            if self.match_keyword("AND") {
                let right = self.parse_cmp()?;
                left = Expr::BinaryOp(BinaryOp::And, Box::new(left), Box::new(right));
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_cmp(&mut self) -> Result<Expr, ParseError> {
        let left = self.parse_unary()?;
        self.skip_whitespace();
        // Check 2-char operators first so `<` doesn't shadow `<=`.
        let op = if self.match_punct("<=") {
            Some(BinaryOp::Lte)
        } else if self.match_punct(">=") {
            Some(BinaryOp::Gte)
        } else if self.match_punct("!=") {
            Some(BinaryOp::Neq)
        } else if self.match_punct("<") {
            Some(BinaryOp::Lt)
        } else if self.match_punct(">") {
            Some(BinaryOp::Gt)
        } else if self.match_punct("=") {
            Some(BinaryOp::Eq)
        } else {
            None
        };
        if let Some(op) = op {
            let right = self.parse_unary()?;
            Ok(Expr::BinaryOp(op, Box::new(left), Box::new(right)))
        } else {
            Ok(left)
        }
    }

    fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        self.skip_whitespace();
        if self.match_keyword("NOT") {
            let inner = self.parse_unary()?;
            return Ok(Expr::UnaryOp(UnaryOp::Not, Box::new(inner)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        self.skip_whitespace();
        let Some(c) = self.peek_char() else {
            return Err(ParseError::UnexpectedEnd);
        };

        if c == '(' {
            self.pos += 1;
            let inner = self.parse_or()?;
            self.skip_whitespace();
            if !self.match_punct(")") {
                return Err(ParseError::Expected {
                    expected: "')'",
                    found: self.src[self.pos..].chars().take(8).collect(),
                    pos: self.pos,
                });
            }
            return Ok(inner);
        }

        if c == '"' {
            return self.parse_string();
        }

        if c == '-' || c.is_ascii_digit() {
            return self.parse_number();
        }

        if c.is_alphabetic() || c == '_' {
            // Reserved words first.
            if self.match_keyword("true") {
                return Ok(Expr::Literal(Value::Bool(true)));
            }
            if self.match_keyword("false") {
                return Ok(Expr::Literal(Value::Bool(false)));
            }
            if self.match_keyword("null") {
                return Ok(Expr::Literal(Value::Null));
            }
            return self.parse_identifier_or_call();
        }

        Err(ParseError::UnexpectedToken(c.to_string(), self.pos))
    }

    fn parse_string(&mut self) -> Result<Expr, ParseError> {
        let start = self.pos;
        debug_assert_eq!(self.peek_char(), Some('"'));
        self.pos += 1;
        let mut out = String::new();
        loop {
            let Some(c) = self.peek_char() else {
                return Err(ParseError::UnterminatedString(start));
            };
            if c == '"' {
                self.pos += 1;
                return Ok(Expr::Literal(Value::String(out)));
            }
            if c == '\\' {
                self.pos += 1;
                let Some(esc) = self.peek_char() else {
                    return Err(ParseError::UnterminatedString(start));
                };
                let mapped = match esc {
                    'n' => '\n',
                    't' => '\t',
                    '\\' => '\\',
                    '"' => '"',
                    other => other,
                };
                out.push(mapped);
                self.pos += esc.len_utf8();
                continue;
            }
            out.push(c);
            self.pos += c.len_utf8();
        }
    }

    fn parse_number(&mut self) -> Result<Expr, ParseError> {
        let start = self.pos;
        if self.peek_char() == Some('-') {
            self.pos += 1;
        }
        let int_start = self.pos;
        while let Some(c) = self.peek_char() {
            if c.is_ascii_digit() {
                self.pos += 1;
            } else {
                break;
            }
        }
        if self.pos == int_start {
            return Err(ParseError::UnexpectedToken(
                self.src[start..self.pos].to_string(),
                start,
            ));
        }
        let mut is_float = false;
        if self.peek_char() == Some('.') {
            is_float = true;
            self.pos += 1;
            while let Some(c) = self.peek_char() {
                if c.is_ascii_digit() {
                    self.pos += 1;
                } else {
                    break;
                }
            }
        }
        let s = &self.src[start..self.pos];
        if is_float {
            let v: f64 = s
                .parse()
                .map_err(|_| ParseError::UnexpectedToken(s.to_string(), start))?;
            Ok(Expr::Literal(Value::Float(v)))
        } else {
            let v: i64 = s
                .parse()
                .map_err(|_| ParseError::UnexpectedToken(s.to_string(), start))?;
            Ok(Expr::Literal(Value::Int(v)))
        }
    }

    fn parse_identifier_or_call(&mut self) -> Result<Expr, ParseError> {
        // Dotted identifier: `subject.id`, `metadata.po_id`, etc.
        let mut parts: Vec<String> = Vec::new();
        loop {
            let start = self.pos;
            while let Some(c) = self.peek_char() {
                if c.is_alphanumeric() || c == '_' || c == '-' {
                    self.pos += c.len_utf8();
                } else {
                    break;
                }
            }
            if self.pos == start {
                return Err(ParseError::UnexpectedToken(
                    self.src[self.pos..].chars().take(8).collect(),
                    self.pos,
                ));
            }
            parts.push(self.src[start..self.pos].to_string());
            if self.peek_char() == Some('.') {
                self.pos += 1;
            } else {
                break;
            }
        }

        // Function-call form: `name(args)` — only when there's no dot.
        if parts.len() == 1 && self.peek_char() == Some('(') {
            self.pos += 1;
            let mut args = Vec::new();
            self.skip_whitespace();
            if self.peek_char() != Some(')') {
                loop {
                    let arg = self.parse_or()?;
                    args.push(arg);
                    self.skip_whitespace();
                    if self.match_punct(",") {
                        continue;
                    }
                    break;
                }
            }
            self.skip_whitespace();
            if !self.match_punct(")") {
                return Err(ParseError::Expected {
                    expected: "')'",
                    found: self.src[self.pos..].chars().take(8).collect(),
                    pos: self.pos,
                });
            }
            return Ok(Expr::FunctionCall(parts.into_iter().next().unwrap(), args));
        }

        Ok(Expr::Identifier(parts))
    }
}

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

pub fn eval(expr: &Expr, ctx: &Context<'_>) -> Result<Value, EvalError> {
    match expr {
        Expr::Literal(v) => Ok(v.clone()),
        Expr::Identifier(path) => resolve_identifier(path, ctx.payload),
        Expr::FunctionCall(name, args) => {
            let vals: Result<Vec<Value>, _> = args.iter().map(|a| eval(a, ctx)).collect();
            ctx.helpers.call(name, &vals?)
        }
        Expr::UnaryOp(UnaryOp::Not, inner) => {
            let v = eval(inner, ctx)?;
            let b = v.as_bool().ok_or(EvalError::TypeError {
                expected: "bool",
                got: v.kind(),
            })?;
            Ok(Value::Bool(!b))
        }
        Expr::BinaryOp(op, lhs, rhs) => {
            let l = eval(lhs, ctx)?;
            let r = eval(rhs, ctx)?;
            eval_binop(*op, &l, &r)
        }
    }
}

fn resolve_identifier(path: &[String], payload: &serde_json::Value) -> Result<Value, EvalError> {
    // Resolution never raises. A missing key, a traversal into a
    // non-object, or a present array/object (no scalar Value) all
    // resolve to `Absent`, which the operators read as false. Before
    // 7b756357 each of these was an `UnknownIdentifier` that a step's
    // `ready_when` could not survive — the step held pending forever and
    // the dispatcher dead-lettered its side effect.
    let mut cur = payload;
    for segment in path {
        match cur {
            serde_json::Value::Object(map) => match map.get(segment) {
                Some(v) => cur = v,
                None => return Ok(Value::Absent),
            },
            _ => return Ok(Value::Absent),
        }
    }
    Ok(json_to_value(cur).unwrap_or(Value::Absent))
}

fn json_to_value(v: &serde_json::Value) -> Option<Value> {
    match v {
        serde_json::Value::Null => Some(Value::Null),
        serde_json::Value::Bool(b) => Some(Value::Bool(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(Value::Int(i))
            } else {
                n.as_f64().map(Value::Float)
            }
        }
        serde_json::Value::String(s) => Some(Value::String(s.clone())),
        // Arrays + objects don't have a native Value type. They round-trip
        // only via helper-function arguments (which the helper unpacks
        // however it likes); they can't be compared directly.
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => None,
    }
}

fn eval_binop(op: BinaryOp, l: &Value, r: &Value) -> Result<Value, EvalError> {
    match op {
        BinaryOp::And | BinaryOp::Or => {
            let lb = l.as_bool().ok_or(EvalError::TypeError {
                expected: "bool",
                got: l.kind(),
            })?;
            let rb = r.as_bool().ok_or(EvalError::TypeError {
                expected: "bool",
                got: r.kind(),
            })?;
            Ok(Value::Bool(if op == BinaryOp::And {
                lb && rb
            } else {
                lb || rb
            }))
        }
        BinaryOp::Eq => Ok(Value::Bool(values_equal(l, r))),
        BinaryOp::Neq => Ok(Value::Bool(!values_equal(l, r))),
        BinaryOp::Lt | BinaryOp::Lte | BinaryOp::Gt | BinaryOp::Gte => {
            // An absent operand orders against nothing: every inequality
            // over it is false, the same way it compares unequal.
            if matches!(l, Value::Absent) || matches!(r, Value::Absent) {
                return Ok(Value::Bool(false));
            }
            let ord = compare_values(l, r)?;
            let result = match op {
                BinaryOp::Lt => ord.is_lt(),
                BinaryOp::Lte => ord.is_le(),
                BinaryOp::Gt => ord.is_gt(),
                BinaryOp::Gte => ord.is_ge(),
                _ => unreachable!(),
            };
            Ok(Value::Bool(result))
        }
    }
}

fn values_equal(l: &Value, r: &Value) -> bool {
    use Value::*;
    match (l, r) {
        (Null, Null) => true,
        (Bool(a), Bool(b)) => a == b,
        (Int(a), Int(b)) => a == b,
        (Float(a), Float(b)) => a == b,
        (Int(a), Float(b)) | (Float(b), Int(a)) => (*a as f64) == *b,
        (String(a), String(b)) => a == b,
        _ => false,
    }
}

fn compare_values(l: &Value, r: &Value) -> Result<std::cmp::Ordering, EvalError> {
    use Value::*;
    match (l, r) {
        (Int(a), Int(b)) => Ok(a.cmp(b)),
        (Float(a), Float(b)) => a.partial_cmp(b).ok_or(EvalError::TypeError {
            expected: "comparable float",
            got: "NaN",
        }),
        (Int(a), Float(b)) => (*a as f64).partial_cmp(b).ok_or(EvalError::TypeError {
            expected: "comparable float",
            got: "NaN",
        }),
        (Float(a), Int(b)) => a.partial_cmp(&(*b as f64)).ok_or(EvalError::TypeError {
            expected: "comparable float",
            got: "NaN",
        }),
        (String(a), String(b)) => Ok(a.cmp(b)),
        (a, b) => Err(EvalError::TypeError {
            expected: "comparable pair",
            got: match (a.kind(), b.kind()) {
                (x, y) if x == y => x,
                _ => "mismatched kinds",
            },
        }),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx<'a>(payload: &'a serde_json::Value) -> Context<'a> {
        Context {
            payload,
            helpers: &NoHelpers,
        }
    }

    // ----- parser -----

    #[test]
    fn parse_string_literal() {
        let e = parse("\"hello\"").unwrap();
        assert_eq!(e, Expr::Literal(Value::String("hello".into())));
    }

    #[test]
    fn parse_int_literal() {
        assert_eq!(parse("42").unwrap(), Expr::Literal(Value::Int(42)));
        assert_eq!(parse("-7").unwrap(), Expr::Literal(Value::Int(-7)));
    }

    #[test]
    fn parse_float_literal() {
        assert_eq!(parse("2.5").unwrap(), Expr::Literal(Value::Float(2.5)));
    }

    #[test]
    fn parse_bool_and_null() {
        assert_eq!(parse("true").unwrap(), Expr::Literal(Value::Bool(true)));
        assert_eq!(parse("false").unwrap(), Expr::Literal(Value::Bool(false)));
        assert_eq!(parse("null").unwrap(), Expr::Literal(Value::Null));
    }

    #[test]
    fn parse_identifier_dotted() {
        assert_eq!(
            parse("subject.id").unwrap(),
            Expr::Identifier(vec!["subject".into(), "id".into()])
        );
    }

    #[test]
    fn parse_function_call_zero_args() {
        assert_eq!(
            parse("now()").unwrap(),
            Expr::FunctionCall("now".into(), vec![])
        );
    }

    #[test]
    fn parse_function_call_with_args() {
        assert_eq!(
            parse("vendor_for(part_sku)").unwrap(),
            Expr::FunctionCall(
                "vendor_for".into(),
                vec![Expr::Identifier(vec!["part_sku".into()])]
            )
        );
    }

    #[test]
    fn parse_comparison() {
        assert_eq!(
            parse("on_hand <= reorder_point").unwrap(),
            Expr::BinaryOp(
                BinaryOp::Lte,
                Box::new(Expr::Identifier(vec!["on_hand".into()])),
                Box::new(Expr::Identifier(vec!["reorder_point".into()]))
            )
        );
    }

    #[test]
    fn parse_and_or_precedence() {
        // a AND b OR c parses as (a AND b) OR c.
        let e = parse("a AND b OR c").unwrap();
        match e {
            Expr::BinaryOp(BinaryOp::Or, lhs, rhs) => {
                assert!(matches!(*lhs, Expr::BinaryOp(BinaryOp::And, _, _)));
                assert!(matches!(*rhs, Expr::Identifier(_)));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parse_not() {
        assert_eq!(
            parse("NOT open_po_exists(part_sku)").unwrap(),
            Expr::UnaryOp(
                UnaryOp::Not,
                Box::new(Expr::FunctionCall(
                    "open_po_exists".into(),
                    vec![Expr::Identifier(vec!["part_sku".into()])]
                ))
            )
        );
    }

    #[test]
    fn parse_parens() {
        // (a OR b) AND c — without parens this would be a OR (b AND c).
        let e = parse("(a OR b) AND c").unwrap();
        match e {
            Expr::BinaryOp(BinaryOp::And, lhs, rhs) => {
                assert!(matches!(*lhs, Expr::BinaryOp(BinaryOp::Or, _, _)));
                assert!(matches!(*rhs, Expr::Identifier(_)));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parse_canonical_reorder_predicate() {
        // The example from the design doc.
        let e = parse("on_hand <= reorder_point AND NOT open_po_exists(part_sku)").unwrap();
        match e {
            Expr::BinaryOp(BinaryOp::And, lhs, rhs) => {
                assert!(matches!(*lhs, Expr::BinaryOp(BinaryOp::Lte, _, _)));
                assert!(matches!(*rhs, Expr::UnaryOp(UnaryOp::Not, _)));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parse_rejects_trailing_garbage() {
        assert!(matches!(
            parse("a AND b junk"),
            Err(ParseError::UnexpectedToken(_, _))
        ));
    }

    #[test]
    fn parse_rejects_unterminated_string() {
        assert!(matches!(
            parse("\"oops"),
            Err(ParseError::UnterminatedString(_))
        ));
    }

    // ----- evaluator -----

    #[test]
    fn eval_literal_passthrough() {
        let payload = json!({});
        let c = ctx(&payload);
        assert_eq!(eval(&parse("42").unwrap(), &c).unwrap(), Value::Int(42));
        assert_eq!(
            eval(&parse("\"hi\"").unwrap(), &c).unwrap(),
            Value::String("hi".into())
        );
    }

    #[test]
    fn eval_identifier_lookup() {
        let payload = json!({ "part_sku": "PKG-CO2-50LB", "on_hand": 12 });
        let c = ctx(&payload);
        assert_eq!(
            eval(&parse("part_sku").unwrap(), &c).unwrap(),
            Value::String("PKG-CO2-50LB".into())
        );
        assert_eq!(
            eval(&parse("on_hand").unwrap(), &c).unwrap(),
            Value::Int(12)
        );
    }

    #[test]
    fn eval_dotted_identifier_walks_payload() {
        let payload = json!({ "subject": { "id": "vnd-001" } });
        let c = ctx(&payload);
        assert_eq!(
            eval(&parse("subject.id").unwrap(), &c).unwrap(),
            Value::String("vnd-001".into())
        );
    }

    // 7b756357: a missing identifier is Absent, not an error — and
    // Absent is false against every literal and in boolean position, so
    // a predicate over optional metadata evaluates instead of pinning
    // its step pending forever.
    #[test]
    fn a_missing_identifier_resolves_to_absent() {
        let payload = json!({});
        let c = ctx(&payload);
        assert_eq!(eval(&parse("nope").unwrap(), &c), Ok(Value::Absent));
    }

    #[test]
    fn absent_is_false_against_every_literal() {
        let payload = json!({ "metadata": {} });
        let c = ctx(&payload);
        // The idiom the fix exists for.
        assert_eq!(
            eval(&parse("metadata.x = \"true\"").unwrap(), &c),
            Ok(Value::Bool(false))
        );
        assert_eq!(
            eval(&parse("metadata.x != \"true\"").unwrap(), &c),
            Ok(Value::Bool(true))
        );
        assert_eq!(
            eval(&parse("metadata.n = 5").unwrap(), &c),
            Ok(Value::Bool(false))
        );
    }

    #[test]
    fn absent_orders_against_nothing() {
        let payload = json!({});
        let c = ctx(&payload);
        for src in ["n < 5", "n <= 5", "n > 5", "n >= 5"] {
            assert_eq!(
                eval(&parse(src).unwrap(), &c),
                Ok(Value::Bool(false)),
                "{src}"
            );
        }
    }

    #[test]
    fn absent_is_false_in_boolean_position() {
        let payload = json!({ "ready": true });
        let c = ctx(&payload);
        // A bare optional flag: absent reads false, so NOT flag is true —
        // the optional-flag-defaults-off behavior the design wants.
        assert_eq!(eval(&parse("missing_flag").unwrap(), &c), Ok(Value::Absent));
        assert_eq!(
            eval(&parse("NOT missing_flag").unwrap(), &c),
            Ok(Value::Bool(true))
        );
        assert_eq!(
            eval(&parse("ready AND missing_flag").unwrap(), &c),
            Ok(Value::Bool(false))
        );
        assert_eq!(
            eval(&parse("ready OR missing_flag").unwrap(), &c),
            Ok(Value::Bool(true))
        );
    }

    #[test]
    fn a_present_array_or_object_is_absent_not_a_crash() {
        // A path that lands on an array/object has no scalar Value; a
        // predicate over it is false rather than an error.
        let payload = json!({ "questions": [1, 2], "obj": { "a": 1 } });
        let c = ctx(&payload);
        assert_eq!(eval(&parse("questions").unwrap(), &c), Ok(Value::Absent));
        assert_eq!(
            eval(&parse("obj = \"x\"").unwrap(), &c),
            Ok(Value::Bool(false))
        );
    }

    #[test]
    fn a_present_scalar_still_resolves_normally() {
        let payload = json!({ "metadata": { "x": "true" } });
        let c = ctx(&payload);
        assert_eq!(
            eval(&parse("metadata.x = \"true\"").unwrap(), &c),
            Ok(Value::Bool(true))
        );
    }

    #[test]
    fn eval_comparison_int() {
        let payload = json!({ "on_hand": 5, "reorder_point": 20 });
        let c = ctx(&payload);
        assert_eq!(
            eval(&parse("on_hand <= reorder_point").unwrap(), &c).unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            eval(&parse("on_hand > reorder_point").unwrap(), &c).unwrap(),
            Value::Bool(false)
        );
    }

    #[test]
    fn eval_comparison_string() {
        let payload = json!({ "a": "alpha", "b": "beta" });
        let c = ctx(&payload);
        assert_eq!(
            eval(&parse("a < b").unwrap(), &c).unwrap(),
            Value::Bool(true)
        );
    }

    #[test]
    fn eval_int_float_equality() {
        let payload = json!({ "a": 1, "b": 1.0 });
        let c = ctx(&payload);
        assert_eq!(
            eval(&parse("a = b").unwrap(), &c).unwrap(),
            Value::Bool(true)
        );
    }

    #[test]
    fn eval_and_or_not() {
        let payload = json!({ "a": true, "b": false });
        let c = ctx(&payload);
        assert_eq!(
            eval(&parse("a AND b").unwrap(), &c).unwrap(),
            Value::Bool(false)
        );
        assert_eq!(
            eval(&parse("a OR b").unwrap(), &c).unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            eval(&parse("NOT b").unwrap(), &c).unwrap(),
            Value::Bool(true)
        );
    }

    #[test]
    fn eval_and_requires_bools() {
        let payload = json!({ "a": "yes", "b": true });
        let c = ctx(&payload);
        assert!(matches!(
            eval(&parse("a AND b").unwrap(), &c),
            Err(EvalError::TypeError { .. })
        ));
    }

    #[test]
    fn eval_comparison_type_mismatch() {
        let payload = json!({ "a": "str", "b": 1 });
        let c = ctx(&payload);
        assert!(matches!(
            eval(&parse("a < b").unwrap(), &c),
            Err(EvalError::TypeError { .. })
        ));
    }

    // ----- helper-function dispatch -----

    struct MockHelpers;

    impl HelperResolver for MockHelpers {
        fn call(&self, name: &str, args: &[Value]) -> Result<Value, EvalError> {
            match name {
                "open_po_exists" => match args.first() {
                    Some(Value::String(sku)) => Ok(Value::Bool(sku == "PKG-OLD-001")),
                    _ => Err(EvalError::TypeError {
                        expected: "string sku",
                        got: "other",
                    }),
                },
                "vendor_for" => match args.first() {
                    Some(Value::String(sku)) => Ok(Value::String(format!("vnd-for-{sku}"))),
                    _ => Err(EvalError::TypeError {
                        expected: "string sku",
                        got: "other",
                    }),
                },
                _ => Err(EvalError::UnknownHelper(name.to_string())),
            }
        }
    }

    fn ctx_with_helpers<'a>(payload: &'a serde_json::Value) -> Context<'a> {
        Context {
            payload,
            helpers: &MockHelpers,
        }
    }

    #[test]
    fn eval_helper_function() {
        let payload = json!({ "part_sku": "PKG-CO2-50LB" });
        let c = ctx_with_helpers(&payload);
        assert_eq!(
            eval(&parse("vendor_for(part_sku)").unwrap(), &c).unwrap(),
            Value::String("vnd-for-PKG-CO2-50LB".into())
        );
    }

    #[test]
    fn eval_unknown_helper_errors() {
        let payload = json!({});
        let c = ctx_with_helpers(&payload);
        assert!(matches!(
            eval(&parse("mystery()").unwrap(), &c),
            Err(EvalError::UnknownHelper(_))
        ));
    }

    #[test]
    fn eval_canonical_reorder_predicate_true_when_thresholds_low() {
        // The signature example end-to-end:
        // on_hand <= reorder_point AND NOT open_po_exists(part_sku)
        let payload = json!({
            "part_sku": "PKG-CO2-50LB",
            "on_hand": 10,
            "reorder_point": 20,
        });
        let c = ctx_with_helpers(&payload);
        let e = parse("on_hand <= reorder_point AND NOT open_po_exists(part_sku)").unwrap();
        assert_eq!(eval(&e, &c).unwrap(), Value::Bool(true));
    }

    #[test]
    fn eval_canonical_reorder_predicate_false_when_po_exists() {
        // open_po_exists returns true for "PKG-OLD-001" in the mock,
        // so the canonical rule must NOT spawn a duplicate restock.
        let payload = json!({
            "part_sku": "PKG-OLD-001",
            "on_hand": 10,
            "reorder_point": 20,
        });
        let c = ctx_with_helpers(&payload);
        let e = parse("on_hand <= reorder_point AND NOT open_po_exists(part_sku)").unwrap();
        assert_eq!(eval(&e, &c).unwrap(), Value::Bool(false));
    }

    #[test]
    fn eval_string_literal_as_arg_to_handler() {
        // The D2 "literal strings are expressions that evaluate to themselves"
        // case — a rule's args = { kind = "ingredient-restock" } parses + evals
        // to the constant string.
        let payload = json!({});
        let c = ctx_with_helpers(&payload);
        assert_eq!(
            eval(&parse("\"ingredient-restock\"").unwrap(), &c).unwrap(),
            Value::String("ingredient-restock".into())
        );
    }

    #[test]
    fn references_collects_every_identifier_path() {
        // A realistic ready_when fork predicate: two step refs, one
        // subject ref, joined by boolean operators.
        let expr = parse(
            "steps.triage.done AND (steps.triage.metadata.outcome = \"repairable\" \
             OR subject.warranty)",
        )
        .unwrap();
        let refs = references(&expr);
        assert!(refs.contains(&vec!["steps".into(), "triage".into(), "done".into()]));
        assert!(refs.contains(&vec![
            "steps".into(),
            "triage".into(),
            "metadata".into(),
            "outcome".into()
        ]));
        assert!(refs.contains(&vec!["subject".into(), "warranty".into()]));
        // Literals contribute nothing.
        assert_eq!(references(&parse("true").unwrap()).len(), 0);
    }
}
