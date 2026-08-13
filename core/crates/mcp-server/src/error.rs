use thiserror::Error;

use crate::protocol::{INTERNAL_ERROR, INVALID_PARAMS, METHOD_NOT_FOUND};

#[derive(Debug, Error)]
pub enum McpError {
    #[error(transparent)]
    Storage(#[from] notewise_storage::StorageError),

    #[error(transparent)]
    Graph(#[from] notewise_graph::GraphError),

    #[error("unknown tool '{0}'")]
    UnknownTool(String),

    #[error("invalid parameters: {0}")]
    InvalidParams(String),

    #[error("unsupported method '{0}'")]
    UnsupportedMethod(String),
}

pub type Result<T> = std::result::Result<T, McpError>;

impl McpError {
    /// JSON-RPC error code for this failure.
    pub fn code(&self) -> i32 {
        match self {
            McpError::UnknownTool(_) | McpError::UnsupportedMethod(_) => METHOD_NOT_FOUND,
            McpError::InvalidParams(_) => INVALID_PARAMS,
            // A missing record is the agent's mistake, not a server fault — reporting it as
            // invalid params lets the agent correct itself instead of retrying blindly.
            McpError::Storage(notewise_storage::StorageError::NotFound { .. }) => INVALID_PARAMS,
            McpError::Storage(_) | McpError::Graph(_) => INTERNAL_ERROR,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notewise_storage::{Id, StorageError};

    #[test]
    fn unknown_tools_and_methods_map_to_method_not_found() {
        assert_eq!(McpError::UnknownTool("x".into()).code(), METHOD_NOT_FOUND);
        assert_eq!(
            McpError::UnsupportedMethod("x".into()).code(),
            METHOD_NOT_FOUND
        );
    }

    #[test]
    fn a_missing_record_is_the_agents_mistake_not_ours() {
        let err = McpError::Storage(StorageError::not_found("Meeting", Id::new()));
        assert_eq!(
            err.code(),
            INVALID_PARAMS,
            "an agent given this code can correct its id rather than retrying"
        );
    }

    #[test]
    fn genuine_storage_failures_are_internal_errors() {
        let err = McpError::Storage(StorageError::Corrupt {
            column: "meetings.source",
            reason: "bad".into(),
        });
        assert_eq!(err.code(), INTERNAL_ERROR);
    }

    #[test]
    fn invalid_params_map_to_the_standard_code() {
        assert_eq!(McpError::InvalidParams("bad".into()).code(), INVALID_PARAMS);
    }
}
