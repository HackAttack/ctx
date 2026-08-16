//! Neutral, fail-closed launch and byte transport for a fixed managed companion.
//!
//! Production launch always resolves the running Core executable from
//! `<root>/bin/ctx[.exe]`, verifies the detached signed V1 pair envelope and
//! retained rollback state under `<root>/share/ctx`, and executes only
//! `<root>/libexec/ctx-pro[.exe]`. This crate owns no companion protocol or
//! product semantics.

mod environment;
mod error;
mod identity;
mod limits;
mod process;
mod request;
mod slot;
mod verifier;

use std::{
    path::Path,
    sync::{Condvar, Mutex},
    time::{Duration, Instant},
};

pub use environment::{CompanionEnvironment, EnvironmentKey};
pub use error::BridgeError;
pub use identity::{FileIdentity, PairIdentity, Sha256Digest};
pub use limits::{
    BridgeLimits, LimitConfiguration, MAX_ADMISSION_WAIT, MAX_ARGUMENTS, MAX_CAPTURED_WALL_TIME,
    MAX_CONCURRENT_PROCESSES, MAX_CONTROL_BYTES, MAX_ENVIRONMENT_ENTRIES, MAX_INPUT_BYTES,
    MAX_STDERR_BYTES, MAX_STDOUT_BYTES,
};
pub use process::{ExitClass, TerminationReason};
pub use request::{CancellationToken, CapturedCompanionRequest, CompanionRequest};
pub use verifier::{
    verify_signed_managed_pair_envelope, CompatibilityIdentity, CoreBuildIdentity,
    ManagedPairExpectations, ReleaseChannel, SignedManagedPairComponentIdentity,
    SignedManagedPairIdentity, SignedManagedPairTarget, MANAGED_PAIR_ENVELOPE_FILENAME,
    MANAGED_PAIR_STATE_FILENAME,
};

use process::{ProcessExit, ProcessOutput};
use verifier::{PairVerifier, ProductionVerifier};

#[derive(Debug)]
pub struct CompanionOutput {
    pair_identity: PairIdentity,
    exit: ExitClass,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

#[derive(Debug)]
pub struct CompanionExit {
    pair_identity: PairIdentity,
    exit: ExitClass,
}

impl CompanionExit {
    pub const fn pair_identity(&self) -> &PairIdentity {
        &self.pair_identity
    }

    pub const fn exit_class(&self) -> ExitClass {
        self.exit
    }
}

impl CompanionOutput {
    pub const fn pair_identity(&self) -> &PairIdentity {
        &self.pair_identity
    }

    pub const fn exit_class(&self) -> ExitClass {
        self.exit
    }

    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    pub fn stderr(&self) -> &[u8] {
        &self.stderr
    }

    pub const fn stdout_truncated(&self) -> bool {
        self.stdout_truncated
    }

    pub const fn stderr_truncated(&self) -> bool {
        self.stderr_truncated
    }
}

pub struct CompanionBridge {
    limits: LimitConfiguration,
    gate: ConcurrencyGate,
}

impl CompanionBridge {
    pub fn new(limits: BridgeLimits) -> Self {
        let limits = limits.configuration();
        Self {
            gate: ConcurrencyGate::new(limits.concurrent_processes),
            limits,
        }
    }

    /// Launches the companion paired with the currently running Core process.
    ///
    /// No executable path, current directory, release authority, rollback
    /// location, or arbitrary environment key is accepted from the caller.
    pub fn launch_captured(
        &self,
        expectations: &ManagedPairExpectations,
        request: CapturedCompanionRequest,
        cancellation: &CancellationToken,
    ) -> Result<CompanionOutput, BridgeError> {
        let launcher = std::env::current_exe()
            .map_err(|error| BridgeError::filesystem("resolve current Core executable", error))?;
        let verifier = ProductionVerifier::new(expectations);
        self.launch_captured_at_with_channel(
            &launcher,
            request,
            cancellation,
            &verifier,
            Some(expectations.channel()),
        )
    }

    /// Launches the verified companion with the caller's existing standard
    /// streams inherited directly. This mode preserves terminal identity,
    /// backpressure, and interactive request/response ordering. Once admitted,
    /// the child runs until it exits or cancellation terminates its process
    /// tree; captured request wall limits do not apply.
    pub fn launch_streaming(
        &self,
        expectations: &ManagedPairExpectations,
        request: CompanionRequest,
        cancellation: &CancellationToken,
    ) -> Result<CompanionExit, BridgeError> {
        let launcher = std::env::current_exe()
            .map_err(|error| BridgeError::filesystem("resolve current Core executable", error))?;
        let verifier = ProductionVerifier::new(expectations);
        self.launch_streaming_at_with_channel(
            &launcher,
            request,
            cancellation,
            &verifier,
            Some(expectations.channel()),
        )
    }

