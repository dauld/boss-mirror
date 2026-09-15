//! The packet partition — which company a Job belongs to.
//!
//! `real` is the operating company. `simulated` is the demo tenant's
//! synthetic load (the brewery sim). `shadow` is the experiment lane
//! (docs/design/network-experiments.md, Tier 3; packet 508cc38c, Q2
//! decided 2026-08-22): a protocol run against a mirror of real
//! traffic that must reach a terminal WITHOUT effects. The three values
//! are one fact — decided ONCE at admission and immutable thereafter —
//! and every boundary consumer reads the same fact through this type
//! instead of keeping its own bool.
//!
//! THE ONE RULE: a consumer that fails closed on `simulated` fails
//! closed on `shadow` the same way, and the sim never participates in
//! an experiment (Q5). So `fails_closed()` is "not real", not "is
//! simulated": the real lane excludes both, and a consumer that wants
//! the simulated company specifically (the sim workforce, the epoch
//! trim) says `== Partition::Simulated`, never `fails_closed()`.
//!
//! This is NOT the sim CLOCK. `boss_core::clock::Now::simulated` and
//! the `X-Sim-Time` header say what time it is; this says whose packet
//! it is. The two share a word, not a meaning.
//!
//! Wire shape (expand/contract, N-1 compatible): a partitioned record
//! carries `partition: "real" | "simulated" | "shadow"` AND the legacy
//! `simulated: bool`, DERIVED as `partition != real` — so an old reader
//! that tests `simulated` fails closed on a shadow packet without
//! knowing the word. A body that sends only `simulated: true` reads as
//! `Simulated`; `partition` wins when both are present. [`wire`] is the
//! serde module that does this for a flattened field.

use serde::{Deserialize, Serialize};

/// Whose packet it is. `Default` is `Real`: absence never invents a
/// simulated company (the rebuilder's posture since the flag existed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Partition {
    #[default]
    Real,
    Simulated,
    Shadow,
}

impl Partition {
    /// Every value, in declaration order — for validators and
    /// error messages that name the vocabulary.
    pub const ALL: [Partition; 3] = [Partition::Real, Partition::Simulated, Partition::Shadow];

    pub fn is_real(self) -> bool {
        matches!(self, Partition::Real)
    }

    /// The fail-closed predicate: `true` for every packet a real-lane
    /// consumer must refuse. Not "is simulated" — see the module doc.
    pub fn fails_closed(self) -> bool {
        !self.is_real()
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Partition::Real => "real",
            Partition::Simulated => "simulated",
            Partition::Shadow => "shadow",
        }
    }

    /// The legacy `simulated: bool` read as a partition — the only
    /// two values that existed before `shadow`.
    pub fn from_legacy_simulated(simulated: bool) -> Self {
        if simulated {
            Partition::Simulated
        } else {
            Partition::Real
        }
    }

    /// The partition an event payload was stamped with: `_partition`
    /// when the stamp wrote one, else the older `_simulated` marker,
    /// else real. Every reader of the `_simulated` / `_source`
    /// marker family (rebuilder, dispatcher, escalation) goes through
    /// here so they cannot disagree on an old event. Absent, null or
    /// mis-typed reads as REAL — the partition's one documented
    /// posture (dispatcher.rs `event_partition`: an ambiguous packet
    /// read as sim would be workable by nobody).
    pub fn from_event_payload(payload: &serde_json::Value) -> Self {
        if let Some(p) = payload
            .get("_partition")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<Partition>().ok())
        {
            return p;
        }
        Partition::from_legacy_simulated(
            payload.get("_simulated").and_then(|v| v.as_bool()) == Some(true),
        )
    }
}

impl std::fmt::Display for Partition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A value outside the three-word vocabulary. Names the word so a 400
/// can say what it refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown partition {0:?}; expected one of real, simulated, shadow")]
pub struct UnknownPartition(pub String);

impl std::str::FromStr for Partition {
    type Err = UnknownPartition;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "real" => Ok(Partition::Real),
            "simulated" => Ok(Partition::Simulated),
            "shadow" => Ok(Partition::Shadow),
            other => Err(UnknownPartition(other.to_string())),
        }
    }
}

