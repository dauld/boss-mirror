//! Search errors.

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("storage: {0}")]
    Storage(String),
    #[error("bad request: {0}")]
    BadRequest(String),
}

impl SearchError {
    pub fn storage(e: impl std::fmt::Display) -> Self {
        SearchError::Storage(e.to_string())
    }
}
