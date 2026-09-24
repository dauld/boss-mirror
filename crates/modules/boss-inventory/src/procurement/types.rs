//! Domain types for the Vendor CRM — contacts, interactions,
//! account-team, contracts. Mirrors the account-side CRM shape
//! intentionally so the frontend can share rendering components.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Vendor contacts
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VendorContact {
    pub id: String,
    pub vendor_id: String,
    pub name: String,
    pub role: String,
    pub email: String,
    pub phone: Option<String>,
    pub territory: Option<String>,
    /// Part categories the contact specializes in. Kept as a free-form
    /// string array — we'll tighten the vocabulary once we see real
    /// vendor data. Maps to JSONB `specialties` in Postgres.
    #[serde(default)]
    pub specialties: Vec<String>,
    #[serde(default)]
    pub is_primary: bool,
    pub relationship_start: Option<NaiveDate>,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewVendorContact {
    pub id: String,
    pub vendor_id: String,
    pub name: String,
    pub role: String,
    pub email: String,
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default)]
    pub territory: Option<String>,
    #[serde(default)]
    pub specialties: Vec<String>,
    #[serde(default)]
    pub is_primary: bool,
    #[serde(default)]
    pub relationship_start: Option<NaiveDate>,
    #[serde(default)]
    pub notes: Option<String>,
}

// ---------------------------------------------------------------------------
// Vendor interactions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionCommitment {
    pub summary: String,
    #[serde(default)]
    pub due_by: Option<NaiveDate>,
    #[serde(default)]
    pub linked_po_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VendorInteraction {
    pub id: String,
    pub vendor_id: String,
    pub vendor_contact_id: Option<String>,
    pub actor_id: String,
    pub kind: String,
    pub body: String,
    #[serde(default)]
    pub commitments: Vec<InteractionCommitment>,
    pub linked_po_id: Option<String>,
    pub linked_part_sku: Option<String>,
    pub linked_job_id: Option<String>,
    pub occurred_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewVendorInteraction {
    pub id: String,
    pub vendor_id: String,
    #[serde(default)]
    pub vendor_contact_id: Option<String>,
    pub actor_id: String,
    pub kind: String,
    pub body: String,
    #[serde(default)]
    pub commitments: Vec<InteractionCommitment>,
    #[serde(default)]
    pub linked_po_id: Option<String>,
    #[serde(default)]
    pub linked_part_sku: Option<String>,
    #[serde(default)]
    pub linked_job_id: Option<String>,
    /// Defaults to `now()` at write time if absent.
    #[serde(default)]
    pub occurred_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Vendor account team
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VendorAccountTeamMember {
    pub id: String,
    pub vendor_id: String,
    pub employee_id: String,
    pub role: String,
    pub assigned_on: NaiveDate,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewVendorAccountTeamMember {
    pub id: String,
    pub vendor_id: String,
    pub employee_id: String,
    pub role: String,
    #[serde(default)]
    pub assigned_on: Option<NaiveDate>,
    #[serde(default)]
    pub notes: Option<String>,
}

// ---------------------------------------------------------------------------
// Vendor contracts
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VendorContract {
    pub id: String,
    pub vendor_id: String,
    pub kind: String,
    pub title: String,
    pub effective_on: NaiveDate,
    pub expires_on: Option<NaiveDate>,
    #[serde(default)]
    pub auto_renew: bool,
    /// Kind-discriminated negotiated terms. Shape varies per kind;
    /// the adapter validates when a `kind`-specific schema is added.
    #[serde(default)]
    pub terms: serde_json::Value,
    pub document_uri: Option<String>,
    pub status: String,
    pub signed_by_employee_id: Option<String>,
    pub signed_at: Option<DateTime<Utc>>,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewVendorContract {
    pub id: String,
    pub vendor_id: String,
    pub kind: String,
    pub title: String,
    pub effective_on: NaiveDate,
    #[serde(default)]
    pub expires_on: Option<NaiveDate>,
    #[serde(default)]
    pub auto_renew: bool,
    #[serde(default)]
    pub terms: serde_json::Value,
    #[serde(default)]
    pub document_uri: Option<String>,
    /// Defaults to `"draft"` if absent.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub signed_by_employee_id: Option<String>,
    #[serde(default)]
    pub signed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub notes: Option<String>,
}
