//! WHICH METADATA KEYS A FILTER MAY NAME — one definition, both doors.
//!
//! `GET /api/jobs?metadata_has=<key>` binds the key to `metadata ? $n`,
//! which reads TOP-LEVEL keys only. A dotted path, a dash, a leading
//! digit or an empty string would be bound as one literal key, match
//! nothing, and answer `total: 0` with a straight face — the
//! wrong-target shape CLAUDE.md §Doors names. So the server refuses
//! anything that is not a plain identifier, and `boss job list --has`
//! refuses the same thing BEFORE the round trip.
//!
//! Until 2026-09-14 those two refusals were two copies of the same six
//! lines — server `metadata_key_from_query` (`http/jobs.rs`) and CLI
//! `has_key` (`boss-cli/src/job.rs`) — with nothing between them
//! (backlog b46e9d8e): a change to the charset on one side would have
//! left the terminal refusing a key the server takes, or sending one
//! the server 400s with a different sentence. This module is the one
//! copy (CLAUDE.md §9a: collapse if you can, pin if you cannot). Each
//! door adds only the NAME of its own parameter in front of what
//! [`check`] says; the rule itself is not theirs to word.

/// The rule, as both the 400 and the terminal say it. A door prefixes
/// its parameter's name (`metadata_has`, `--has`) and nothing else.
pub const RULE: &str = "must be a top-level metadata key — letters, digits and underscore, \
                        not starting with a digit";

/// Accept `key` if it is a plain identifier — ASCII letters, digits and
/// underscore, not starting with a digit — or say why not. The `Err` is
/// the whole sentence a door prints after its parameter's name: the
/// [`RULE`], the key as given, and the one thing people most often try
/// (a dotted path) named as not walked.
pub fn check(key: &str) -> Result<&str, String> {
    let mut chars = key.chars();
    let plain = matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if plain {
        Ok(key)
    } else {
        Err(format!("{RULE} — got {key:?}; dotted paths are not walked"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_identifier_passes_through_unchanged() {
        for ok in ["proof_probe", "_x9", "a", "estate_finding", "Branch2"] {
            assert_eq!(check(ok), Ok(ok));
        }
    }

    #[test]
    fn anything_else_is_refused_with_the_rule_and_the_key() {
        for bad in ["steps.0", "9lives", "a-b", "", "has space", "ünïcode"] {
            let why = check(bad).unwrap_err();
            assert!(why.starts_with(RULE), "{bad:?}: {why}");
            assert!(why.contains(&format!("{bad:?}")), "{bad:?}: {why}");
            assert!(
                why.ends_with("dotted paths are not walked"),
                "{bad:?}: {why}"
            );
        }
    }
}
