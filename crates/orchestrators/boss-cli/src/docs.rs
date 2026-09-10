//! `boss docs reindex` — re-scan `docs/design/*.md` and refresh the
//! corpus index boss-docs-api serves.
//!
//! This used to carry `flush-pending` too: the worker that applied
//! recorded decisions to a markdown file, committed, and pushed. That
//! half was deleted on 2026-09-10 (backlog f5da586c) to execute the
//! decision settled in review 87f5bc84 — the packet is the doc, so
//! nothing writes back to a file.

use anyhow::{Context, Result, anyhow};

use crate::identity;

/// The docs API path this verb calls, named once so a refusal message
/// and the call it refuses cannot drift apart.
const REINDEX_PATH: &str = "/api/design/reindex";

/// Every `boss docs` call goes out signed by the actor RUNNING it,
/// through the SAME definition the jobs-API path uses
/// (`identity::signature_for` + `identity::header`) — never a second
/// one, because a second definition of "who" is a second answer
/// waiting to disagree (CLAUDE.md §9a).
///
/// WHY THIS EXISTS (backlog c3cd3301). `fix/the-cli-signs-as-its-caller`
/// made every jobs-API write carry its caller and refused an unnamed
/// one. It deliberately did not touch this file, which talks to a
/// DIFFERENT service (`BOSS_DOCS_API`) and sent no identity header at
/// all — so `boss docs` moved docs state anonymously while `boss job`,
/// `boss gate` and `boss prove` were exact. A guarantee that holds on
/// one surface and not the neighbouring one is worse than one that
/// holds nowhere: a reader assumes it is global.
///
/// The rule is therefore the jobs-API rule, verbatim. A WRITE carries
/// the caller or does not go out at all; a READ carries them if known
/// and goes out marked (`operator:unidentified`) otherwise.
fn signed(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
    signature: identity::Signature,
) -> Result<reqwest::RequestBuilder> {
    let signer = identity::apply(signature)?;
    Ok(client
        .request(method, url)
        .header("x-boss-user", identity::header(&signer)))
}

/// [`signed`] with the decision taken from this process's
/// environment. `path` is what a refusal names: the url carries the
/// instance too, which is not the part a reader needs.
fn request(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
    path: &str,
) -> Result<reqwest::RequestBuilder> {
    let signature = identity::signature_for(&method, path, identity::caller());
    signed(client, method, url, signature)
}

/// The message a caller gets when `BOSS_DOCS_API` is unset. Kept apart
/// from the lookup so a test can assert what it teaches without touching
/// process environment — the same split as `gate::no_instance_message`.
pub(crate) fn no_docs_api_message() -> String {
    "BOSS_DOCS_API is not set, and this verb has no default on purpose.\n\
     It used to fall back to http://127.0.0.1:7050. On boss-gcp that is \
     not the docs API of record — it is a SECOND, older docs stack \
     holding different data, and a wrong instance does not fail, it \
     answers, which is worse — the same defect class as `boss gate` \
     before aa783636.\n\
     Set it explicitly to the docs API of record for your vantage; from \
     inside the cluster:\n    \
     BOSS_DOCS_API=http://boss-docs-internal.boss.svc.cluster.local:7050 boss docs reindex"
        .to_string()
}

/// The docs API this verb talks to. **NO DEFAULT, deliberately** (packet
/// 7e10d3be). A default that is right on one host and silently wrong on
/// another IS the defect: `boss_ports::url("docs")` is `127.0.0.1:7050`,
/// which on boss-gcp is a legacy stack, so the verb reported success
/// against data nobody was looking at. A verb that cannot reach the
/// right instance now reaches none.
fn api_base() -> Result<String> {
    std::env::var("BOSS_DOCS_API")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| anyhow!("{}", no_docs_api_message()))
}

