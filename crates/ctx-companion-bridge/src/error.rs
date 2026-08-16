use std::io;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("managed companion slot is invalid: {0}")]
    InvalidSlot(&'static str),
    #[error("managed companion filesystem validation failed: {context}")]
    Filesystem {
        context: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("managed companion verification refused the pair: {0}")]
    Verification(String),
    #[error("managed companion request exceeds the {0} limit")]
    Limit(&'static str),
    #[error("managed companion data root must be an absolute native path")]
    InvalidDataRoot,
    #[error("managed companion request deadline expired before spawn")]
    QueueTimeout,
    #[error("managed companion launch was cancelled before spawn")]
    CancelledBeforeSpawn,
    #[error("managed companion process could not be started: {0}")]
    Spawn(#[source] io::Error),
    #[error("managed companion transport failed: {0}")]
    Transport(#[source] io::Error),
    #[error("managed companion transport worker failed")]
    WorkerFailed,
    #[error("managed companion transport is unsupported on this platform")]
    UnsupportedPlatform,
}

impl BridgeError {
    pub(crate) fn filesystem(context: &'static str, source: io::Error) -> Self {
        Self::Filesystem { context, source }
    }
}
