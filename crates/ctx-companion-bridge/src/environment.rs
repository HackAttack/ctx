use std::{collections::BTreeMap, ffi::OsString};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum EnvironmentKey {
    Home,
    InstallationId,
    Lang,
    LcAll,
    TimeZone,
    DbusSessionBusAddress,
    XdgRuntimeDir,
    LocalUsageEnabled,
}

impl EnvironmentKey {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Home => "HOME",
            Self::InstallationId => "CTX_PRO_INSTALLATION_ID",
            Self::Lang => "LANG",
            Self::LcAll => "LC_ALL",
            Self::TimeZone => "TZ",
            Self::DbusSessionBusAddress => "DBUS_SESSION_BUS_ADDRESS",
            Self::XdgRuntimeDir => "XDG_RUNTIME_DIR",
            Self::LocalUsageEnabled => "CTX_LOCAL_USAGE_ENABLED",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct CompanionEnvironment {
    values: BTreeMap<EnvironmentKey, OsString>,
}

impl CompanionEnvironment {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, key: EnvironmentKey, value: impl Into<OsString>) -> &mut Self {
        self.values.insert(key, value.into());
        self
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (EnvironmentKey, &std::ffi::OsStr)> {
        self.values
            .iter()
            .map(|(key, value)| (*key, value.as_os_str()))
    }

    pub(crate) fn len(&self) -> usize {
        self.values.len()
    }
}
