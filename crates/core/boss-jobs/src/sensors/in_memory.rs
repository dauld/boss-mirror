//! In-memory adapter for `Sensors` — the port-level test double, and
//! the stated twin of the Pg adapter's semantics: insert-if-absent on
//! both writes, the first stamp wins, the cursor is monotonic, the
//! sweep keeps a reading still owed a packet.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::RwLock;

use super::port::{Sensors, SensorsError};
use super::types::{BatchOutcome, NewReading, PollStamp, Reading, SensorInput, SensorRow};

#[derive(Default)]
pub struct InMemorySensors {
    sensors: RwLock<Vec<SensorRow>>,
    readings: RwLock<Vec<Reading>>,
}

impl InMemorySensors {
    pub fn new() -> Self {
        Self::default()
    }

    /// Test visibility into every reading held, in record order.
    pub async fn readings(&self) -> Vec<Reading> {
        self.readings.read().await.clone()
    }
}

#[async_trait]
impl Sensors for InMemorySensors {
    async fn list(&self) -> Result<Vec<SensorRow>, SensorsError> {
        let mut rows = self.sensors.read().await.clone();
        rows.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(rows)
    }

    async fn publish(
        &self,
        tenant_id: &str,
        sensors: &[SensorInput],
    ) -> Result<BatchOutcome, SensorsError> {
        let mut rows = self.sensors.write().await;
        let mut inserted = 0;
        for s in sensors {
            if rows.iter().any(|r| r.id == s.id) {
                continue;
            }
            rows.push(SensorRow {
                id: s.id.clone(),
                source: s.source.clone(),
                credential: s.credential.clone(),
                every_minutes: s.every_minutes,
                opens_kind: s.opens.clone(),
                subject_kind: s.subject_kind.clone(),
                enabled: s.enabled,
                tenant_id: tenant_id.to_string(),
                published_at: boss_clock_client::wall_now(),
                last_polled_at: None,
                cursor_at: None,
            });
            inserted += 1;
        }
        Ok(BatchOutcome {
            received: sensors.len(),
            inserted,
        })
    }

    async fn record(
        &self,
        sensor_id: &str,
        readings: &[NewReading],
    ) -> Result<BatchOutcome, SensorsError> {
        if !self.sensors.read().await.iter().any(|r| r.id == sensor_id) {
            return Err(SensorsError::UnknownSensor(sensor_id.to_string()));
        }
        let mut rows = self.readings.write().await;
        let mut inserted = 0;
        for r in readings {
            if rows
                .iter()
                .any(|h| h.sensor_id == sensor_id && h.external_id == r.external_id)
            {
                continue;
            }
            rows.push(Reading {
                sensor_id: sensor_id.to_string(),
                external_id: r.external_id.clone(),
                observed_at: r.observed_at,
                payload: r.payload.clone(),
                packet_id: None,
            });
            inserted += 1;
        }
        Ok(BatchOutcome {
            received: readings.len(),
            inserted,
        })
    }

    async fn unstamped(&self, sensor_id: &str) -> Result<Vec<Reading>, SensorsError> {
        let mut out: Vec<Reading> = self
            .readings
            .read()
            .await
            .iter()
            .filter(|r| r.sensor_id == sensor_id && r.packet_id.is_none())
            .cloned()
            .collect();
        out.sort_by(|a, b| {
            a.observed_at
                .cmp(&b.observed_at)
                .then_with(|| a.external_id.cmp(&b.external_id))
        });
        Ok(out)
    }

    async fn stamp(
        &self,
        sensor_id: &str,
        external_id: &str,
        packet_id: &str,
    ) -> Result<(), SensorsError> {
        let mut rows = self.readings.write().await;
        let Some(r) = rows
            .iter_mut()
            .find(|r| r.sensor_id == sensor_id && r.external_id == external_id)
        else {
            return Err(SensorsError::UnknownSensor(format!(
                "{sensor_id}/{external_id}"
            )));
        };
        if r.packet_id.is_none() {
            r.packet_id = Some(packet_id.to_string());
        }
        Ok(())
    }

    async fn mark_polled(&self, sensor_id: &str, stamp: &PollStamp) -> Result<(), SensorsError> {
        let mut rows = self.sensors.write().await;
        let Some(r) = rows.iter_mut().find(|r| r.id == sensor_id) else {
            return Err(SensorsError::UnknownSensor(sensor_id.to_string()));
        };
        r.last_polled_at = Some(stamp.polled_at);
        if let Some(c) = stamp.cursor_at {
            r.cursor_at = Some(r.cursor_at.map_or(c, |held| held.max(c)));
        }
        Ok(())
    }

