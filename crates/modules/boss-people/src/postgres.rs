//! Postgres adapter for `PeopleRepository`.
//!
//! Queries `employees`, `employee_skills`, and `employee_certifications`
//! tables and assembles into `Employee` structs.

use std::sync::Arc;

use async_trait::async_trait;
use boss_classes_client::ClassesClient;
use boss_core::primitives::ClassRef;
use boss_locations_client::LocationsClient;
use sqlx::PgPool;

use crate::port::{PeopleError, PeopleRepository};
use crate::types::*;

pub struct PgPeople {
    pool: PgPool,
    /// Optional Class registry client. When present, every write
    /// validates the closed-set attributes of `Employee` (`role`,
    /// `department`, `employment_type`, `status`) against
    /// `class_exists("employee", code)` before the row hits the DB.
    /// `skill_level` is a numeric range (1..=5), not a closed enum,
    /// so it stays on its CHECK and is not a Class registry
    /// candidate.
    classes: Option<Arc<dyn ClassesClient>>,
    /// Optional Locations registry client. When present, every write
    /// validates `location` against `location_exists(id)`.
    locations: Option<Arc<dyn LocationsClient>>,
}

impl PgPeople {
    /// Construct a PgPeople with no registry clients wired. Used by
    /// in-memory / test paths; production binaries always wire both
    /// clients via `with_registries`.
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            classes: None,
            locations: None,
        }
    }

    /// Construct a PgPeople wired to both registries. Every write
    /// validates closed-set Class attributes (`role`, `department`,
    /// `employment_type`, `status`) and the Location id before
    /// committing.
    pub fn with_registries(
        pool: PgPool,
        classes: Arc<dyn ClassesClient>,
        locations: Arc<dyn LocationsClient>,
    ) -> Self {
        Self {
            pool,
            classes: Some(classes),
            locations: Some(locations),
        }
    }

    /// Reject writes whose `role` doesn't resolve to an active Class.
    /// No-op when no `classes` client is configured.
    async fn validate_role(&self, role_code: &str) -> Result<(), PeopleError> {
        self.validate_employee_class("role", role_code).await
    }

    /// Reject writes whose `department` doesn't resolve to an active
    /// Class. No-op when no `classes` client is configured.
    async fn validate_department(&self, department_code: &str) -> Result<(), PeopleError> {
        self.validate_employee_class("department", department_code)
            .await
    }

    /// Reject writes whose `employment_type` doesn't resolve to an
    /// active Class. No-op when no `classes` client is configured.
    async fn validate_employment_type(&self, code: &str) -> Result<(), PeopleError> {
        self.validate_employee_class("employment_type", code).await
    }

    /// Reject writes whose `status` doesn't resolve to an active
    /// Class. No-op when no `classes` client is configured.
    async fn validate_status(&self, code: &str) -> Result<(), PeopleError> {
        self.validate_employee_class("status", code).await
    }

    async fn validate_employee_class(
        &self,
        attribute: &str,
        code: &str,
    ) -> Result<(), PeopleError> {
        let Some(classes) = &self.classes else {
            return Ok(());
        };
        let class_ref = ClassRef::new("employee", code);
        let exists = classes.class_exists(&class_ref).await.map_err(|e| {
            // Upstream service failure — surface as Storage so the
            // caller's retry/error UX matches a DB hiccup.
            PeopleError::Storage(format!("classes registry: {e}"))
        })?;
        if !exists {
            return Err(PeopleError::Conflict(format!(
                "{attribute} `{code}` is not an active Class in the registry"
            )));
        }
        Ok(())
    }

    /// Reject writes whose `location` doesn't resolve to an active
    /// Location id in the registry. No-op when no `locations` client
    /// is configured.
    async fn validate_location(&self, location_id: &str) -> Result<(), PeopleError> {
        let Some(locations) = &self.locations else {
            return Ok(());
        };
        let exists = locations
            .location_exists(location_id)
            .await
            .map_err(|e| PeopleError::Storage(format!("locations registry: {e}")))?;
        if !exists {
            return Err(PeopleError::Conflict(format!(
                "location `{location_id}` is not an active Location in the registry"
            )));
        }
        Ok(())
    }
}

