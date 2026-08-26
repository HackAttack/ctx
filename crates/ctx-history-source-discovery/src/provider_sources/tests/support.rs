use std::{env, ffi::OsString, sync::Mutex};

pub(super) static ENV_LOCK: Mutex<()> = Mutex::new(());

pub(super) fn tempdir() -> tempfile::TempDir {
    crate::test_support_paths::tempdir()
        .expect("system temporary directory should support test fixtures")
}

pub(super) struct EnvGuard {
    name: &'static str,
    original: Option<OsString>,
}

impl EnvGuard {
    pub(super) fn remove(name: &'static str) -> Self {
        let original = env::var_os(name);
        env::remove_var(name);
        Self { name, original }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(value) = &self.original {
            env::set_var(self.name, value);
        } else {
            env::remove_var(self.name);
        }
    }
}
