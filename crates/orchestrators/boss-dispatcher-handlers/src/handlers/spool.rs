//! The estate spool, Rust half — keep a reading the system of record
//! could not take, and replay it once the record can.
//!
//! WHY. CLAUDE.md §Diagnosis: *an alarm that reports through its
//! subject dies with it*. The estate loop observes, compares and files
//! entirely through the jobs API, so during a system-of-record outage
//! it is silent about the outage — which is exactly when it is needed
//! (packet 6bf34846). The host observer's half of the answer already
//! exists and is pinned on every gate: `infra/estate/observe-lib.sh`
//! spools a reading the API refused and replays it, oldest first, when
//! the API answers again (packet ec50db46). This is the same mechanism
//! for the Rust stage that runs downstream of it.
//!
//! It is deliberately the SAME mechanism, not a new one: same spool
//! directory, same cap, same `SPOOL_DIR` / `SPOOL_MAX` knobs, same
//! oldest-first-and-stop-at-the-first-failure replay, same one-file-per
//! -reading-named-by-`observed_at` layout. An operator has one place to
//! look and one cap to reason about, and the two halves cannot disagree
//! about either: [`SPOOL_DIR_DEFAULT`] and [`SPOOL_MAX_DEFAULT`] are
//! DERIVED from the shell library by `build.rs` rather than restated
//! here (CLAUDE.md §9a — prefer collapsing).
//!
//! WHAT IS RETAINED IS THE INPUT, NOT THE OUTPUT. A stage spools the
//! reading it was handed, and replay re-runs the stage over it. That
//! matters because during an outage the stage's own *reads* fail too
//! (`estate.compare` cannot fetch the declared nodes it compares
//! against), so there is no output yet to keep. Re-running on replay
//! also lets the comparison be made against the registry as it is when
//! the record comes back, which is the only version that can be read.
//!
//! THE GAP STAYS VISIBLE. A replayed reading carries its ORIGINAL
//! `observed_at` — the stage stamps it from the reading, never from the
//! clock — so the series afterwards shows readings that arrived late,
//! not readings that never were.
//!
//! HONEST LIMIT, stated because it decides what this does and does not
//! buy. The spool is a directory on the process's own filesystem, and
//! the dispatcher shares a container with the jobs API
//! (`infra/oss-quickstart/services-launcher.sh`, one pod under
//! `Recreate`). So it covers the outage class where the record is
//! REACHABLE AND WRONG — 500s, a saturated database, a wedged jobs
//! process answering errors — which is the 2026-08-27 (25 minutes of
//! 500s) and 2026-09-02 shape. It does not survive the container
//! itself restarting; that class is already covered, because the
//! triggering event is unacked and JetStream redelivers it to the
//! fresh process (`boss-nats::durable`). What neither covers is an
//! outage longer than the redelivery budget — `MAX_DELIVER` 8 across a
//! ~98-second backoff — with the process alive, and that is the hole
//! this closes.
//!
//! An independent REPORT path — telling somebody about the outage over
//! a transport that does not run through the thing that is down — is
//! the packet's other half and is deliberately not built here.

use std::path::{Path, PathBuf};
use std::pin::Pin;

use serde_json::Value as Json;

include!(concat!(env!("OUT_DIR"), "/estate_spool_defaults.rs"));

/// A future produced by a replay's post function. Boxed and `'static`
/// so a caller can hand in a closure that owns its own HTTP client.
pub type PostFuture = Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>;

/// What a replay did. Never an error on its own — a replay that could
/// not drain is a fact about the record, and the caller decides what
/// that means for its firing.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Replayed {
    /// Readings the record took this pass.
    pub posted: usize,
    /// Readings still waiting afterwards.
    pub waiting: usize,
    /// Why the replay stopped, if it stopped early.
    pub stopped: Option<String>,
}

/// One stage's spool: a directory of retained readings, capped.
///
/// One file per reading, named by the reading's own `observed_at`, so
/// the directory sorts into the order the readings were taken — the
/// layout `observe-lib.sh` uses, for the same reason.
#[derive(Debug, Clone)]
pub struct Spool {
    dir: PathBuf,
    max: usize,
}