/// Serde for a partitioned record's flattened field: writes
/// `partition` AND the derived legacy `simulated`, reads either.
///
/// ```ignore
/// #[serde(flatten, with = "boss_core::partition::wire")]
/// pub partition: Partition,
/// ```
pub mod wire {
    use super::Partition;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize)]
    struct Out {
        partition: Partition,
        simulated: bool,
    }

    #[derive(Deserialize)]
    struct In {
        #[serde(default)]
        partition: Option<Partition>,
        #[serde(default)]
        simulated: bool,
    }

    pub fn serialize<S: Serializer>(p: &Partition, s: S) -> Result<S::Ok, S::Error> {
        Out {
            partition: *p,
            simulated: p.fails_closed(),
        }
        .serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Partition, D::Error> {
        let In {
            partition,
            simulated,
        } = In::deserialize(d)?;
        Ok(partition.unwrap_or_else(|| Partition::from_legacy_simulated(simulated)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn real_is_the_default_and_the_only_open_value() {
        assert_eq!(Partition::default(), Partition::Real);
        assert!(Partition::Real.is_real());
        assert!(!Partition::Real.fails_closed());
        assert!(Partition::Simulated.fails_closed());
        // The whole point of the type: shadow closes every door
        // simulated closes.
        assert!(Partition::Shadow.fails_closed());
        assert!(!Partition::Shadow.is_real());
    }

    #[test]
    fn serde_is_the_lowercase_word() {
        for p in Partition::ALL {
            assert_eq!(serde_json::to_value(p).unwrap(), json!(p.as_str()));
            assert_eq!(
                serde_json::from_value::<Partition>(json!(p.as_str())).unwrap(),
                p
            );
            assert_eq!(p.as_str().parse::<Partition>().unwrap(), p);
            assert_eq!(p.to_string(), p.as_str());
        }
        assert!(serde_json::from_value::<Partition>(json!("Shadow")).is_err());
        assert_eq!(
            "nonsense".parse::<Partition>(),
            Err(UnknownPartition("nonsense".into()))
        );
    }

    #[test]
    fn legacy_bool_maps_to_the_two_old_values() {
        assert_eq!(Partition::from_legacy_simulated(false), Partition::Real);
        assert_eq!(Partition::from_legacy_simulated(true), Partition::Simulated);
    }

    #[test]
    fn event_payload_reads_partition_then_simulated_then_real() {
        assert_eq!(
            Partition::from_event_payload(&json!({"_partition": "shadow", "_simulated": true})),
            Partition::Shadow
        );
        // An old event: only the bool.
        assert_eq!(
            Partition::from_event_payload(&json!({"_simulated": true})),
            Partition::Simulated
        );
        assert_eq!(
            Partition::from_event_payload(&json!({"_simulated": false})),
            Partition::Real
        );
        // Absent / null / mis-typed read as real — the dispatcher's
        // documented posture, kept.
        assert_eq!(Partition::from_event_payload(&json!({})), Partition::Real);
        assert_eq!(
            Partition::from_event_payload(&json!({"_simulated": null})),
            Partition::Real
        );
        assert_eq!(
            Partition::from_event_payload(&json!({"_simulated": "true"})),
            Partition::Real
        );
        // An unreadable `_partition` falls through to the bool, so a
        // shadow-era event a reader cannot parse still fails closed.
        assert_eq!(
            Partition::from_event_payload(&json!({"_partition": "nope", "_simulated": true})),
            Partition::Simulated
        );
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Rec {
        name: String,
        #[serde(flatten, with = "wire")]
        partition: Partition,
    }

    #[test]
    fn wire_writes_both_keys_and_derives_simulated_as_not_real() {
        let v = serde_json::to_value(Rec {
            name: "x".into(),
            partition: Partition::Shadow,
        })
        .unwrap();
        assert_eq!(
            v,
            json!({"name": "x", "partition": "shadow", "simulated": true})
        );
        let v = serde_json::to_value(Rec {
            name: "x".into(),
            partition: Partition::Real,
        })
        .unwrap();
        assert_eq!(
            v,
            json!({"name": "x", "partition": "real", "simulated": false})
        );
    }

    #[test]
    fn wire_reads_partition_over_simulated_and_falls_back_to_the_bool() {
        let r: Rec = serde_json::from_value(json!({"name": "x", "partition": "shadow"})).unwrap();
        assert_eq!(r.partition, Partition::Shadow);
        // partition wins when both are present, even when they disagree.
        let r: Rec =
            serde_json::from_value(json!({"name": "x", "partition": "real", "simulated": true}))
                .unwrap();
        assert_eq!(r.partition, Partition::Real);
        // An N-1 body: only the bool.
        let r: Rec = serde_json::from_value(json!({"name": "x", "simulated": true})).unwrap();
        assert_eq!(r.partition, Partition::Simulated);
        // A pre-flag body: neither.
        let r: Rec = serde_json::from_value(json!({"name": "x"})).unwrap();
        assert_eq!(r.partition, Partition::Real);
        // Round trip.
        let rec = Rec {
            name: "x".into(),
            partition: Partition::Shadow,
        };
        let back: Rec = serde_json::from_value(serde_json::to_value(&rec).unwrap()).unwrap();
        assert_eq!(back, rec);
    }
}