    async fn sweep(&self, before: DateTime<Utc>) -> Result<u64, SensorsError> {
        let mut rows = self.readings.write().await;
        let n = rows.len();
        rows.retain(|r| !(r.observed_at < before && r.packet_id.is_some()));
        Ok((n - rows.len()) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn input(id: &str) -> SensorInput {
        SensorInput {
            id: id.into(),
            source: "stripe".into(),
            credential: "stripe-restricted-read".into(),
            every_minutes: 15,
            opens: "receive-a-sponsorship".into(),
            subject_kind: "custom".into(),
            enabled: true,
        }
    }

    fn reading(id: &str, at: DateTime<Utc>) -> NewReading {
        NewReading {
            external_id: id.into(),
            observed_at: at,
            payload: serde_json::json!({"id": id}),
        }
    }

    #[tokio::test]
    async fn a_second_publish_inserts_nothing_and_keeps_the_cursor() {
        let repo = InMemorySensors::new();
        let out = repo.publish("acme", &[input("a")]).await.unwrap();
        assert_eq!((out.received, out.inserted), (1, 1));
        let now = boss_clock_client::wall_now();
        repo.mark_polled(
            "a",
            &PollStamp {
                polled_at: now,
                cursor_at: Some(now),
            },
        )
        .await
        .unwrap();
        let out = repo
            .publish("acme", &[input("a"), input("b")])
            .await
            .unwrap();
        assert_eq!((out.received, out.inserted), (2, 1));
        let rows = repo.list().await.unwrap();
        assert_eq!(rows[0].cursor_at, Some(now), "the row kept its cursor");
        assert_eq!(rows[1].id, "b");
    }

    #[tokio::test]
    async fn readings_are_insert_if_absent_and_the_first_stamp_wins() {
        let repo = InMemorySensors::new();
        repo.publish("acme", &[input("a")]).await.unwrap();
        let t = boss_clock_client::wall_now();
        let out = repo
            .record("a", &[reading("ch_1", t), reading("ch_2", t)])
            .await
            .unwrap();
        assert_eq!(out.inserted, 2);
        let out = repo
            .record("a", &[reading("ch_1", t), reading("ch_3", t)])
            .await
            .unwrap();
        assert_eq!((out.received, out.inserted), (2, 1));
        assert_eq!(repo.unstamped("a").await.unwrap().len(), 3);

        repo.stamp("a", "ch_1", "packet-1").await.unwrap();
        repo.stamp("a", "ch_1", "packet-9").await.unwrap();
        let owed = repo.unstamped("a").await.unwrap();
        assert_eq!(owed.len(), 2);
        let stamped = repo
            .readings()
            .await
            .into_iter()
            .find(|r| r.external_id == "ch_1")
            .unwrap();
        assert_eq!(stamped.packet_id.as_deref(), Some("packet-1"));

        assert!(matches!(
            repo.record("nope", &[reading("x", t)]).await,
            Err(SensorsError::UnknownSensor(_))
        ));
    }

    #[tokio::test]
    async fn the_cursor_is_monotonic_and_the_sweep_keeps_what_is_owed() {
        let repo = InMemorySensors::new();
        repo.publish("acme", &[input("a")]).await.unwrap();
        let now = boss_clock_client::wall_now();
        repo.mark_polled(
            "a",
            &PollStamp {
                polled_at: now,
                cursor_at: Some(now),
            },
        )
        .await
        .unwrap();
        repo.mark_polled(
            "a",
            &PollStamp {
                polled_at: now + Duration::minutes(15),
                cursor_at: Some(now - Duration::days(1)),
            },
        )
        .await
        .unwrap();
        let row = &repo.list().await.unwrap()[0];
        assert_eq!(row.cursor_at, Some(now), "never backwards");
        assert_eq!(row.last_polled_at, Some(now + Duration::minutes(15)));

        let old = now - Duration::days(100);
        repo.record("a", &[reading("done", old), reading("owed", old)])
            .await
            .unwrap();
        repo.stamp("a", "done", "p").await.unwrap();
        let deleted = repo.sweep(now - Duration::days(90)).await.unwrap();
        assert_eq!(deleted, 1);
        assert_eq!(repo.readings().await[0].external_id, "owed");
    }
}
