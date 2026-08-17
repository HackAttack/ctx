use super::*;

pub(super) fn source_topology_open_error(error: io::Error) -> IndexError {
    if platform::is_nofollow_rejection(&error) {
        IndexError::CurrentRepublishSourceTopology(
            "symlink, reparse point, or remote-provider file in republish source",
        )
    } else {
        IndexError::Io(error)
    }
}

pub(super) fn admit_available_bytes(
    directory: &BoundDirectory,
    required: u64,
    recheck: bool,
) -> Result<()> {
    let available = available_bytes(directory, recheck)?;
    if available < required {
        return Err(IndexError::CurrentRepublishInsufficientHeadroom {
            available,
            required,
        });
    }
    Ok(())
}

fn available_bytes(directory: &BoundDirectory, recheck: bool) -> Result<u64> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(available) = test_support::TEST_OPTIONS.with(|options| {
        let options = options.borrow();
        if recheck {
            options
                .rechecked_available_bytes
                .or(options.available_bytes)
        } else {
            options.available_bytes
        }
    }) {
        return Ok(available);
    }
    #[cfg(not(any(test, feature = "test-support")))]
    let _ = recheck;
    platform::available_bytes(&directory.file, &directory.path).map_err(IndexError::Io)
}
