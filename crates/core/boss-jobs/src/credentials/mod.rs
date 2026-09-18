//! Credentials registry — KNOWLEDGE about credentials as registry
//! data (packet 7ee101aa, second leg).
//!
//! Possession of a credential lives in Secrets and token files, where
//! it always has. What this registry holds is everything else: kind,
//! issuer, principal, scopes, where the value is stored, who consumes
//! it, and its rotation posture — so that "what can this token do?"
//! is a lookup, not an experiment. On 2026-09-02 an admin token went
//! half-used for days because its scope lived in nobody's head but
//! David's, and a 403 was how an agent learned /user was out of
//! scope. That question is now `GET /api/credentials/{id}`.
//!
//! THE ONE RULE: a row carries LOCATIONS, never contents. No secret
//! value ever enters this module, its table, or its HTTP surface.
//!
//! Readers: `boss credential list` (boss-cli), the weekly
//! forge-token-audit (compares live forge tokens against rows of kind
//! `forgejo-access-token`, both directions), and any agent asking a
//! scope question. Writes are the instance's DECLARATION and the
//! rotation path (`rotated_at`/`notes`); the mutability decision is
//! written down in
//! `infra/postgres/schema/202609031700-credentials-are-registry-rows.sql`.
//!
//! WHO AUTHORS A ROW (backlog ee368d0c, 2026-09-18). Until that car
//! the rows were authored by migrations, so every OSS install booted
//! with one operator's forge, Stripe and Cloudflare credential ids —
//! instance data in the platform schema. Now the instance declares
//! them: `seeds/credentials.toml` in the tenant contract, published by
//! `boss tenant publish` through `POST /api/credentials/batch`
//! (insert-if-absent by id, one `credential.declared` fact per row
//! inserted — the classes/locations/agents doors' shape). The
//! migration 20260918-instance-data-leaves-the-platform-schema removes
//! the rows the historical migrations seeded where nothing references
//! them, so a fresh database holds no credential row until an instance
//! declares one. Values never enter: the declaration shape has no field
//! a value could ride in.
//!
//! The rotation path's write is the other HTTP write on this surface —
//! `POST /api/credentials/{id}/rotation/{phase}` — because the broker
//! is a dispatcher handler and handlers own no database: each phase
//! records a `credential.minted` / `.installed` / `.verified` /
//! `.revoked` event (declared in `event_kinds`, source `jobs`), and
//! the install phase stamps `rotated_at` in the same transaction.
//!
//! Hexagonal: port trait + Pg adapter + in-memory adapter + HTTP
//! door, the same shape as `delivery` and `cadence`.

pub mod http;
pub mod in_memory;
pub mod port;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod seed;
pub mod types;

pub use in_memory::InMemoryCredentials;
pub use port::{CredentialsError, CredentialsRegistry, declared_event};
#[cfg(feature = "postgres")]
pub use postgres::PgCredentials;
pub use seed::{load_credentials_toml, parse_credentials_toml};
pub use types::{
    CredentialBatch, CredentialInput, CredentialRow, CredentialsBatchOutcome, RotationPhase,
    validate_credential,
};
