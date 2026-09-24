//! REPORTING TO — the person a session reports to, read from the tenant
//! rather than remembered (backlog 2de32950; David's resolution on the
//! `david-as-subject` anchor of design a5368918, 2026-09-19).
//!
//! WHY. Two facts about the operator lived only in agent memory files:
//! that the stack runs UTC while he reads Pacific time, and that his
//! company address is not the one the harness reports. Both were
//! already tenant data — the employee row carries `email`, and the
//! Location it names carries a NOT NULL `timezone` — but nothing an
//! agent runs consulted either, so deleting the memory files would have
//! left the behaviour depending on nothing, and the first wrong-timezone
//! report would have been a silent regression. A fact is migrated only
//! when an agent READS it through an ordinary door at the moment it
//! needs it; `boss orient` is the verb every session runs before it
//! picks up work, so this is where the line prints.
//!
//! WHO. The platform owner (`boss_core::platform_owner`) — the one
//! person the platform's own packets are filed to, the first active
//! `platform-admin` hire, or `BOSS_PLATFORM_OWNER` — is by the same
//! definition the person an agent working the platform reports to. One
//! definition, reused, rather than a second notion of "the operator".
//!
//! WHERE THE TIMEZONE LIVES — decided, not defaulted. Neither a Class
//! (a taxonomy value shared by many Subjects) nor a new column on the
//! employee: it is a property of WHERE the employee works, which is the
//! Location Subject the row already references and whose `timezone`
//! the locations registry already requires. A column on the employee
//! would hold the same fact twice (CLAUDE.md §9a); a remote worker gets
//! a remote location, as `loc-algedonic-hq` ("HQ (remote)") already is.
//!
//! NEVER SILENT. Each read that fails says which one and why, and says
//! what to do instead (report in UTC and say so), because a reader line
//! that vanished on an error would look exactly like one never written.

use serde_json::Value;

use boss_core::platform_owner::PlatformOwner;

/// What the tenant says about the person this session reports to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Reader {
    pub id: String,
    pub name: String,
    /// The company address — the employee row's own `email`.
    pub email: String,
    /// The Location the row names, and its timezone — or why neither
    /// could be read.
    pub timezone: Result<(String, String), String>,
}

