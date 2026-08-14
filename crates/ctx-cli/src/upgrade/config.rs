use std::{
    fs,
    io::{Read, Write},
    path::Path,
};

use anyhow::{Context, Result};
use ctx_history_platform::platform_security::{
    establish_private_data_root, restrict_private_file_handle, verify_private_directory,
    verify_private_file_handle,
};
use uuid::Uuid;

pub(super) fn set_auto_mode(data_root: &Path, mode: &str) -> Result<()> {
    establish_private_data_root(data_root)
        .with_context(|| format!("protect private upgrade data root {}", data_root.display()))?;
    verify_private_directory(data_root)
        .with_context(|| format!("verify private upgrade data root {}", data_root.display()))?;
    let config_path = data_root.join(crate::config::CONFIG_FILE);
    let existing = read_upgrade_config(&config_path)?;
    let next = set_toml_section_value(&existing, "upgrade", "auto", &format!("\"{mode}\""));
    write_private_config(&config_path, next.as_bytes())?;
    Ok(())
}

fn read_upgrade_config(path: &Path) -> Result<String> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
        };

        options
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => return Err(error).with_context(|| format!("open {}", path.display())),
    };
    verify_private_file_handle(&file)
        .with_context(|| format!("verify private upgrade config {}", path.display()))?;
    let mut existing = String::new();
    file.read_to_string(&mut existing)
        .with_context(|| format!("read {}", path.display()))?;
    Ok(existing)
}

fn write_private_config(path: &Path, body: &[u8]) -> Result<()> {
    let temporary = path.with_extension(format!("tmp.{}", Uuid::new_v4().simple()));
    let result = (|| -> Result<()> {
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;

            options
                .mode(0o600)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            use windows_sys::Win32::{
                Foundation::{GENERIC_READ, GENERIC_WRITE},
                Storage::FileSystem::{
                    FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, READ_CONTROL, WRITE_DAC,
                },
            };

            options
                .access_mode(GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC)
                .share_mode(FILE_SHARE_READ)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        let mut file = options
            .open(&temporary)
            .with_context(|| format!("create {}", temporary.display()))?;
        restrict_private_file_handle(&file)
            .with_context(|| format!("protect {}", temporary.display()))?;
        verify_private_file_handle(&file)
            .with_context(|| format!("verify {}", temporary.display()))?;
        file.write_all(body)
            .with_context(|| format!("write {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("sync {}", temporary.display()))?;
        drop(file);
        replace_config_file(&temporary, path)?;
        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(not(windows))]
fn replace_config_file(temporary: &Path, target: &Path) -> Result<()> {
    fs::rename(temporary, target)
        .with_context(|| format!("rename {} to {}", temporary.display(), target.display()))
}

#[cfg(windows)]
fn replace_config_file(temporary: &Path, target: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let temporary_wide = temporary
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let target_wide = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let moved = unsafe {
        MoveFileExW(
            temporary_wide.as_ptr(),
            target_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("replace {}", target.display()));
    }
    Ok(())
}

fn set_toml_section_value(input: &str, section: &str, key: &str, value: &str) -> String {
    let mut lines = Vec::new();
    let mut in_section = false;
    let mut saw_section = false;
    let mut wrote_key = false;
    for raw in input.lines() {
        let trimmed = raw.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if in_section && !wrote_key {
                lines.push(format!("{key} = {value}"));
                wrote_key = true;
            }
            in_section = trimmed == format!("[{section}]");
            saw_section |= in_section;
            lines.push(raw.to_owned());
            continue;
        }
        if in_section
            && (trimmed.starts_with(&format!("{key} ")) || trimmed.starts_with(&format!("{key}=")))
        {
            lines.push(format!("{key} = {value}"));
            wrote_key = true;
        } else {
            lines.push(raw.to_owned());
        }
    }
    if saw_section {
        if in_section && !wrote_key {
            lines.push(format!("{key} = {value}"));
        }
    } else {
        if !lines.is_empty() && lines.last().is_some_and(|line| !line.is_empty()) {
            lines.push(String::new());
        }
        lines.push(format!("[{section}]"));
        lines.push(format!("{key} = {value}"));
    }
    lines.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_editor_keeps_one_canonical_upgrade_control() {
        let input = "[daemon]\nenabled = true\n\n[upgrade]\nauto = \"off\"\n";
        let output = set_toml_section_value(input, "upgrade", "auto", "\"apply\"");
        assert_eq!(output.matches("[upgrade]").count(), 1);
        assert_eq!(output.matches("auto = \"apply\"").count(), 1);
        assert!(!output.contains("auto = \"off\""));
    }
}
