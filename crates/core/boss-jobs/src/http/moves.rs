//! `GET /api/yard/moves` and `GET /api/yard/moves/stream` — the moves
//! record served, and the loop that writes it (design e765b3fc §3; car
//! M1 on feedback 84cba7e2). The record itself — what a move is, the
//! diff, the fold — is [`crate::moves`], pure and unit-tested; this is
//! the adapter: the mover loop that reads the log and the map, and the
//! two reads that serve what it wrote.
//!
//! ONE MOVER PER REPLICA, NOT PER VIEWER. `/api/events/stream` runs one
//! poll of the log per viewer and hands raw payloads to the page, which
//! would then have to place packets itself — a second placement function
//! in TypeScript, the defect CLAUDE.md §9a names. Here one loop reads the
//! log's head once a second, places packets through the partition the
//! regions read is pinned to, writes the moves, and publishes each
//! recorded row to every open stream on this replica. A viewer costs a
//! channel subscription.
//!
//! THE STREAM RESUMES FROM THE RECORD. A viewer that reconnects with
//! `Last-Event-ID` (or `?since=`) is sent the rows after it from
//! `yard_moves` — a range read, not a replay — and one that connects
//! fresh, falls behind, or asks past what a backfill holds is sent a
//! `resync`: take the current placement, nothing moved. A restart never
//! animates a flood that did not happen.
//!
//! ACCESS: a caller who reads every packet. The regions read, which
//! already shows these branches and titles, narrows its rows to a scoped
//! caller's packets; a move row carries no owner to narrow on, so a
//! scoped caller is refused by name rather than served a record that
//! names packets outside its scope — or an empty one that reads as
//! "nothing moved".

use super::*;

use std::convert::Infallible;
use std::time::Duration;

use axum::http::HeaderMap;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};

use crate::moves::{Baseline, Frame, MAX_BATCH, MoverStatus, Reading, Snapshot, Tick};

/// How often the mover reads the log's head.
pub const TICK: Duration = Duration::from_secs(1);

/// THE SETTLE: a batch is judged once the log has been quiet for one
/// tick, or once its oldest event has waited this long. A gate's green
/// closes the gate-run and the dispatcher files the car about a second
/// later; judged between the two, the car's gate-run would read as a
/// stranded green for one reading and the hand-off would be two moves.
/// Waiting out the burst keeps one journey one row, and bounds the map
/// reads under steady traffic to one per this interval.
pub const SETTLE: Duration = Duration::from_secs(5);

/// The page `GET /api/yard/moves` answers when the caller names none,
/// and the most it will answer.
const DEFAULT_PAGE: i64 = 200;
const MAX_PAGE: i64 = 1000;

/// How many recorded rows a reconnecting stream is sent before it is
/// told to resync instead.
const BACKFILL: i64 = 500;

/// The actor the mover reads the map as. It is the service reading its
/// own record, not a person, so no policy scope narrows what it places.
pub const MOVER_ACTOR: &str = "automation:yard-mover";

/// The caller's refusal, or `None` when they may read the record.
async fn refused<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &JobsApiState<R, B>,
    user: &boss_policy_client::User,
) -> Option<Response> {
    match state.policy.scope_predicate(user, Resource::job()).await {
        Err(e) => Some(e.into_response()),
        Ok(p) => match job_scope_from_predicate(user, &p) {
            JobScope::All => None,
            JobScope::None => Some(
                (
                    StatusCode::FORBIDDEN,
                    "the moves record names packets; this caller may read none",
                )
                    .into_response(),
            ),
            _ => Some(
                (
                    StatusCode::FORBIDDEN,
                    "the moves record names every packet that moved on the map; a caller \
                     whose packet scope is narrowed cannot be served it row by row yet",
                )
                    .into_response(),
            ),
        },
    }
}

fn unwired() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the moves record is not wired on this jobs API",
    )
        .into_response()
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct MovesQuery {
    since: Option<i64>,
    limit: Option<i64>,
    window: Option<String>,
}

