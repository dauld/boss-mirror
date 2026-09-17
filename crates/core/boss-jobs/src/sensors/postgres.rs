//! Postgres adapter for `Sensors` over the two tables of
//! 20260917041600-sensors-are-declared-and-their-readings-are-kept.sql.
//! Every write is one statement whose idempotence is the table's own
//! (`ON CONFLICT DO NOTHING`, `WHERE packet_id IS NULL`, `GREATEST`),
//! so a redelivered poll and a first poll run the same SQL.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};

use super::port::{Sensors, SensorsError};
use super::types::{BatchOutcome, NewReading, PollStamp, Reading, SensorInput, SensorRow};

const SENSOR_COLUMNS: &str = "id, source, credential, every_minutes, opens_kind, subject_kind, \
                              enabled, tenant_id, published_at, last_polled_at, cursor_at";

pub struct PgSensors {
    pool: PgPool,
}

impl PgSensors {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn storage(e: sqlx::Error) -> SensorsError {
    SensorsError::Storage(e.to_string())
}

fn sensor_of(row: &sqlx::postgres::PgRow) -> Result<SensorRow, SensorsError> {
    Ok(SensorRow {
        id: row.try_get("id").map_err(storage)?,
        source: row.try_get("source").map_err(storage)?,
        credential: row.try_get("credential").map_err(storage)?,
        every_minutes: row.try_get("every_minutes").map_err(storage)?,
        opens_kind: row.try_get("opens_kind").map_err(storage)?,
        subject_kind: row.try_get("subject_kind").map_err(storage)?,
        enabled: row.try_get("enabled").map_err(storage)?,
        tenant_id: row.try_get("tenant_id").map_err(storage)?,
        published_at: row.try_get("published_at").map_err(storage)?,
        last_polled_at: row.try_get("last_polled_at").map_err(storage)?,
        cursor_at: row.try_get("cursor_at").map_err(storage)?,
    })
}

fn reading_of(row: &sqlx::postgres::PgRow) -> Result<Reading, SensorsError> {
    Ok(Reading {
        sensor_id: row.try_get("sensor_id").map_err(storage)?,
        external_id: row.try_get("external_id").map_err(storage)?,
        observed_at: row.try_get("observed_at").map_err(storage)?,
        payload: row.try_get("payload").map_err(storage)?,
        packet_id: row.try_get("packet_id").map_err(storage)?,
    })
}

#[async_trait]
impl Sensors for PgSensors {
    async fn list(&self) -> Result<Vec<SensorRow>, SensorsError> {
        let rows = sqlx::query(&format!("SELECT {SENSOR_COLUMNS} FROM sensors ORDER BY id"))
            .fetch_all(&self.pool)
            .await
            .map_err(storage)?;
        rows.iter().map(sensor_of).collect()
    }

    async fn publish(
        &self,
        tenant_id: &str,
        sensors: &[SensorInput],
    ) -> Result<BatchOutcome, SensorsError> {
        let mut inserted = 0;
        for s in sensors {
            let n = sqlx::query(
                "INSERT INTO sensors \
                 (id, source, credential, every_minutes, opens_kind, subject_kind, enabled, tenant_id) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(&s.id)
            .bind(&s.source)
            .bind(&s.credential)
            .bind(s.every_minutes)
            .bind(&s.opens)
            .bind(&s.subject_kind)
            .bind(s.enabled)
            .bind(tenant_id)
            .execute(&self.pool)
            .await
            .map_err(storage)?
            .rows_affected();
            inserted += n as usize;
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
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sensors WHERE id = $1)")
            .bind(sensor_id)
            .fetch_one(&self.pool)
            .await
            .map_err(storage)?;
        if !exists {
            return Err(SensorsError::UnknownSensor(sensor_id.to_string()));
        }
        let mut inserted = 0;
        for r in readings {
            let n = sqlx::query(
                "INSERT INTO sensor_readings (sensor_id, external_id, observed_at, payload) \
                 VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (sensor_id, external_id) DO NOTHING",
            )
            .bind(sensor_id)
            .bind(&r.external_id)
            .bind(r.observed_at)
            .bind(&r.payload)
            .execute(&self.pool)
            .await
            .map_err(storage)?
            .rows_affected();
            inserted += n as usize;
        }
        Ok(BatchOutcome {
            received: readings.len(),
            inserted,
        })
    }

    async fn unstamped(&self, sensor_id: &str) -> Result<Vec<Reading>, SensorsError> {
        let rows = sqlx::query(
            "SELECT sensor_id, external_id, observed_at, payload, packet_id \
             FROM sensor_readings WHERE sensor_id = $1 AND packet_id IS NULL \
             ORDER BY observed_at, external_id",
        )
        .bind(sensor_id)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        rows.iter().map(reading_of).collect()
    }

    async fn stamp(
        &self,
        sensor_id: &str,
        external_id: &str,
        packet_id: &str,
    ) -> Result<(), SensorsError> {
        // The first stamp wins: the WHERE keeps a second packet id off
        // a reading that already has one.
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM sensor_readings WHERE sensor_id = $1 AND external_id = $2)",
        )
        .bind(sensor_id)
        .bind(external_id)
        .fetch_one(&self.pool)
        .await
        .map_err(storage)?;
        if !exists {
            return Err(SensorsError::UnknownSensor(format!(
                "{sensor_id}/{external_id}"
            )));
        }
        sqlx::query(
            "UPDATE sensor_readings SET packet_id = $3 \
             WHERE sensor_id = $1 AND external_id = $2 AND packet_id IS NULL",
        )
        .bind(sensor_id)
        .bind(external_id)
        .bind(packet_id)
        .execute(&self.pool)
        .await
        .map_err(storage)?;
        Ok(())
    }

    async fn mark_polled(&self, sensor_id: &str, stamp: &PollStamp) -> Result<(), SensorsError> {
        let n = sqlx::query(
            "UPDATE sensors SET last_polled_at = $2, \
             cursor_at = GREATEST(cursor_at, $3) \
             WHERE id = $1",
        )
        .bind(sensor_id)
        .bind(stamp.polled_at)
        .bind(stamp.cursor_at)
        .execute(&self.pool)
        .await
        .map_err(storage)?
        .rows_affected();
        if n == 0 {
            return Err(SensorsError::UnknownSensor(sensor_id.to_string()));
        }
        Ok(())
    }

    async fn sweep(&self, before: DateTime<Utc>) -> Result<u64, SensorsError> {
        let res = sqlx::query(
            "DELETE FROM sensor_readings WHERE observed_at < $1 AND packet_id IS NOT NULL",
        )
        .bind(before)
        .execute(&self.pool)
        .await
        .map_err(storage)?;
        Ok(res.rows_affected())
    }
}