impl Spool {
    /// The spool for a named stage, under `SPOOL_DIR` (default
    /// [`SPOOL_DIR_DEFAULT`]) and capped at `SPOOL_MAX` (default
    /// [`SPOOL_MAX_DEFAULT`]).
    ///
    /// Each stage gets its own subdirectory: the cap is then a
    /// per-stage promise, and a stage that has been down for a day
    /// cannot evict a neighbour's readings.
    pub fn for_stage(stage: &str) -> Self {
        let base = std::env::var("SPOOL_DIR").unwrap_or_else(|_| SPOOL_DIR_DEFAULT.to_string());
        let max = std::env::var("SPOOL_MAX")
            .ok()
            .and_then(|m| m.parse::<usize>().ok())
            .filter(|m| *m > 0)
            .unwrap_or(SPOOL_MAX_DEFAULT);
        Self::at(Path::new(&base).join(safe_name(stage)), max)
    }

    /// An explicit spool. The constructor tests use, and the one to
    /// reach for when a caller already knows where it wants to keep
    /// things.
    pub fn at(dir: impl Into<PathBuf>, max: usize) -> Self {
        Self {
            dir: dir.into(),
            max,
        }
    }

    /// Where the readings wait — for the log line that tells an
    /// operator where to look.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// How many readings are waiting.
    pub fn waiting(&self) -> usize {
        self.entries().len()
    }

    /// Keep a reading the record could not take.
    ///
    /// The file name is the reading's own `observed_at`, so a
    /// redelivery of the same reading overwrites rather than
    /// duplicates, and the directory sorts oldest-first. Past the cap
    /// the OLDEST is dropped: a long outage must not fill a disk that
    /// is very possibly the thing being observed, and the newest
    /// reading is the one that still describes now.
    pub fn put(&self, observed_at: &str, reading: &Json) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let name = format!("{}.json", safe_name(observed_at));
        std::fs::write(self.dir.join(name), serde_json::to_vec(reading)?)?;
        // The cap, applied after the write so the newest reading is
        // never the one refused.
        let mut entries = self.entries();
        while entries.len() > self.max {
            let oldest = entries.remove(0);
            let _ = std::fs::remove_file(&oldest);
            tracing::warn!(
                spool = %self.dir.display(),
                dropped = %oldest.display(),
                cap = self.max,
                "estate spool over its cap — dropped the oldest retained reading"
            );
        }
        Ok(())
    }

    /// Replay every waiting reading, oldest first.
    ///
    /// A reading the record takes is deleted; the FIRST failure stops
    /// the replay with everything still waiting kept. Stopping matters:
    /// a record that just refused one reading will refuse the next
    /// twenty, and draining into a dead API would spend the whole spool
    /// on one bad pass.
    pub async fn replay<F>(&self, mut post: F) -> Replayed
    where
        F: FnMut(Json) -> PostFuture,
    {
        let entries = self.entries();
        if entries.is_empty() {
            return Replayed::default();
        }
        tracing::info!(
            spool = %self.dir.display(),
            waiting = entries.len(),
            "estate spool: replaying retained readings, oldest first"
        );
        let mut posted = 0usize;
        for (idx, path) in entries.iter().enumerate() {
            let reading = match std::fs::read(path)
                .ok()
                .and_then(|b| serde_json::from_slice::<Json>(&b).ok())
            {
                Some(r) => r,
                None => {
                    // Unreadable: replay cannot help, and keeping it
                    // would block every reading behind it forever.
                    tracing::error!(
                        file = %path.display(),
                        "estate spool: retained reading is unreadable — discarding it rather than blocking the replay"
                    );
                    let _ = std::fs::remove_file(path);
                    continue;
                }
            };
            match post(reading).await {
                Ok(()) => {
                    let _ = std::fs::remove_file(path);
                    posted += 1;
                }
                Err(e) => {
                    let waiting = entries.len() - idx;
                    tracing::warn!(
                        spool = %self.dir.display(),
                        waiting,
                        error = %e,
                        "estate spool: replay stopped — the rest are kept"
                    );
                    return Replayed {
                        posted,
                        waiting,
                        stopped: Some(e),
                    };
                }
            }
        }
        Replayed {
            posted,
            waiting: 0,
            stopped: None,
        }
    }

    /// Retained readings, oldest first. The name IS the order: it is
    /// the reading's `observed_at`, and an ISO-8601 UTC stamp sorts
    /// lexically the way it sorts chronologically.
    fn entries(&self) -> Vec<PathBuf> {
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut out: Vec<PathBuf> = rd
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "json"))
            .collect();
        out.sort();
        out
    }
}