#[async_trait]
impl PeopleRepository for PgPeople {
    async fn all_employees(&self) -> Result<Vec<Employee>, PeopleError> {
        let rows: Vec<EmployeeRow> = sqlx::query_as("SELECT * FROM employees ORDER BY id")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| PeopleError::Storage(e.to_string()))?;

        let mut employees = Vec::with_capacity(rows.len());
        for row in rows {
            let emp = self.assemble(row).await?;
            employees.push(emp);
        }
        Ok(employees)
    }

    async fn employee_by_id(&self, id: &str) -> Result<Option<Employee>, PeopleError> {
        let row: Option<EmployeeRow> = sqlx::query_as("SELECT * FROM employees WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| PeopleError::Storage(e.to_string()))?;

        match row {
            Some(r) => Ok(Some(self.assemble(r).await?)),
            None => Ok(None),
        }
    }

    async fn create_employee_at(
        &self,
        emp: &Employee,
        now: chrono::DateTime<chrono::Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<String, PeopleError> {
        // Registry validation runs before the transaction so a
        // mis-typed code doesn't waste a Postgres connection.
        // Identity-first: validate only the descriptive fields that are
        // present. An id-only employee record carries none of these yet;
        // each is validated against its Class registry once assigned.
        if let Some(role) = &emp.role {
            self.validate_role(role).await?;
        }
        if let Some(department) = &emp.department {
            self.validate_department(department).await?;
        }
        if let Some(employment_type) = &emp.employment_type {
            self.validate_employment_type(&to_kebab(employment_type))
                .await?;
        }
        if let Some(status) = &emp.status {
            self.validate_status(&to_kebab(status)).await?;
        }
        if let Some(location) = &emp.location {
            self.validate_location(location).await?;
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| PeopleError::Storage(e.to_string()))?;
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM employees WHERE id = $1)")
                .bind(&emp.id)
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| PeopleError::Storage(e.to_string()))?;
        // Identity write-through (subject-model R1, Q1): same tx as
        // the domain row.
        boss_subject_kinds::subjects::record_subject_in_tx(
            &mut tx,
            "employee",
            &emp.id,
            emp.name.as_deref(),
        )
        .await
        .map_err(PeopleError::Storage)?;
        if exists {
            return Err(PeopleError::Conflict(format!(
                "employee {} already exists",
                emp.id
            )));
        }
        upsert_employee_row(&mut tx, emp, now).await?;
        insert_employee_satellites(&mut tx, emp).await?;
        // OUTBOX (phase 2): the created event (full row state)
        // records with the rows.
        let event = stamp.event(
            crate::events::EMPLOYEE_CREATED,
            serde_json::to_value(emp).unwrap_or_default(),
        );
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(PeopleError::Storage)?;
        tx.commit()
            .await
            .map_err(|e| PeopleError::Storage(e.to_string()))?;
        Ok(emp.id.clone())
    }

    async fn update_employee_at(
        &self,
        id: &str,
        emp: &Employee,
        now: chrono::DateTime<chrono::Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), PeopleError> {
        // Identity-first: validate only the descriptive fields that are
        // present. An id-only employee record carries none of these yet;
        // each is validated against its Class registry once assigned.
        if let Some(role) = &emp.role {
            self.validate_role(role).await?;
        }
        if let Some(department) = &emp.department {
            self.validate_department(department).await?;
        }
        if let Some(employment_type) = &emp.employment_type {
            self.validate_employment_type(&to_kebab(employment_type))
                .await?;
        }
        if let Some(status) = &emp.status {
            self.validate_status(&to_kebab(status)).await?;
        }
        if let Some(location) = &emp.location {
            self.validate_location(location).await?;
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| PeopleError::Storage(e.to_string()))?;
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM employees WHERE id = $1)")
                .bind(id)
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| PeopleError::Storage(e.to_string()))?;
        if !exists {
            return Err(PeopleError::NotFound(id.to_string()));
        }
        // UPSERT preserves `created_at` (load-bearing for rebuild
        // equality). Satellites still get full replacement since
        // skills + certifications have no per-row id we can UPSERT
        // by — drift would accumulate otherwise.
        upsert_employee_row(&mut tx, emp, now).await?;
        sqlx::query("DELETE FROM employee_skills WHERE employee_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| PeopleError::Storage(e.to_string()))?;
        sqlx::query("DELETE FROM employee_certifications WHERE employee_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| PeopleError::Storage(e.to_string()))?;
        insert_employee_satellites(&mut tx, emp).await?;
        // OUTBOX (phase 2): the updated event (full row state)
        // records with the rows.
        let event = stamp.event(
            crate::events::EMPLOYEE_UPDATED,
            serde_json::to_value(emp).unwrap_or_default(),
        );
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(PeopleError::Storage)?;
        tx.commit()
            .await
            .map_err(|e| PeopleError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn delete_employee_at(
        &self,
        id: &str,
        now: chrono::DateTime<chrono::Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), PeopleError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| PeopleError::Storage(e.to_string()))?;
        sqlx::query("DELETE FROM employee_skills WHERE employee_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| PeopleError::Storage(e.to_string()))?;
        sqlx::query("DELETE FROM employee_certifications WHERE employee_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| PeopleError::Storage(e.to_string()))?;
        let result = sqlx::query("DELETE FROM employees WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| PeopleError::Storage(e.to_string()))?;
        if result.rows_affected() == 0 {
            return Err(PeopleError::NotFound(id.to_string()));
        }
        // OUTBOX (phase 2): the deleted event records only after the
        // row actually deleted (NotFound above returns pre-recording).
        let event = stamp.event(
            crate::events::EMPLOYEE_DELETED,
            serde_json::json!({ "id": id, "deleted_at": now }),
        );
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(PeopleError::Storage)?;
        tx.commit()
            .await
            .map_err(|e| PeopleError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn direct_reports(&self, manager_id: &str) -> Result<Vec<Employee>, PeopleError> {
        let rows: Vec<EmployeeRow> =
            sqlx::query_as("SELECT * FROM employees WHERE manager_id = $1 ORDER BY name")
                .bind(manager_id)
                .fetch_all(&self.pool)
                .await
                .map_err(|e| PeopleError::Storage(e.to_string()))?;

        let mut employees = Vec::with_capacity(rows.len());
        for row in rows {
            let emp = self.assemble(row).await?;
            employees.push(emp);
        }
        Ok(employees)
    }
}

impl PgPeople {
    async fn assemble(&self, row: EmployeeRow) -> Result<Employee, PeopleError> {
        let (skills, certifications) = tokio::try_join!(
            self.fetch_skills(&row.id),
            self.fetch_certifications(&row.id),
        )?;

        Ok(row.into_employee(skills, certifications))
    }

    async fn fetch_skills(&self, employee_id: &str) -> Result<Vec<String>, PeopleError> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT skill FROM employee_skills WHERE employee_id = $1 ORDER BY skill",
        )
        .bind(employee_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| PeopleError::Storage(e.to_string()))?;

        Ok(rows.into_iter().map(|(s,)| s).collect())
    }

    async fn fetch_certifications(
        &self,
        employee_id: &str,
    ) -> Result<Vec<Certification>, PeopleError> {
        let rows: Vec<CertRow> = sqlx::query_as(
            "SELECT name, issuing_body, issued_on, expires_on FROM employee_certifications WHERE employee_id = $1 ORDER BY issued_on",
        )
        .bind(employee_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| PeopleError::Storage(e.to_string()))?;

        Ok(rows
            .into_iter()
            .map(|r| Certification {
                name: r.name,
                issuing_body: r.issuing_body,
                issued_on: r.issued_on,
                expires_on: r.expires_on,
            })
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Write helpers
// ---------------------------------------------------------------------------

pub(crate) fn to_kebab<T: serde::Serialize>(val: &T) -> String {
    serde_json::to_value(val)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

pub(crate) async fn upsert_employee_row(
    tx: &mut sqlx::PgConnection,
    e: &Employee,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), PeopleError> {
    sqlx::query(
        "INSERT INTO employees (id, name, email, role, department, skill_level, \
         hire_date, location, manager_id, employment_type, status, annual_salary_cents, \
         created_at, updated_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$13) \
         ON CONFLICT (id) DO UPDATE SET \
            name = EXCLUDED.name, \
            email = EXCLUDED.email, \
            role = EXCLUDED.role, \
            department = EXCLUDED.department, \
            skill_level = EXCLUDED.skill_level, \
            hire_date = EXCLUDED.hire_date, \
            location = EXCLUDED.location, \
            manager_id = EXCLUDED.manager_id, \
            employment_type = EXCLUDED.employment_type, \
            status = EXCLUDED.status, \
            annual_salary_cents = EXCLUDED.annual_salary_cents, \
            updated_at = EXCLUDED.updated_at",
    )
    .bind(&e.id)
    .bind(&e.name)
    .bind(&e.email)
    .bind(&e.role)
    .bind(&e.department)
    .bind(e.skill_level.map(|v| v as i16))
    .bind(e.hire_date)
    .bind(&e.location)
    .bind(&e.manager_id)
    .bind(e.employment_type.as_ref().map(to_kebab))
    .bind(e.status.as_ref().map(to_kebab))
    .bind(e.annual_salary_cents)
    .bind(now)
    .execute(&mut *tx)
    .await
    .map_err(|e| PeopleError::Storage(e.to_string()))?;
    Ok(())
}

pub(crate) async fn insert_employee_satellites(
    tx: &mut sqlx::PgConnection,
    e: &Employee,
) -> Result<(), PeopleError> {
    for skill in &e.skills {
        sqlx::query("INSERT INTO employee_skills (employee_id, skill) VALUES ($1, $2)")
            .bind(&e.id)
            .bind(skill)
            .execute(&mut *tx)
            .await
            .map_err(|err| PeopleError::Storage(err.to_string()))?;
    }
    for cert in &e.certifications {
        sqlx::query(
            "INSERT INTO employee_certifications (employee_id, name, issuing_body, issued_on, expires_on) \
             VALUES ($1,$2,$3,$4,$5)",
        )
        .bind(&e.id)
        .bind(&cert.name)
        .bind(&cert.issuing_body)
        .bind(cert.issued_on)
        .bind(cert.expires_on)
        .execute(&mut *tx)
        .await
        .map_err(|err| PeopleError::Storage(err.to_string()))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Row types
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct EmployeeRow {
    id: String,
    // Identity-first: descriptive columns are nullable (see Employee).
    name: Option<String>,
    email: Option<String>,
    role: Option<String>,
    department: Option<String>,
    skill_level: Option<i16>,
    hire_date: Option<chrono::NaiveDate>,
    location: Option<String>,
    manager_id: Option<String>,
    employment_type: Option<String>,
    status: Option<String>,
    annual_salary_cents: Option<i64>,
    // No `created_at` / `updated_at`: `Employee` carries neither, and
    // `FromRow` ignores columns it has no field for.
}

impl EmployeeRow {
    fn into_employee(self, skills: Vec<String>, certifications: Vec<Certification>) -> Employee {
        Employee {
            id: self.id,
            name: self.name,
            email: self.email,
            role: self.role,
            department: self.department,
            skill_level: self.skill_level.map(|v| v as u8),
            skills,
            hire_date: self.hire_date,
            location: self.location,
            manager_id: self.manager_id,
            // Trust the column — write-time validation against the
            // Class registry catches invalid values before they land,
            // so the column is just a string here.
            employment_type: self.employment_type,
            status: self.status,
            certifications,
            annual_salary_cents: self.annual_salary_cents,
        }
    }
}

#[derive(sqlx::FromRow)]
struct CertRow {
    name: String,
    issuing_body: String,
    issued_on: chrono::NaiveDate,
    expires_on: Option<chrono::NaiveDate>,
}

// Employee taxonomies (employment_type, status, …) carry no Rust-side
// closed set: every value is validated at write time against the Class
// registry (see PgPeople::validate_employee_class).
