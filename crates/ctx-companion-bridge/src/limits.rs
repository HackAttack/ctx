use std::time::Duration;

use crate::BridgeError;

pub const MAX_CONTROL_BYTES: usize = 64 * 1024;
pub const MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_STDOUT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_STDERR_BYTES: usize = 256 * 1024;
pub const MAX_ARGUMENTS: usize = 256;
pub const MAX_ENVIRONMENT_ENTRIES: usize = 6;
pub const MAX_CONCURRENT_PROCESSES: usize = 4;
pub const MAX_ADMISSION_WAIT: Duration = Duration::from_secs(120);
pub const MAX_CAPTURED_WALL_TIME: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, Debug)]
pub struct LimitConfiguration {
    pub control_bytes: usize,
    pub input_bytes: usize,
    pub stdout_bytes: usize,
    pub stderr_bytes: usize,
    pub arguments: usize,
    pub environment_entries: usize,
    pub concurrent_processes: usize,
    /// Maximum wait for a bridge process slot. Applies to captured and
    /// interactive launches independently of child process lifetime.
    pub admission_wait: Duration,
    /// Maximum child lifetime for captured request/response launches only.
    /// Interactive launches run until child exit or explicit cancellation.
    pub captured_wall_time: Duration,
}

impl Default for LimitConfiguration {
    fn default() -> Self {
        Self {
            control_bytes: MAX_CONTROL_BYTES,
            input_bytes: MAX_INPUT_BYTES,
            stdout_bytes: MAX_STDOUT_BYTES,
            stderr_bytes: MAX_STDERR_BYTES,
            arguments: 128,
            environment_entries: MAX_ENVIRONMENT_ENTRIES,
            concurrent_processes: 2,
            admission_wait: Duration::from_secs(60),
            captured_wall_time: Duration::from_secs(60),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct BridgeLimits(LimitConfiguration);

impl BridgeLimits {
    pub fn new(configuration: LimitConfiguration) -> Result<Self, BridgeError> {
        validate_positive(
            configuration.control_bytes,
            MAX_CONTROL_BYTES,
            "control bytes",
        )?;
        validate_positive(configuration.input_bytes, MAX_INPUT_BYTES, "input bytes")?;
        validate_positive(configuration.stdout_bytes, MAX_STDOUT_BYTES, "stdout bytes")?;
        validate_positive(configuration.stderr_bytes, MAX_STDERR_BYTES, "stderr bytes")?;
        validate_positive(configuration.arguments, MAX_ARGUMENTS, "argument count")?;
        validate_positive(
            configuration.environment_entries,
            MAX_ENVIRONMENT_ENTRIES,
            "environment entry count",
        )?;
        validate_positive(
            configuration.concurrent_processes,
            MAX_CONCURRENT_PROCESSES,
            "process concurrency",
        )?;
        if configuration.admission_wait.is_zero()
            || configuration.admission_wait > MAX_ADMISSION_WAIT
        {
            return Err(BridgeError::Limit("admission wait"));
        }
        if configuration.captured_wall_time.is_zero()
            || configuration.captured_wall_time > MAX_CAPTURED_WALL_TIME
        {
            return Err(BridgeError::Limit("captured wall time"));
        }
        Ok(Self(configuration))
    }

    pub(crate) const fn configuration(self) -> LimitConfiguration {
        self.0
    }
}

fn validate_positive(value: usize, maximum: usize, label: &'static str) -> Result<(), BridgeError> {
    if value == 0 || value > maximum {
        Err(BridgeError::Limit(label))
    } else {
        Ok(())
    }
}
