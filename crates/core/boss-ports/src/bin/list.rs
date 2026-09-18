//! `boss-ports-list` — emits the canonical port table in a
//! shell-sourceable form so `infra/oss-quickstart/generate-configs.sh`
//! reads the same source of truth as the Rust binaries, plus a JSON
//! mode the SPA build step consumes to generate
//! `apps/web/src/_generated/ports.ts` (deriving the in-app
//! services list and the dev-server's proxy routes from the
//! Rust source of truth — no more hand-maintained mirrors).
//!
//! Modes:
//!   --paired   "name:prod:scratch" lines for each paired service
//!   --solo     "name:prod" lines for each solo service
//!   --json     full registry as JSON (paired + solo, prod + scratch)
//!   (default)  both shell sections, with a comment header per section

fn main() {
    let arg = std::env::args().nth(1);
    match arg.as_deref() {
        Some("--paired") => print_paired(),
        Some("--solo") => print_solo(),
        Some("--json") => print_json(),
        Some("--help") | Some("-h") => {
            eprintln!("usage: boss-ports-list [--paired | --solo | --json]");
            std::process::exit(0);
        }
        None => {
            println!("# paired services (name:prod:scratch)");
            print_paired();
            println!();
            println!("# solo services (name:prod)");
            print_solo();
        }
        Some(other) => {
            eprintln!("boss-ports-list: unknown arg '{other}'");
            std::process::exit(2);
        }
    }
}

fn print_paired() {
    for s in boss_ports::PAIRED {
        let scratch = s.scratch.expect("paired services have a scratch port");
        println!("{}:{}:{}", s.name, s.prod, scratch);
    }
}

fn print_solo() {
    for s in boss_ports::SOLO {
        println!("{}:{}", s.name, s.prod);
    }
}

/// JSON shape the SPA build step parses. Hand-rolled (no serde
/// dep — this crate stays tiny) but stable: a top-level array of
/// objects with `name`, `prod`, `scratch` (nullable).
fn print_json() {
    print!("[");
    let mut first = true;
    for s in boss_ports::all() {
        if !first {
            print!(",");
        }
        first = false;
        match s.scratch {
            Some(scratch) => {
                print!(
                    "{{\"name\":\"{}\",\"prod\":{},\"scratch\":{}}}",
                    s.name, s.prod, scratch
                );
            }
            None => {
                print!(
                    "{{\"name\":\"{}\",\"prod\":{},\"scratch\":null}}",
                    s.name, s.prod
                );
            }
        }
    }
    println!("]");
}
