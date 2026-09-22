//! `boss reach <ipv4> <port>` — can THIS host open a TCP connection to
//! ip:port? READ-ONLY, and the one definition of the `reach` ops verb.
//!
//! Until backlog 9f00a805 car 3 (2026-09-22) this was
//! `infra/forge/reach.sh`, 65 lines of bash doing the connect through
//! `/dev/tcp` under `timeout 4`, because the forge had no `boss`
//! binary. Car 1 of that item put the CLI on the forge from the
//! converged image (`infra/estate/install-cli-from-image.sh`) and car 2
//! retired the first twin; this is the next one. The script is deleted
//! and `infra/ops/verbs/reach.json` names `boss reach` instead.
//!
//! WHAT. One TCP connect, under a 4 s deadline. Nothing is sent on the
//! socket and nothing is read from it: the handshake completing, being
//! refused, or timing out IS the answer. Nothing on this host is
//! mutated. Prints exactly one line —
//!
//!     reach: <ip>:<port> open (<N>ms)
//!     reach: <ip>:<port> closed (<why the kernel refused>)
//!     reach: <ip>:<port> timeout (4s)
//!
//! and exits 0 only when open (1 otherwise, 2 on bad usage), so an ops
//! packet's `exit_code` carries the verdict as well as its output. The
//! shape is the shell's, verbatim, because the ops-request output shape
//! stays across a twin retirement.
//!
//! `closed` names the reason because the two it hides are different
//! answers to the question this was built for: "Connection refused"
//! means the host is reachable and the port is shut; "No route to host"
//! / "Network is unreachable" means the path itself is gone.
//!
//! WHY. The cluster dev pod cannot probe the network the way a host
//! can: no dig/traceroute/ping, no CAP_NET_RAW, and no route into the
//! WireGuard overlay (10.99.0.0/24). A LAN/tunnel-side vantage answers
//! whether the WireGuard hub 10.99.0.1 (boss-gcp) is reachable from the
//! forge over wg0 — the tunnel David's bastion route into the LAN
//! depends on.
//!
//! BOUNDS, AND THE ONE THING THE SHELL LACKED. The verb's params admit
//! only a dotted IPv4 quad and a 1-5 digit port (max 65535): no
//! hostname, so the host never resolves a name on a packet's behalf.
//! [`parse_target`] repeats that shape so the command is just as
//! bounded run by hand — and then PARSES the quad as an [`Ipv4Addr`].
//! The shell could not: `/dev/tcp/999.1.1.1/80` matched its regex and
//! was handed to the resolver, which is precisely the name lookup the
//! bound exists to prevent. An out-of-range octet (and a leading-zero
//! octet, which `Ipv4Addr` also refuses) is now usage, exit 2, before
//! any syscall.

use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream};
use std::time::{Duration, Instant};

/// The shell's `timeout 4`, kept so the `timeout (4s)` line stays true.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);

/// Bad arguments — the shell's `usage()` exit.
const USAGE_EXIT: i32 = 2;

/// What one connect attempt answered.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Open { ms: u128 },
    Closed { reason: String },
    Timeout,
}

/// True for the verb's own `^[0-9]{1,3}(\.[0-9]{1,3}){3}$`, checked
/// without a regex dependency so the bound is readable where it is
/// applied.
fn is_dotted_quad(ip: &str) -> bool {
    let parts: Vec<&str> = ip.split('.').collect();
    parts.len() == 4
        && parts
            .iter()
            .all(|p| (1..=3).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit()))
}

/// The verb's params, applied here too: a dotted IPv4 quad that is a
/// real address, and a 1-5 digit port in 1..=65535. `None` is usage.
pub fn parse_target(ip: &str, port: &str) -> Option<SocketAddr> {
    if !is_dotted_quad(ip) {
        return None;
    }
    let addr: Ipv4Addr = ip.parse().ok()?;
    if port.is_empty() || port.len() > 5 || !port.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let port: u16 = port.parse().ok()?;
    if port == 0 {
        return None;
    }
    Some(SocketAddr::V4(SocketAddrV4::new(addr, port)))
}

/// The kernel's reason without the ` (os error N)` tail that
/// `io::Error`'s Display appends — bash printed the bare reason, and
/// the line shape stays.
pub fn reason_of(err: &io::Error) -> String {
    let shown = err.to_string();
    match shown.rfind(" (os error ") {
        Some(cut) => shown[..cut].to_string(),
        None => shown,
    }
}

/// The one line the verb prints, for every outcome.
pub fn verdict_line(target: &SocketAddr, outcome: &Outcome) -> String {
    match outcome {
        Outcome::Open { ms } => format!("reach: {target} open ({ms}ms)"),
        Outcome::Closed { reason } => format!("reach: {target} closed ({reason})"),
        Outcome::Timeout => format!("reach: {target} timeout ({}s)", CONNECT_TIMEOUT.as_secs()),
    }
}

