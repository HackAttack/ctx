use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};

use super::{version::CtxBinaryVersion, ReleaseProcessPort};

const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const VERSION_PROBE_OUTPUT_LIMIT: usize = 4096;

pub(super) fn ctx_binary_version(
    process: &dyn ReleaseProcessPort,
    path: &Path,
) -> Result<CtxBinaryVersion> {
    let output = run_ctx_version_command(process, path)?;
    if !output.status.success() {
        return Err(anyhow!("{} --version failed", path.display()));
    }
    if output.truncated {
        return Err(anyhow!(
            "{} --version output exceeded {} bytes",
            path.display(),
            VERSION_PROBE_OUTPUT_LIMIT
        ));
    }
    CtxBinaryVersion::parse(&output.stdout)
        .with_context(|| format!("parse {} --version output", path.display()))
}

struct VersionCommandOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    truncated: bool,
}

fn run_ctx_version_command(
    process: &dyn ReleaseProcessPort,
    path: &Path,
) -> Result<VersionCommandOutput> {
    let mut command = Command::new(path);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    process.sanitize_release_authority_env(&mut command);
    let mut child = command
        .spawn()
        .with_context(|| format!("run {} --version", path.display()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("capture {} --version output", path.display()))?;
    let (output_tx, output_rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = output_tx.send(read_capped_output(stdout, VERSION_PROBE_OUTPUT_LIMIT));
    });
    let started = Instant::now();
    let mut status = None;
    let mut output = None;
    loop {
        if status.is_none() {
            status = child
                .try_wait()
                .with_context(|| format!("wait for {} --version", path.display()))?;
        }
        if output.is_none() {
            match output_rx.try_recv() {
                Ok(result) => {
                    output =
                        Some(result.with_context(|| {
                            format!("read {} --version output", path.display())
                        })?);
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(anyhow!(
                        "reader thread stopped for {} --version",
                        path.display()
                    ));
                }
            }
        }
        match (status.take(), output.take()) {
            (Some(status), Some((stdout, truncated))) => {
                return Ok(VersionCommandOutput {
                    status,
                    stdout,
                    truncated,
                });
            }
            (next_status, next_output) => {
                status = next_status;
                output = next_output;
            }
        }
        if started.elapsed() >= VERSION_PROBE_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err(anyhow!(
                "{} --version timed out after {}ms",
                path.display(),
                VERSION_PROBE_TIMEOUT.as_millis()
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn read_capped_output(mut reader: impl Read, limit: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 1024];
    while output.len() < limit {
        let remaining = limit - output.len();
        let max_read = remaining.min(buffer.len());
        let read = reader.read(&mut buffer[..max_read])?;
        if read == 0 {
            return Ok((output, false));
        }
        output.extend_from_slice(&buffer[..read]);
    }
    Ok((output, true))
}
