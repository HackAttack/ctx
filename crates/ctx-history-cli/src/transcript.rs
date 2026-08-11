use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
};

use anyhow::Result;

mod artifact;
use artifact::{atomic_write_output, AtomicOutputFile};

pub fn write_output(body: String, out: Option<PathBuf>, stdout: &mut dyn Write) -> Result<()> {
    if let Some(out) = out {
        if let Some(parent) = out.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        atomic_write_output(&out, body.as_bytes())?;
    } else {
        stdout.write_all(body.as_bytes())?;
        if !body.ends_with('\n') {
            stdout.write_all(b"\n")?;
        }
        stdout.flush()?;
    }
    Ok(())
}

pub struct TranscriptOutput<'a> {
    destination: TranscriptDestination<'a>,
    bytes_written: usize,
}

enum TranscriptDestination<'a> {
    Stdout(&'a mut (dyn Write + Send)),
    Staged(AtomicOutputFile),
}

impl<'a> TranscriptOutput<'a> {
    pub fn create(out: Option<PathBuf>, stdout: &'a mut (dyn Write + Send)) -> Result<Self> {
        let destination = if let Some(out) = out {
            if let Some(parent) = out.parent().filter(|parent| !parent.as_os_str().is_empty()) {
                fs::create_dir_all(parent)?;
            }
            TranscriptDestination::Staged(AtomicOutputFile::create(&out)?)
        } else {
            TranscriptDestination::Stdout(stdout)
        };
        Ok(Self {
            destination,
            bytes_written: 0,
        })
    }

    pub fn finish(mut self) -> Result<usize> {
        self.flush()?;
        let bytes_written = self.bytes_written;
        if let TranscriptDestination::Staged(output) = self.destination {
            output.commit()?;
        }
        Ok(bytes_written)
    }
}

impl Write for TranscriptOutput<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let written = match &mut self.destination {
            TranscriptDestination::Stdout(writer) => writer.write(buffer)?,
            TranscriptDestination::Staged(writer) => writer.write(buffer)?,
        };
        self.bytes_written = self.bytes_written.saturating_add(written);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.destination {
            TranscriptDestination::Stdout(writer) => writer.flush(),
            TranscriptDestination::Staged(writer) => writer.flush(),
        }
    }
}

pub fn shell_quote_arg(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '@'))
    {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}