/// Exit 0 only when open, the way the shell did, so the packet's
/// `exit_code` is the verdict.
pub fn exit_code(outcome: &Outcome) -> i32 {
    match outcome {
        Outcome::Open { .. } => 0,
        _ => 1,
    }
}

/// One connect. The stream is dropped immediately: nothing is written
/// and nothing is read.
pub fn probe(target: SocketAddr) -> Outcome {
    let start = Instant::now();
    match TcpStream::connect_timeout(&target, CONNECT_TIMEOUT) {
        Ok(_stream) => Outcome::Open {
            ms: start.elapsed().as_millis(),
        },
        Err(err) if err.kind() == io::ErrorKind::TimedOut => Outcome::Timeout,
        Err(err) => Outcome::Closed {
            reason: reason_of(&err),
        },
    }
}

pub fn run(ip: &str, port: &str) -> ! {
    let Some(target) = parse_target(ip, port) else {
        eprintln!("usage: boss reach <ipv4> <port>");
        std::process::exit(USAGE_EXIT);
    };
    let outcome = probe(target);
    println!("{}", verdict_line(&target, &outcome));
    std::process::exit(exit_code(&outcome))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn a_dotted_quad_and_a_port_parse() {
        let target = parse_target("10.99.0.1", "51820").expect("a real address");
        assert_eq!(target.to_string(), "10.99.0.1:51820");
    }

    #[test]
    fn a_hostname_is_usage_not_a_lookup() {
        // The bound the whole verb rests on: this host never resolves a
        // name on a packet's behalf.
        assert!(parse_target("example.com", "443").is_none());
        assert!(parse_target("10.99.0.1.example.com", "443").is_none());
    }

    #[test]
    fn an_octet_over_255_is_refused_before_any_syscall() {
        // The gap the shell had: `999.1.1.1` matched its regex and went
        // to /dev/tcp, which resolves it as a NAME (9f00a805 car 3).
        assert!(parse_target("999.1.1.1", "80").is_none());
        assert!(parse_target("10.99.0.256", "80").is_none());
    }

    #[test]
    fn a_leading_zero_octet_is_refused() {
        // Ipv4Addr refuses it; the shell's regex did not, and a
        // leading zero is read as octal by some resolvers.
        assert!(parse_target("010.99.0.1", "80").is_none());
    }

    #[test]
    fn a_port_outside_the_verbs_bound_is_usage() {
        assert!(parse_target("10.99.0.1", "0").is_none());
        assert!(parse_target("10.99.0.1", "65536").is_none());
        assert!(parse_target("10.99.0.1", "123456").is_none());
        assert!(parse_target("10.99.0.1", "").is_none());
        assert!(parse_target("10.99.0.1", "80x").is_none());
        assert_eq!(
            parse_target("10.99.0.1", "65535").map(|t| t.to_string()),
            Some("10.99.0.1:65535".to_string())
        );
    }

    #[test]
    fn every_line_keeps_the_shells_shape() {
        let target = parse_target("10.99.0.1", "51820").expect("a real address");
        assert_eq!(
            verdict_line(&target, &Outcome::Open { ms: 3 }),
            "reach: 10.99.0.1:51820 open (3ms)"
        );
        assert_eq!(
            verdict_line(
                &target,
                &Outcome::Closed {
                    reason: "Connection refused".to_string()
                }
            ),
            "reach: 10.99.0.1:51820 closed (Connection refused)"
        );
        assert_eq!(
            verdict_line(&target, &Outcome::Timeout),
            "reach: 10.99.0.1:51820 timeout (4s)"
        );
    }

    #[test]
    fn the_os_error_tail_is_not_part_of_the_reason() {
        let err = io::Error::from_raw_os_error(111);
        assert!(err.to_string().contains("(os error 111)"));
        assert_eq!(reason_of(&err), "Connection refused");
    }

    #[test]
    fn a_listening_port_is_open_and_exits_zero() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
        let target = listener.local_addr().expect("its address");
        let outcome = probe(target);
        assert!(
            matches!(outcome, Outcome::Open { .. }),
            "expected open, got {outcome:?}"
        );
        assert_eq!(exit_code(&outcome), 0);
    }

    #[test]
    fn a_shut_port_is_closed_with_the_kernels_reason_and_exits_one() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
        let target = listener.local_addr().expect("its address");
        drop(listener);
        let outcome = probe(target);
        match &outcome {
            Outcome::Closed { reason } => assert_eq!(reason, "Connection refused"),
            other => panic!("expected closed, got {other:?}"),
        }
        assert_eq!(exit_code(&outcome), 1);
        assert_eq!(
            verdict_line(&target, &outcome),
            format!("reach: {target} closed (Connection refused)")
        );
    }
}
