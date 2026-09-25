//! MARSHALLING: packets standing at a station other than the dock,
//! counted once each and never a packet another region holds.

use super::*;

/// MARSHALLING: packets standing at a station other than the dock,
/// counted once each. The marshalling yard's grammar decides the
/// state: a station over its WIP limit, or holding work that nothing
/// left in the window (not draining), is troubled; any station holding
/// work that drains is clear. The trend is packets served per day, summed over the
/// stations whose flow the cube can count.
pub(super) fn marshalling(inputs: &RegionInputs<'_>, w: &Windows) -> Region {
    const UNIT: &str = "packets at stations";
    // THE PARTITION (design 62de32ae decision 4): each station holding
    // only what is marshalling's — never a packet still in receiving or
    // held by a region of its own.
    let stations = match marshalling_view(inputs) {
        Ok(view) => view,
        Err(why) => {
            return region(
                "marshalling",
                None,
                None,
                UNIT,
                unread_settled(why, inputs.now),
                Trend {
                    metric: "served".to_string(),
                    unit: "per day".to_string(),
                    current: None,
                    previous: None,
                    samples: 0,
                    previous_samples: 0,
                },
                vec![measure("oldest at a station", None, "hours")],
            );
        }
    };
    let distinct: std::collections::BTreeSet<&str> = stations
        .iter()
        .flat_map(|s| s.members.iter().map(String::as_str))
        .collect();
    let served: Option<i64> = stations
        .iter()
        .filter_map(|s| s.served)
        .reduce(|a, b| a + b);
    let previous: Option<i64> = stations
        .iter()
        .filter_map(|s| s.previous_served)
        .reduce(|a, b| a + b);
    let to_rate = |n: Option<i64>| {
        n.and_then(|n| usize::try_from(n).ok())
            .map(|n| w.per_day(n))
    };
    let trend = Trend {
        metric: "served".to_string(),
        unit: "per day".to_string(),
        current: to_rate(served),
        previous: to_rate(previous),
        samples: served.and_then(|n| usize::try_from(n).ok()).unwrap_or(0),
        previous_samples: previous.and_then(|n| usize::try_from(n).ok()).unwrap_or(0),
    };
    let over: Vec<&str> = stations
        .iter()
        .filter(|s| s.over_limit)
        .map(|s| s.name.as_str())
        .collect();
    let stuck: Vec<&str> = stations
        .iter()
        .filter(|s| !s.members.is_empty() && s.served == Some(0))
        .map(|s| s.name.as_str())
        .collect();
    let holding = stations.iter().filter(|s| !s.members.is_empty()).count();
    let n = distinct.len();
    let mut findings = Vec::new();
    if !over.is_empty() {
        findings.push(Finding::new(
            bands::MARSHALLING_OVER_LIMIT,
            None,
            String::new(),
            format!("over the WIP limit: {}", over.join(", ")),
        ));
    }
    if !stuck.is_empty() {
        findings.push(Finding::new(
            bands::MARSHALLING_NOT_DRAINING,
            None,
            String::new(),
            format!(
                "not draining — nothing left in {}h: {}",
                w.hours,
                stuck.join(", ")
            ),
        ));
    }
    // Packets standing at stations that drain is the yard working —
    // clear (decision 1).
    let clear_why = if holding > 0 {
        format!(
            "{} standing at {}",
            plural(n, "packet", "packets"),
            plural(holding, "station", "stations")
        )
    } else {
        "nothing waiting at any station".to_string()
    };
    let settled = settle(findings, clear_why, inputs.now);
    // THE KPI (decision 9): the oldest packet standing at a station, in
    // hours from its opening — of the members the partition left here.
    let oldest = stations
        .iter()
        .flat_map(|s| s.members.iter().filter_map(|m| s.opened.get(m)))
        .min()
        .map(|t| (inputs.now - t).num_hours().max(0));
    #[allow(clippy::cast_precision_loss)]
    let mut kpi = vec![match oldest {
        Some(h) => measure("oldest at a station", Some(h as f64), "hours"),
        None if n == 0 => measure_said(
            "oldest at a station",
            None,
            "hours",
            "nothing standing".to_string(),
        ),
        None => measure("oldest at a station", None, "hours"),
    }];
    // BLINDNESS COUNTED (decision 11): the stations whose flow nobody
    // can count are drawn `?`, and the header says how many — a zero
    // is stated too, because it is a reading.
    let blind = stations.iter().filter(|s| unjudged(s)).count();
    kpi.push(measure_said(
        "stations unjudged",
        count_value(blind),
        "stations",
        if blind == 0 {
            "every station judged".to_string()
        } else {
            format!("{} unjudged", plural(blind, "station", "stations"))
        },
    ));
    // EACH STATION AT MARSHALLING'S OWN MEMBERS (the rest of decision
    // 5): the partition, per place, so the interior can draw the head's
    // count instead of each station's full depth.
    let places = stations
        .iter()
        .map(|s| Place {
            name: s.name.clone(),
            count: s.members.len(),
        })
        .collect();
    Region {
        places,
        ..region("marshalling", Some(n), None, UNIT, settled, trend, kpi)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regions::fixtures::*;

    /// Marshalling: packets counted once across stations; a station
    /// nothing leaves is trouble; served per day sums the countable
    /// stations.
    #[test]
    fn marshalling_counts_packets_once_and_a_station_not_draining_is_trouble() {
        let stations = vec![
            StationReading {
                name: "q.platform-admin.task".into(),
                over_limit: false,
                members: vec!["p1".into(), "p2".into()],
                served: Some(4),
                previous_served: Some(2),
                opened: [("p1".to_string(), t("2026-09-18T12:00:00Z"))].into(),
            },
            StationReading {
                name: "design-review".into(),
                over_limit: false,
                members: vec!["p2".into()],
                served: Some(1),
                previous_served: Some(0),
                opened: [("p2".to_string(), t("2026-09-19T06:00:00Z"))].into(),
            },
            StationReading {
                name: "my-watchlist".into(),
                over_limit: false,
                members: vec![],
                served: None,
                previous_served: None,
                opened: Default::default(),
            },
        ];
        let status = empty_status();
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &[],
            &[],
            Some(&[]),
            Some(&stations),
        ));
        let m = by_name(&out, "marshalling");
        assert_eq!(m.count, Some(2), "p2 stands at two stations, counted once");
        // Standing at stations that drain is the yard working.
        assert_eq!(m.state, RegionState::Clear, "{}", m.why);
        assert_eq!(m.kpi[0].text, "oldest at a station 24 hours");
        assert_eq!(m.trend.current, Some(5.0));
        assert_eq!(m.trend.previous, Some(2.0));

        let mut stuck = stations.clone();
        stuck[1].served = Some(0);
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &[],
            &[],
            Some(&[]),
            Some(&stuck),
        ));
        let m = by_name(&out, "marshalling");
        assert_eq!(m.state, RegionState::Troubled);
        assert!(m.why.contains("design-review"), "{}", m.why);
    }

    /// An unread intake leaves marshalling unknown rather than counting
    /// packets that may still be receiving's — no evidence is not a pass.
    #[test]
    fn marshalling_without_the_inbound_read_is_unknown_not_a_double_count() {
        let stations = vec![holding("q.platform-admin.task", &["p1"])];
        let status = empty_status();
        let out = regions(&inputs(&status, &[], &[], &[], &[], None, Some(&stations)));
        let m = by_name(&out, "marshalling");
        assert_eq!(m.count, None);
        assert_eq!(m.state, RegionState::Troubled);
        assert!(m.why.contains("still in receiving"), "{}", m.why);
        assert!(m.places.is_empty(), "an unread partition has no places");
    }

    /// EACH STATION AT ITS PARTITIONED COUNT. Live on 2026-09-24 the
    /// marshalling platforms drew 562 standings under a head of 236:
    /// each station drew its full depth, because the station reads carry
    /// no partition. The server holds it, so it says per station how many
    /// of MARSHALLING'S members stand there — an untriaged packet at the
    /// same station is receiving's and is not among them. A packet at
    /// two stations is at both, and the places sum past the head by
    /// exactly the packets that stand twice.
    #[test]
    fn marshalling_says_how_many_of_its_own_members_stand_at_each_station() {
        let untriaged = admitted(
            "backlog-item",
            "untriaged",
            &[("triage", "task", StepStatus::Ready)],
        );
        let triaged = admitted(
            "backlog-item",
            "triaged",
            &[
                ("triage", "task", StepStatus::Completed),
                ("build", "task", StepStatus::Ready),
            ],
        );
        let (u, tr) = (untriaged.0.id.to_string(), triaged.0.id.to_string());
        let inbound = vec![untriaged, triaged];
        let stations = vec![
            holding("q.platform-admin.task", &[&u, &tr, "other"]),
            holding("a.platform-admin.opus", &[&tr]),
            holding("design-review", &[]),
        ];
        let status = empty_status();
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &[],
            &[],
            Some(&inbound),
            Some(&stations),
        ));
        let m = by_name(&out, "marshalling");
        assert_eq!(m.count, Some(2), "{}", m.why);
        let places: Vec<(&str, usize)> = m
            .places
            .iter()
            .map(|p| (p.name.as_str(), p.count))
            .collect();
        assert_eq!(
            places,
            vec![
                ("q.platform-admin.task", 2),
                ("a.platform-admin.opus", 1),
                ("design-review", 0),
            ],
            "every station, in the registry's order, at marshalling's own members"
        );
        let drawn: usize = m.places.iter().map(|p| p.count).sum();
        assert_eq!(
            drawn - m.count.unwrap(),
            1,
            "the one packet at two stations"
        );
        // Nothing else has places it does not declare.
        assert!(by_name(&out, "receiving").places.is_empty());
    }

    /// STATIONS THE SERVER CANNOT JUDGE ARE COUNTED IN THE HEADER
    /// (decision 11): blindness is itself a KPI. A station is unjudged by
    /// the same predicate that draws its glyph `?`, so the header and the
    /// glyphs cannot disagree about how many there are.
    #[test]
    fn marshalling_counts_the_stations_it_cannot_judge_in_its_header() {
        let blind = |name: &str| StationReading {
            name: name.into(),
            served: None,
            previous_served: None,
            ..Default::default()
        };
        let stations = vec![
            blind("design-review"),
            blind("a.platform-admin.opus"),
            holding("q.platform-admin.task", &[]),
        ];
        let status = empty_status();
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &[],
            &[],
            Some(&[]),
            Some(&stations),
        ));
        let m = by_name(&out, "marshalling");
        let unjudged = m
            .kpi
            .iter()
            .find(|k| k.name == "stations unjudged")
            .expect("the header counts the stations it cannot judge");
        assert_eq!(unjudged.value, Some(2.0));
        assert_eq!(unjudged.text, "2 stations unjudged");
        let glyphs = m
            .machines
            .iter()
            .filter(|g| g.state == MachineState::Unknown)
            .count();
        assert_eq!(glyphs, 2, "the header's count is the glyphs' count");

        let judged = vec![holding("q.platform-admin.task", &[])];
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &[],
            &[],
            Some(&[]),
            Some(&judged),
        ));
        let k = by_name(&out, "marshalling")
            .kpi
            .iter()
            .find(|k| k.name == "stations unjudged")
            .cloned()
            .expect("a zero is a reading too");
        assert_eq!(
            (k.value, k.text.as_str()),
            (Some(0.0), "every station judged")
        );
    }
}
