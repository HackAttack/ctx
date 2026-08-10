use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};

#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
#[cfg(windows)]
use std::os::windows::process::CommandExt as _;

#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
#[cfg(windows)]
const DETACHED_PROCESS: u32 = 0x0000_0008;

/// A complete, immutable process launch assembled by product composition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedLaunch {
    program: PathBuf,
    args: Vec<OsString>,
    environment: BTreeMap<OsString, OsString>,
}

impl NormalizedLaunch {
    pub fn new(
        program: PathBuf,
        args: Vec<OsString>,
        environment: BTreeMap<OsString, OsString>,
    ) -> Self {
        Self {
            program,
            args,
            environment,
        }
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    pub fn args(&self) -> impl Iterator<Item = &OsStr> {
        self.args.iter().map(OsString::as_os_str)
    }

    pub fn get_args(&self) -> impl Iterator<Item = &OsStr> {
        self.args()
    }

    pub fn environment(&self) -> impl Iterator<Item = (&OsStr, &OsStr)> {
        self.environment
            .iter()
            .map(|(key, value)| (key.as_os_str(), value.as_os_str()))
    }

    pub fn get_envs(&self) -> impl Iterator<Item = (&OsStr, Option<&OsStr>)> {
        self.environment().map(|(name, value)| (name, Some(value)))
    }
}

/// Spawns an already-normalized launch without inheriting stdio or a terminal session.
pub fn spawn_detached(launch: NormalizedLaunch) -> io::Result<Child> {
    let mut command = Command::new(launch.program);
    command
        .args(launch.args)
        .env_clear()
        .envs(launch.environment)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    #[cfg(windows)]
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    command.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_launch_is_an_immutable_snapshot() {
        let mut environment = BTreeMap::from([(OsString::from("A"), OsString::from("before"))]);
        let launch = NormalizedLaunch::new(
            PathBuf::from("daemon"),
            vec![OsString::from("run")],
            environment.clone(),
        );
        environment.insert(OsString::from("A"), OsString::from("after"));

        assert_eq!(launch.program(), Path::new("daemon"));
        assert_eq!(launch.args().collect::<Vec<_>>(), [OsStr::new("run")]);
        assert_eq!(
            launch.environment().collect::<Vec<_>>(),
            [(OsStr::new("A"), OsStr::new("before"))]
        );
    }
}