    #[cfg(test)]
    fn launch_captured_at(
        &self,
        launcher: &Path,
        request: CapturedCompanionRequest,
        cancellation: &CancellationToken,
        verifier: &dyn PairVerifier,
    ) -> Result<CompanionOutput, BridgeError> {
        self.launch_captured_at_with_channel(launcher, request, cancellation, verifier, None)
    }

    fn launch_captured_at_with_channel(
        &self,
        launcher: &Path,
        request: CapturedCompanionRequest,
        cancellation: &CancellationToken,
        verifier: &dyn PairVerifier,
        verified_channel: Option<ReleaseChannel>,
    ) -> Result<CompanionOutput, BridgeError> {
        request.validate(self.limits)?;
        let (pair, _permit) = self.prepare_launch(launcher, cancellation, verifier)?;
        let ProcessOutput {
            exit,
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
        } = process::run_captured(
            &pair.execution,
            request,
            cancellation,
            self.limits,
            verified_channel,
        )?;
        Ok(CompanionOutput {
            pair_identity: pair.identity,
            exit,
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
        })
    }

    #[cfg(test)]
    fn launch_streaming_at(
        &self,
        launcher: &Path,
        request: CompanionRequest,
        cancellation: &CancellationToken,
        verifier: &dyn PairVerifier,
    ) -> Result<CompanionExit, BridgeError> {
        self.launch_streaming_at_with_channel(launcher, request, cancellation, verifier, None)
    }

    fn launch_streaming_at_with_channel(
        &self,
        launcher: &Path,
        request: CompanionRequest,
        cancellation: &CancellationToken,
        verifier: &dyn PairVerifier,
        verified_channel: Option<ReleaseChannel>,
    ) -> Result<CompanionExit, BridgeError> {
        request.validate(self.limits)?;
        let (pair, _permit) = self.prepare_launch(launcher, cancellation, verifier)?;
        let ProcessExit { exit } =
            process::run_streaming(&pair.execution, request, cancellation, verified_channel)?;
        Ok(CompanionExit {
            pair_identity: pair.identity,
            exit,
        })
    }

    fn prepare_launch<'a>(
        &'a self,
        launcher: &Path,
        cancellation: &CancellationToken,
        verifier: &dyn PairVerifier,
    ) -> Result<(slot::PreparedPair, ConcurrencyPermit<'a>), BridgeError> {
        let permit = self
            .gate
            .acquire(cancellation, self.limits.admission_wait)?;
        if cancellation.is_cancelled() {
            return Err(BridgeError::CancelledBeforeSpawn);
        }
        let pair = slot::prepare(launcher)?;
        verifier.verify(&pair)?;
        if cancellation.is_cancelled() {
            return Err(BridgeError::CancelledBeforeSpawn);
        }
        Ok((pair, permit))
    }
}

impl Default for CompanionBridge {
    fn default() -> Self {
        Self::new(BridgeLimits::default())
    }
}

struct ConcurrencyGate {
    maximum: usize,
    active: Mutex<usize>,
    changed: Condvar,
}

impl ConcurrencyGate {
    const fn new(maximum: usize) -> Self {
        Self {
            maximum,
            active: Mutex::new(0),
            changed: Condvar::new(),
        }
    }

    fn acquire(
        &self,
        cancellation: &CancellationToken,
        timeout: Duration,
    ) -> Result<ConcurrencyPermit<'_>, BridgeError> {
        let started = Instant::now();
        let mut active = self.active.lock().map_err(|_| BridgeError::WorkerFailed)?;
        while *active >= self.maximum {
            if cancellation.is_cancelled() {
                return Err(BridgeError::CancelledBeforeSpawn);
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(BridgeError::QueueTimeout);
            }
            let wait = Duration::from_millis(10).min(remaining);
            let (next, _) = self
                .changed
                .wait_timeout(active, wait)
                .map_err(|_| BridgeError::WorkerFailed)?;
            active = next;
        }
        *active += 1;
        Ok(ConcurrencyPermit { gate: self })
    }
}

struct ConcurrencyPermit<'a> {
    gate: &'a ConcurrencyGate,
}

impl Drop for ConcurrencyPermit<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.gate.active.lock() {
            *active = active.saturating_sub(1);
            self.gate.changed.notify_one();
        }
    }
}

#[cfg(test)]
mod tests;