/// The employee row (and the Location it names, when it was read) as a
/// [`Reader`]. Pure, so every wording below is pinned without a socket.
pub(crate) fn reader_from(
    id: &str,
    employee: &Value,
    location: Option<Result<Value, String>>,
) -> Reader {
    let text = |v: &Value, key: &str| {
        v.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let timezone = match (text(employee, "location"), location) {
        (None, _) => Err("the employee row names no location".to_string()),
        (Some(loc), None) => Err(format!("location {loc} was not read")),
        (Some(_), Some(Err(e))) => Err(e),
        (Some(loc), Some(Ok(row))) => text(&row, "timezone")
            .map(|tz| (loc.clone(), tz))
            .ok_or_else(|| format!("location {loc} carries no timezone")),
    };
    Reader {
        id: id.to_string(),
        name: text(employee, "name").unwrap_or_else(|| id.to_string()),
        email: text(employee, "email").unwrap_or_default(),
        timezone,
    }
}

/// What to do about a time when no timezone could be read.
const UTC_OUT_LOUD: &str = "report times in UTC and say so — nothing here converts them";

/// The section as printed.
pub(crate) fn lines(reader: &Result<Reader, String>) -> Vec<String> {
    let r = match reader {
        Ok(r) => r,
        Err(e) => {
            return vec![
                format!("  REPORTING TO — UNKNOWN: {e}"),
                format!("    {UTC_OUT_LOUD}"),
            ];
        }
    };
    let address = if r.email.is_empty() {
        "<no email on the employee row>".to_string()
    } else {
        format!("<{}>", r.email)
    };
    let mut out = vec![format!(
        "  REPORTING TO — {} {address}  ({}, the platform owner)",
        r.name, r.id
    )];
    match &r.timezone {
        Ok((loc, tz)) => {
            out.push(format!("    timezone {tz}, from location {loc}."));
            out.push(format!(
                "    the stack runs UTC: give every time you report in {tz}, or as a delta from now"
            ));
        }
        Err(e) => {
            out.push(format!("    timezone UNKNOWN: {e}"));
            out.push(format!("    {UTC_OUT_LOUD}"));
        }
    }
    out
}

/// Read the owner, their employee row and its Location. Any read that
/// fails becomes the named reason, never an early return that drops the
/// section.
pub(crate) async fn read(
    http: &reqwest::Client,
    owner: &dyn PlatformOwner,
    people_base: &str,
    locations_base: &str,
) -> Result<Reader, String> {
    let id = owner.platform_owner().await.map_err(|e| e.to_string())?;
    let employee = get(http, people_base, &format!("/api/people/{id}"))
        .await
        .map_err(|e| format!("the platform owner {id} has no readable employee row: {e}"))?;
    let location = match employee.get("location").and_then(Value::as_str) {
        Some(loc) if !loc.trim().is_empty() => Some(
            get(
                http,
                locations_base,
                &format!("/api/locations/{}", loc.trim()),
            )
            .await,
        ),
        _ => None,
    };
    Ok(reader_from(&id, &employee, location))
}

/// [`read`] against the service ports the jobs base implies — the same
/// host, each service on its `boss_ports` prod port (the rule
/// `owner::people_base_from` already applies for filing).
pub(crate) async fn section(http: &reqwest::Client) -> Vec<String> {
    let base = match crate::gate::resolve_jobs_base(None) {
        Ok(b) => b,
        Err(e) => return lines(&Err(e.to_string())),
    };
    let owner = crate::owner::resolver(&base);
    let people =
        crate::owner::people_base_from(std::env::var(crate::owner::PEOPLE_ENV).ok(), &base);
    let locations = crate::owner::on_port(&base, boss_ports::prod("locations"));
    lines(&read(http, &owner, &people, &locations).await)
}

/// A GET through the signed door, as a named reason on failure.
async fn get(http: &reqwest::Client, base: &str, path: &str) -> Result<Value, String> {
    match crate::gate::api_at(http, base, reqwest::Method::GET, path, None).await {
        Ok(Some(v)) => Ok(v),
        Ok(None) => Err(format!("GET {path} answered no JSON")),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_core::platform_owner::Fixed;
    use serde_json::json;

    fn employee() -> Value {
        json!({ "id": "emp-ada", "name": "Ada Operator", "email": "ada@example.test",
                "location": "loc-remote", "role": "platform-admin", "status": "active" })
    }

    fn location() -> Value {
        json!({ "id": "loc-remote", "name": "HQ (remote)", "kind": "office",
                "timezone": "Europe/Lisbon" })
    }

    /// The whole point: the name, the company address and the timezone
    /// all reach the session, with the rule that makes the timezone
    /// matter — the stack runs UTC.
    #[test]
    fn the_reader_line_carries_the_address_and_the_timezone() {
        let r = reader_from("emp-ada", &employee(), Some(Ok(location())));
        assert_eq!(r.name, "Ada Operator");
        assert_eq!(r.email, "ada@example.test");
        assert_eq!(
            r.timezone,
            Ok(("loc-remote".to_string(), "Europe/Lisbon".to_string()))
        );
        let text = lines(&Ok(r)).join("\n");
        assert!(
            text.starts_with(
                "  REPORTING TO — Ada Operator <ada@example.test>  (emp-ada, the platform owner)"
            ),
            "{text}"
        );
        assert!(
            text.contains("timezone Europe/Lisbon, from location loc-remote"),
            "{text}"
        );
        assert!(
            text.contains("the stack runs UTC: give every time you report in Europe/Lisbon"),
            "{text}"
        );
    }

    /// A Location that could not be read keeps the rest of the line and
    /// says what to do about the time.
    #[test]
    fn an_unreadable_location_says_so_and_falls_back_to_utc_out_loud() {
        let r = reader_from(
            "emp-ada",
            &employee(),
            Some(Err("GET /api/locations/loc-remote -> 404".into())),
        );
        let text = lines(&Ok(r)).join("\n");
        assert!(text.contains("<ada@example.test>"), "{text}");
        assert!(
            text.contains("timezone UNKNOWN: GET /api/locations/loc-remote -> 404"),
            "{text}"
        );
        assert!(text.contains("report times in UTC and say so"), "{text}");
    }

    /// An employee row that names no Location has no timezone, and a
    /// Location row with no timezone field is refused by name.
    #[test]
    fn no_location_and_no_timezone_field_are_each_named() {
        let mut e = employee();
        e["location"] = Value::Null;
        let r = reader_from("emp-ada", &e, None);
        assert_eq!(
            r.timezone,
            Err("the employee row names no location".to_string())
        );
        let r = reader_from(
            "emp-ada",
            &employee(),
            Some(Ok(json!({ "id": "loc-remote" }))),
        );
        assert_eq!(
            r.timezone,
            Err("location loc-remote carries no timezone".to_string())
        );
    }

    /// No owner at all is a line, never an absent section.
    #[test]
    fn an_unresolvable_owner_is_a_line_not_a_silence() {
        let text = lines(&Err("no platform owner: nobody holds it".into())).join("\n");
        assert!(
            text.starts_with("  REPORTING TO — UNKNOWN: no platform owner: nobody holds it"),
            "{text}"
        );
        assert!(text.contains("report times in UTC and say so"), "{text}");
    }

    /// Serves `/api/people/emp-ada` and `/api/locations/loc-remote`;
    /// anything else is a 404.
    async fn stub() -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let text = String::from_utf8_lossy(&buf).into_owned();
                let target = text.split_whitespace().nth(1).unwrap_or("/").to_string();
                let (status, body) = match target.as_str() {
                    "/api/people/emp-ada" => ("200 OK", employee().to_string()),
                    "/api/locations/loc-remote" => ("200 OK", location().to_string()),
                    _ => ("404 Not Found", "no such row".to_string()),
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });
        format!("http://{addr}")
    }

    /// The read end to end: owner → employee row → its Location, each
    /// from its own service base.
    #[tokio::test]
    async fn the_read_follows_owner_to_employee_to_location() {
        let base = stub().await;
        let http = reqwest::Client::new();
        let r = read(&http, &Fixed("emp-ada".into()), &base, &base)
            .await
            .unwrap();
        assert_eq!(r.email, "ada@example.test");
        assert_eq!(
            r.timezone,
            Ok(("loc-remote".to_string(), "Europe/Lisbon".to_string()))
        );
        // An owner the registry has no row for is named, not dropped.
        let missing = read(&http, &Fixed("emp-gone".into()), &base, &base)
            .await
            .unwrap_err();
        assert!(missing.contains("emp-gone"), "{missing}");
    }
}