/// `GET /api/yard/moves?since=<seq>&limit=<n>&window=24h` — the recorded
/// moves after `since`, oldest first; the newest seq; the undrawn routes
/// moves took in the window (the observed-undeclared count); and what
/// this replica's mover is doing.
pub(super) async fn yard_moves<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    axum::extract::Query(q): axum::extract::Query<MovesQuery>,
) -> Response {
    let Some(feed) = state.yard_moves.as_ref() else {
        return unwired();
    };
    if let Some(refusal) = refused(&state, &user).await {
        return refusal;
    }
    let window_hours = match crate::regions::parse_window(q.window.as_deref()) {
        Ok(h) => h,
        Err(why) => return (StatusCode::BAD_REQUEST, why).into_response(),
    };
    let limit = q.limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE);
    let since = boss_clock_client::wall_now() - chrono::Duration::hours(window_hours);
    let read = async {
        Ok::<_, crate::moves::MovesError>((
            feed.store.since(q.since.unwrap_or(0), limit).await?,
            feed.store.latest_seq().await?,
            feed.store.crossings(since).await?,
        ))
    };
    match read.await {
        Ok((moves, latest_seq, crossings)) => {
            // Judged against the routes derived now (car R2). Routes that
            // cannot be derived leave the reading null, never empty.
            let undeclared = super::routes::derived(&state, Some(&crossings))
                .await
                .ok()
                .map(|routes| routes.undeclared());
            Json(serde_json::json!({
                "moves": moves,
                "latest_seq": latest_seq,
                "window_hours": window_hours,
                "undeclared": {
                    "band": crate::region_states::MOVES_UNDECLARED.id,
                    "reads": crate::region_states::MOVES_UNDECLARED.band,
                    "moves": undeclared.as_ref().map(|u| u.iter().map(|r| r.moves).sum::<i64>()),
                    "routes": undeclared,
                },
                "mover": feed.status(),
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("the moves record could not be read: {e}"),
        )
            .into_response(),
    }
}

/// One frame as the stream sends it: a move under its seq, so the
/// browser's `Last-Event-ID` is the record's own position.
fn sse(frame: &Frame) -> SseEvent {
    match frame {
        Frame::Move(r) => SseEvent::default()
            .event("move")
            .id(r.seq.to_string())
            .data(serde_json::to_string(r).unwrap_or_default()),
        Frame::Resync { .. } => SseEvent::default()
            .event("resync")
            .data(serde_json::to_string(frame).unwrap_or_default()),
    }
}

/// `GET /api/yard/moves/stream[?since=<seq>]` (or `Last-Event-ID`) — see
/// the module header.
pub(super) async fn yard_moves_stream<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<MovesQuery>,
) -> Response {
    let Some(feed) = state.yard_moves.clone() else {
        return unwired();
    };
    if let Some(refusal) = refused(&state, &user).await {
        return refusal;
    }
    let resume = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<i64>().ok())
        .or(q.since);
    // Subscribed BEFORE the backfill is read, so a move recorded in
    // between is in the channel; the seq check below drops the overlap.
    let mut frames = feed.subscribe();
    let stream = async_stream::stream! {
        let latest = feed.store.latest_seq().await.unwrap_or(0);
        let mut sent = latest;
        match resume {
            Some(from) if from <= latest => {
                match feed.store.since(from, BACKFILL + 1).await {
                    Ok(rows) if rows.len() as i64 <= BACKFILL => {
                        sent = from;
                        for r in rows {
                            sent = sent.max(r.seq);
                            yield Ok::<_, Infallible>(sse(&Frame::Move(Box::new(r))));
                        }
                    }
                    Ok(_) => yield Ok(sse(&Frame::Resync {
                        seq: latest,
                        reason: format!("more than {BACKFILL} moves since {from}"),
                    })),
                    Err(e) => yield Ok(sse(&Frame::Resync {
                        seq: latest,
                        reason: format!("the record could not be read from {from}: {e}"),
                    })),
                }
            }
            _ => yield Ok(sse(&Frame::Resync {
                seq: latest,
                reason: "connected: the current placement is the baseline".into(),
            })),
        }
        loop {
            match frames.recv().await {
                Ok(Frame::Move(r)) => {
                    if r.seq > sent {
                        sent = r.seq;
                        yield Ok(sse(&Frame::Move(r)));
                    }
                }
                Ok(frame) => yield Ok(sse(&frame)),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    sent = feed.store.latest_seq().await.unwrap_or(sent);
                    yield Ok(sse(&Frame::Resync {
                        seq: sent,
                        reason: format!("this stream fell {n} frames behind"),
                    }));
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Stamp `found` against the routes derived now (car R2) — or leave it
/// unjudged (`declared: null`) when they cannot be derived, rather than
/// stamping a guess into a record that keeps it. Only a batch that
/// holds a move pays for the derivation.
async fn judge<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &JobsApiState<R, B>,
    found: Vec<crate::moves::Move>,
) -> Vec<crate::moves::Move> {
    if found.is_empty() {
        return found;
    }
    match super::routes::derived(state, None).await {
        Ok(routes) => crate::moves::judged(found, &routes),
        Err(_) => found,
    }
}

/// THE MOVER LOOP (design e765b3fc §3): read the log's head every
/// [`TICK`]; when packet-naming events have landed and settled
/// ([`SETTLE`]), read the map, fold the tick into the baseline
/// ([`crate::moves::advance`]), record the moves, and publish every row
/// the record now holds past what this replica last published — its own
/// and any another replica wrote. Runs until `cancel` flips.
///
/// NOTHING HERE IS FATAL. A failed read keeps the baseline and says so
/// on the status `GET /api/yard/moves` serves; the next tick tries
/// again. A loop that stops serving a record must say it has stopped.
pub async fn run_mover<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: Arc<JobsApiState<R, B>>,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) {
    let Some(feed) = state.yard_moves.clone() else {
        return;
    };
    let actor = boss_policy_client::User {
        id: MOVER_ACTOR.into(),
        role: "automation".into(),
        access_tier: boss_policy_client::AccessTier::User,
        territory_account_ids: Vec::new(),
        direct_report_ids: Vec::new(),
        department: None,
    };
    let mut baseline: Option<Baseline> = None;
    let mut published = feed.store.latest_seq().await.unwrap_or(0);
    // The head the last tick saw, and when events first stood unjudged.
    let mut seen: i64 = -1;
    let mut pending_since: Option<std::time::Instant> = None;
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = cancel.changed() => return,
        }
        let status = |state: &str, why: String, cursor: i64| MoverStatus {
            state: state.into(),
            why,
            cursor,
            read_at: Some(boss_clock_client::wall_now()),
        };
        let cursor = baseline.as_ref().map_or(0, |b| b.cursor);
        let head = match feed.store.log_head().await {
            Ok(h) => h,
            Err(e) => {
                feed.set_status(status(
                    "failing",
                    format!("the log's head could not be read: {e}"),
                    cursor,
                ));
                continue;
            }
        };
        let settled = head == seen || pending_since.is_some_and(|t| t.elapsed() >= SETTLE);
        seen = head;
        let due = match &baseline {
            None => true,
            Some(b) if head < b.cursor => true,
            Some(b) if head == b.cursor => false,
            Some(_) => settled,
        };
        if !due {
            if baseline.as_ref().is_some_and(|b| head > b.cursor) {
                pending_since.get_or_insert_with(std::time::Instant::now);
            }
            continue;
        }
        let causes = match &baseline {
            Some(b) if head > b.cursor => {
                match feed.store.causes(b.cursor, head, MAX_BATCH + 1).await {
                    Ok(c) => c,
                    Err(e) => {
                        feed.set_status(status(
                            "failing",
                            format!("the log could not be read after {}: {e}", b.cursor),
                            b.cursor,
                        ));
                        continue;
                    }
                }
            }
            _ => Vec::new(),
        };
        let truncated = causes.len() as i64 > MAX_BATCH;
        // Events that name no packet move nothing: advance the cursor
        // without paying for a reading of the map.
        if let Some(b) = baseline.as_mut()
            && head >= b.cursor
            && causes.is_empty()
        {
            b.cursor = head;
            pending_since = None;
            feed.set_status(status(
                "recording",
                "no packet-naming events since the last reading".into(),
                head,
            ));
            continue;
        }
        let now = boss_clock_client::now_from(&state.clock).await;
        let rows = match super::regions::read_map_as(
            &state,
            &actor,
            JobScope::All,
            now,
            crate::regions::DEFAULT_WINDOW_HOURS,
        )
        .await
        {
            Ok(rows) => rows,
            Err(resp) => {
                feed.set_status(status(
                    "failing",
                    format!("the map could not be read (HTTP {})", resp.status()),
                    cursor,
                ));
                continue;
            }
        };
        let snapshot = Snapshot::of(&rows.inputs(now, crate::regions::DEFAULT_WINDOW_HOURS));
        let unread = snapshot.unread.clone();
        let (next, reading) = crate::moves::advance(
            baseline.as_ref(),
            Tick {
                head,
                causes,
                truncated,
                snapshot,
            },
        );
        let why = match reading {
            Reading::Resync(reason) => {
                feed.publish(Frame::Resync {
                    seq: published,
                    reason: reason.clone(),
                });
                reason
            }
            Reading::Moves(found) => {
                let found = judge(&state, found).await;
                match feed.store.record(&found).await {
                    Ok(added) => format!(
                        "{} move(s) found, {added} new, at log position {head}",
                        found.len()
                    ),
                    Err(e) => {
                        // The baseline does not advance: the same batch is
                        // judged again next tick, and the record's key
                        // dedupes whatever did land.
                        feed.set_status(status(
                            "failing",
                            format!("the moves could not be recorded: {e}"),
                            cursor,
                        ));
                        continue;
                    }
                }
            }
        };
        baseline = Some(next);
        pending_since = None;
        let why = if unread.is_empty() {
            why
        } else {
            format!(
                "{why}; unread, and judged when they read again: {}",
                unread.join(", ")
            )
        };
        feed.set_status(status("recording", why, head));
        // Publish what the record holds past what this replica last
        // published — whoever wrote it.
        if let Ok(rows) = feed.store.since(published, MAX_PAGE).await {
            for r in rows {
                published = published.max(r.seq);
                feed.publish(Frame::Move(Box::new(r)));
            }
        }
    }
}