/// A file name that stays inside the spool. `observed_at` arrives in an
/// event payload, so a `../` in it must not become a path.
fn safe_name(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, ':' | '.' | '+' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('.').to_string();
    if cleaned.is_empty() {
        "unstamped".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    /// A reading, shaped like the observation the compare stage is
    /// handed: what matters here is that it carries `observed_at`.
    fn reading(at: &str) -> Json {
        json!({ "observed_at": at, "scope": "host", "nodes": [{ "id": "forge" }] })
    }

    fn at(j: &Json) -> String {
        j["observed_at"].as_str().unwrap_or_default().to_string()
    }

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "boss-estate-spool-test-{tag}-{}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            let _ = std::fs::remove_dir_all(&p);
            Self(p)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A stand-in for the system of record: answers according to `up`,
    /// and records every reading it was handed, in order.
    #[derive(Clone, Default)]
    struct Record {
        up: Arc<Mutex<bool>>,
        /// Post number after which the record starts refusing; `None`
        /// means it never changes its mind mid-replay.
        fails_after: Arc<Mutex<Option<usize>>>,
        took: Arc<Mutex<Vec<Json>>>,
    }
    impl Record {
        fn up(up: bool) -> Self {
            Self {
                up: Arc::new(Mutex::new(up)),
                fails_after: Arc::new(Mutex::new(None)),
                took: Arc::new(Mutex::new(Vec::new())),
            }
        }
        fn poster(&self) -> impl FnMut(Json) -> PostFuture + use<> {
            let me = self.clone();
            move |reading: Json| {
                let me = me.clone();
                Box::pin(async move {
                    let mut took = me.took.lock().expect("took lock");
                    let limit = *me.fails_after.lock().expect("fails_after lock");
                    let refuse =
                        !*me.up.lock().expect("up lock") || limit.is_some_and(|n| took.len() >= n);
                    if refuse {
                        return Err("jobs api: 503".to_string());
                    }
                    took.push(reading);
                    Ok(())
                })
            }
        }
        fn took(&self) -> Vec<String> {
            self.took
                .lock()
                .expect("took lock")
                .iter()
                .map(at)
                .collect()
        }
    }

    #[tokio::test]
    async fn a_reading_the_record_could_not_take_is_retained() {
        let tmp = TempDir::new("retained");
        let spool = Spool::at(&tmp.0, 10);
        assert_eq!(spool.waiting(), 0, "a fresh spool holds nothing");

        let r = reading("2026-09-05T08:00:00Z");
        spool.put(&at(&r), &r).expect("put");

        assert_eq!(
            spool.waiting(),
            1,
            "the reading the record refused was dropped instead of retained"
        );
    }

    #[tokio::test]
    async fn replay_drains_oldest_first_and_empties_the_spool() {
        let tmp = TempDir::new("oldest-first");
        let spool = Spool::at(&tmp.0, 10);
        // Put them out of order: the ORDER OF THE READINGS, not the
        // order they were spooled in, is what replay must honour.
        for stamp in [
            "2026-09-05T08:30:00Z",
            "2026-09-05T08:00:00Z",
            "2026-09-05T08:15:00Z",
        ] {
            let r = reading(stamp);
            spool.put(&at(&r), &r).expect("put");
        }

        let record = Record::up(true);
        let out = spool.replay(record.poster()).await;

        assert_eq!(
            record.took(),
            vec![
                "2026-09-05T08:00:00Z",
                "2026-09-05T08:15:00Z",
                "2026-09-05T08:30:00Z"
            ],
            "replay did not go oldest first"
        );
        assert_eq!(out.posted, 3);
        assert_eq!(out.waiting, 0);
        assert_eq!(spool.waiting(), 0, "the spool is not empty after a replay");
    }

    #[tokio::test]
    async fn a_replay_that_fails_partway_keeps_the_rest() {
        let tmp = TempDir::new("partway");
        let spool = Spool::at(&tmp.0, 10);
        for stamp in [
            "2026-09-05T08:00:00Z",
            "2026-09-05T08:15:00Z",
            "2026-09-05T08:30:00Z",
        ] {
            let r = reading(stamp);
            spool.put(&at(&r), &r).expect("put");
        }

        let record = Record::up(true);
        *record.fails_after.lock().expect("lock") = Some(1);
        let out = spool.replay(record.poster()).await;

        assert_eq!(
            record.took(),
            vec!["2026-09-05T08:00:00Z"],
            "replay kept posting past the first failure"
        );
        assert_eq!(out.posted, 1);
        assert_eq!(out.waiting, 2);
        assert!(out.stopped.is_some(), "a stopped replay must say why");
        assert_eq!(
            spool.waiting(),
            2,
            "a failed replay lost the readings it had not posted yet"
        );
    }

    #[tokio::test]
    async fn the_cap_drops_the_oldest_never_the_newest() {
        let tmp = TempDir::new("cap");
        let spool = Spool::at(&tmp.0, 3);
        for stamp in [
            "2026-09-05T08:00:00Z",
            "2026-09-05T08:15:00Z",
            "2026-09-05T08:30:00Z",
            "2026-09-05T08:45:00Z",
        ] {
            let r = reading(stamp);
            spool.put(&at(&r), &r).expect("put");
        }

        assert_eq!(spool.waiting(), 3, "the cap did not hold");
        let record = Record::up(true);
        spool.replay(record.poster()).await;
        assert_eq!(
            record.took(),
            vec![
                "2026-09-05T08:15:00Z",
                "2026-09-05T08:30:00Z",
                "2026-09-05T08:45:00Z"
            ],
            "the cap dropped the wrong end — the newest reading is the one that still describes now"
        );
    }

    #[tokio::test]
    async fn a_replayed_reading_keeps_its_original_observed_at() {
        let tmp = TempDir::new("stamp");
        let spool = Spool::at(&tmp.0, 10);
        let r = reading("2026-09-05T08:00:00Z");
        spool.put(&at(&r), &r).expect("put");

        let record = Record::up(true);
        spool.replay(record.poster()).await;

        let took = record.took.lock().expect("took lock").clone();
        assert_eq!(took.len(), 1);
        assert_eq!(
            took[0]["observed_at"], "2026-09-05T08:00:00Z",
            "a replayed reading lost its original observed_at — the gap now looks like it happened just now"
        );
        assert_eq!(
            took[0], r,
            "a replayed reading must be the reading, unchanged"
        );
    }

    #[tokio::test]
    async fn a_redelivered_reading_overwrites_rather_than_duplicates() {
        // The triggering event is NAK'd and redelivered while the
        // record is down, so the same reading is spooled repeatedly.
        // Named by its own observed_at, it lands on itself.
        let tmp = TempDir::new("redelivery");
        let spool = Spool::at(&tmp.0, 10);
        let r = reading("2026-09-05T08:00:00Z");
        for _ in 0..8 {
            spool.put(&at(&r), &r).expect("put");
        }
        assert_eq!(
            spool.waiting(),
            1,
            "eight redeliveries of one reading became eight retained readings"
        );
    }

    #[test]
    fn a_stamp_from_an_event_payload_cannot_escape_the_spool() {
        let tmp = TempDir::new("traversal");
        let spool = Spool::at(&tmp.0, 10);
        let r = reading("../../etc/passwd");
        spool.put(&at(&r), &r).expect("put");
        assert_eq!(spool.waiting(), 1);
        assert!(
            std::fs::read_dir(&tmp.0)
                .expect("spool dir")
                .filter_map(Result::ok)
                .all(|e| e.path().parent() == Some(tmp.0.as_path())),
            "a spooled reading escaped its directory"
        );
    }

    /// CLAUDE.md §9a. The spool directory and its cap are defined ONCE,
    /// in the shell observer's library, and derived here by `build.rs`.
    /// This is not an equality test between two copies — there is one
    /// copy — it pins that the derivation still finds it, so a rename
    /// in the shell file fails here with a name rather than silently
    /// sending the two halves of one mechanism to different places.
    #[test]
    fn the_defaults_are_the_shell_observers_own() {
        assert_eq!(
            SPOOL_DIR_DEFAULT, "/var/tmp/boss-estate-spool",
            "derived from infra/estate/observe-lib.sh; if that file moved the spool deliberately, update this expectation"
        );
        assert_eq!(
            SPOOL_MAX_DEFAULT, 200,
            "derived from infra/estate/observe-lib.sh SPOOL_MAX"
        );
    }

    #[test]
    fn a_stage_gets_its_own_subdirectory_under_the_one_spool() {
        // Not a shared bag: a stage down for a day must not evict a
        // neighbour's readings through the shared cap.
        let s = Spool::for_stage("estate.compare");
        assert!(
            s.dir().starts_with(SPOOL_DIR_DEFAULT)
                || std::env::var("SPOOL_DIR").is_ok_and(|d| s.dir().starts_with(d)),
            "a stage spool must live under the one spool directory, got {}",
            s.dir().display()
        );
        assert!(s.dir().ends_with("estate.compare"));
    }
}
