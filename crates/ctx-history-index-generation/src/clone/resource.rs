use super::*;

pub(super) fn validate_single_component(path: &Path) -> Result<()> {
    let mut components = path.components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(IndexError::CurrentRepublishSourceTopology(
            "managed path escapes generation directory",
        ));
    }
    Ok(())
}

pub(super) fn admit_clone_resource(
    files: &mut usize,
    bytes: &mut u64,
    next_bytes: u64,
    maximum_files: usize,
    maximum_bytes: u64,
) -> Result<()> {
    *files = files.checked_add(1).ok_or(IndexError::CountOverflow)?;
    if *files > maximum_files {
        return Err(IndexError::CurrentRepublishFileLimit {
            actual: *files,
            maximum: maximum_files,
        });
    }
    *bytes = bytes
        .checked_add(next_bytes)
        .ok_or(IndexError::CountOverflow)?;
    if *bytes > maximum_bytes {
        return Err(IndexError::CurrentRepublishByteLimit {
            actual: *bytes,
            maximum: maximum_bytes,
        });
    }
    Ok(())
}