pub async fn reindex() -> Result<()> {
    let url = format!("{}{REINDEX_PATH}", api_base()?);
    let client = reqwest::Client::new();
    let resp = request(&client, reqwest::Method::POST, &url, REINDEX_PATH)?
        .send()
        .await
        .context("POST /api/design/reindex")?;
    if !resp.status().is_success() {
        anyhow::bail!("reindex failed: HTTP {}", resp.status());
    }
    let body: serde_json::Value = resp.json().await?;
    println!(
        "reindex complete: {} docs indexed, {} deleted ({} ms)",
        body["docs_indexed"].as_u64().unwrap_or(0),
        body["docs_deleted"].as_u64().unwrap_or(0),
        body["duration_ms"].as_u64().unwrap_or(0),
    );
    // A refused doc is ABSENT from the index, which is
    // indistinguishable from a doc nobody wrote — so name each one
    // here rather than leaving the reason in a response field nothing
    // prints (CLAUDE.md, Diagnosis: a reduction before storage throws
    // away the only copy).
    if let Some(rejected) = body["rejected"].as_array().filter(|r| !r.is_empty()) {
        println!("{} doc(s) REFUSED and not in the index:", rejected.len());
        for r in rejected {
            println!(
                "  - {}: {}",
                r["path"].as_str().unwrap_or("?"),
                r["reason"].as_str().unwrap_or("?")
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::no_docs_api_message;

    // The verb must refuse a silent default, and the refusal must TEACH:
    // name the variable, say there is no default, and warn that the old
    // 127.0.0.1:7050 fallback is a second stack that answers wrongly
    // (packet 7e10d3be). Asserted on the message, not the env lookup, so
    // the test is parallel-safe.
    #[test]
    fn the_missing_api_message_names_the_variable_and_the_trap() {
        let m = no_docs_api_message();
        assert!(m.contains("BOSS_DOCS_API"), "{m}");
        assert!(m.contains("no default"), "{m}");
        assert!(m.contains("127.0.0.1:7050"), "{m}");
        assert!(m.contains("boss-docs-internal"), "{m}");
    }
}

/// Backlog c3cd3301 — the docs path signs like the jobs-API path.
///
/// Asserted at the WIRE, for the same reason `gate::signing_tests`
/// is: what the socket carries is what a service can record, and a
/// unit test of the decision function would have passed on the old
/// code too — the decision was never the missing part, the header
/// was.
///
/// Retargeted from the flush-job status PUT to the reindex POST when
/// the flush pipeline was deleted (2026-09-10). The claim is unchanged
/// and the remaining write is the one that carries it.
#[cfg(test)]
mod signing_tests {
    use super::*;
    use crate::identity::Signature;

    /// A one-shot HTTP stub that hands back the request head it read.
    async fn one_request(body: &'static str) -> (String, tokio::task::JoinHandle<Option<String>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.ok()?;
            let mut buf = Vec::new();
            let mut chunk = [0u8; 2048];
            loop {
                match sock.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                 content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
            Some(String::from_utf8_lossy(&buf).into_owned())
        });
        (format!("http://{addr}"), handle)
    }

    /// The claim of the backlog item, at the wire: a docs write must
    /// arrive naming the operator who ran the verb, not nobody.
    #[tokio::test]
    async fn a_docs_write_arrives_signed_as_its_caller() {
        let (base, stub) = one_request("{}").await;
        let http = reqwest::Client::new();
        signed(
            &http,
            reqwest::Method::POST,
            &format!("{base}{REINDEX_PATH}"),
            Signature::As("claude@algedonic.dev".into()),
        )
        .expect("a named write is not refused")
        .send()
        .await
        .expect("the stub answers 200");
        let head = stub.await.unwrap().expect("the stub read a request");
        assert!(
            head.contains(r#""id":"claude@algedonic.dev""#),
            "a docs write must name its caller; head was:\n{head}"
        );
        assert!(
            !head.contains(crate::identity::CONDUCTOR),
            "an operator's docs write must not be signed as the train automation; head was:\n{head}"
        );
    }

    /// The chosen behaviour for an unnamed writer: REFUSED, exactly as
    /// the jobs-API path refuses. A reindex rewrites every row the
    /// review surfaces read; an anonymous one of those is the defect,
    /// and a placeholder id would record a fiction rather than stop.
    #[tokio::test]
    async fn an_unnamed_docs_write_never_reaches_the_network() {
        let (base, stub) = one_request("{}").await;
        let http = reqwest::Client::new();
        let refusal = crate::identity::refusal("POST", REINDEX_PATH);
        let err = signed(
            &http,
            reqwest::Method::POST,
            &format!("{base}{REINDEX_PATH}"),
            Signature::Refused(refusal.clone()),
        )
        .expect_err("an unnamed write is refused");
        assert_eq!(err.to_string(), refusal);
        assert!(
            err.to_string().contains(crate::identity::ACTOR_ENV),
            "the refusal must say how to fix it: {err}"
        );
        // The stub finishes only once it has ACCEPTED a connection, so
        // an unfinished stub is proof nothing was sent.
        assert!(
            !stub.is_finished(),
            "a refused docs write must not reach the socket"
        );
        stub.abort();
    }

    /// A read attributes nothing, so it proceeds — marked, never as an
    /// automation slug. Refusing it would stop a read on a box that has
    /// not named its operator, for no provenance gained.
    #[tokio::test]
    async fn an_unnamed_docs_read_arrives_marked() {
        let (base, stub) = one_request("[]").await;
        let http = reqwest::Client::new();
        signed(
            &http,
            reqwest::Method::GET,
            &format!("{base}/api/design/docs"),
            Signature::Unidentified,
        )
        .expect("a read is never refused")
        .send()
        .await
        .expect("the stub answers 200");
        let head = stub.await.unwrap().expect("the stub read a request");
        assert!(
            head.contains(crate::identity::UNIDENTIFIED),
            "an unnamed read must say so; head was:\n{head}"
        );
        assert!(!head.contains(crate::identity::CONDUCTOR), "{head}");
    }

    /// The uniformity claim itself: the docs path does not decide who
    /// signs, it ASKS the one definition. A second copy of the rule
    /// here is the failure mode this car exists to close, so the test
    /// pins the decision to `identity::signature_for` rather than
    /// restating it.
    #[test]
    fn the_docs_path_asks_the_one_definition() {
        match crate::identity::signature_for(&reqwest::Method::POST, REINDEX_PATH, None) {
            Signature::Refused(msg) => assert!(msg.contains(REINDEX_PATH), "{msg}"),
            other => panic!("an unnamed docs write must be refused, got {other:?}"),
        }
        assert_eq!(
            crate::identity::signature_for(&reqwest::Method::GET, "/api/design/docs", None),
            Signature::Unidentified
        );
    }
}
