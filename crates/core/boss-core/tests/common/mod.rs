//! Shared scaffolding for the publisher's log-asserting tests.
//!
//! Three binaries read the same publish path from different angles —
//! an audit write that fails, a bus publish that fails, a batch where
//! both fail — and each asserts what the publisher LOGGED, so each is
//! a binary of its own with exactly one test in it. `tracing` keeps
//! its callsite-interest cache and dynamic max level in process-global
//! state, while `tracing::subscriber::set_default` installs a
//! subscriber for one THREAD; `cargo test` runs a binary's tests on
//! many threads at once, so a sibling test that reaches the same
//! `error!` callsite with no subscriber on its thread can leave it
//! resolved against no subscriber for everyone, and the capturing test
//! reads a log that is missing lines, or empty. Measured 1 run in 25
//! in `boss-jobs` (train #281, 2026-09-09); the same shape stood here
//! in the lib binary's `publish_contract_tests` until backlog 2c257761
//! (2026-09-18). `boss-testing/tests/a_log_capturing_test_owns_its_
//! process.rs` refuses it coming back.
//!
//! Provides:
//! - `StubBus` / `StubAudit` — a bus and an audit writer that succeed
//!   or fail on demand and count their calls
//! - `event()` — the one event every test publishes
//! - `Captured` — a `MakeWriter` that collects what
//!   `tracing_subscriber::fmt` prints, so a test can read the log the
//!   way an operator would; each binary installs it with
//!   `set_global_default` itself, so the file that owns the process is
//!   the file that claims the subscriber

#![allow(dead_code)]

use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use boss_core::audit::AuditWriter;
use boss_core::event::Event;
use boss_core::port::{EventBus, EventBusError, EventStream};

pub struct StubBus {
    pub ok: bool,
    pub published: AtomicUsize,
}

impl StubBus {
    pub fn new(ok: bool) -> Arc<Self> {
        Arc::new(Self {
            ok,
            published: AtomicUsize::new(0),
        })
    }
}

#[async_trait::async_trait]
impl EventBus for StubBus {
    async fn publish(&self, _e: Event) -> Result<(), EventBusError> {
        self.published.fetch_add(1, Ordering::SeqCst);
        if self.ok {
            Ok(())
        } else {
            Err(EventBusError::PublishFailed("bus down".into()))
        }
    }
    async fn subscribe(&self, _p: &str) -> Result<Box<dyn EventStream>, EventBusError> {
        Err(EventBusError::SubscribeFailed("stub".into()))
    }
}

pub struct StubAudit {
    pub ok: bool,
    pub writes: AtomicUsize,
}

impl StubAudit {
    pub fn new(ok: bool) -> Arc<Self> {
        Arc::new(Self {
            ok,
            writes: AtomicUsize::new(0),
        })
    }
}

#[async_trait::async_trait]
impl AuditWriter for StubAudit {
    async fn write(&self, _e: &Event) -> Result<(), String> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        if self.ok {
            Ok(())
        } else {
            Err("insert rejected by audit_log_check_refs".into())
        }
    }
}

pub fn event() -> Event {
    Event::new(
        "test-svc",
        "commerce.invoice.created",
        serde_json::json!({"id": "inv-1"}),
        chrono::Utc::now(),
    )
}

#[derive(Clone, Default)]
pub struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Captured;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}
