use super::*;

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortableCloneStage {
    BeforeCopy,
    AfterSourceOpen,
    AfterCopy,
    BeforeCleanup,
}

#[cfg(not(any(test, feature = "test-support")))]
#[derive(Debug, Clone, Copy)]
pub(super) enum PortableCloneStage {
    BeforeCopy,
    AfterSourceOpen,
    AfterCopy,
    BeforeCleanup,
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Copy, Default)]
pub struct PortableCloneTestOptions {
    pub available_bytes: Option<u64>,
    pub rechecked_available_bytes: Option<u64>,
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PortableCloneMetrics {
    pub planned_files: usize,
    pub logical_bytes: u64,
    pub required_headroom: u64,
    pub available_bytes: u64,
    pub copied_bytes: u64,
    pub copied_files: usize,
}

#[cfg(any(test, feature = "test-support"))]
type PortableCloneTestHook = Box<dyn for<'a> FnMut(PortableCloneStage, &'a Path) -> Result<()>>;

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static FORCE_PORTABLE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(super) static TEST_OPTIONS: std::cell::RefCell<PortableCloneTestOptions> = const {
        std::cell::RefCell::new(PortableCloneTestOptions {
            available_bytes: None,
            rechecked_available_bytes: None,
        })
    };
    static TEST_HOOK: std::cell::RefCell<Option<PortableCloneTestHook>> =
        std::cell::RefCell::new(None);
    static TEST_METRICS: std::cell::Cell<PortableCloneMetrics> = const {
        std::cell::Cell::new(PortableCloneMetrics {
            planned_files: 0,
            logical_bytes: 0,
            required_headroom: 0,
            available_bytes: 0,
            copied_bytes: 0,
            copied_files: 0,
        })
    };
}

#[cfg(any(test, feature = "test-support"))]
pub struct PortableCloneTestGuard {
    previous_force: bool,
    previous_options: PortableCloneTestOptions,
    previous_hook: Option<PortableCloneTestHook>,
    previous_metrics: PortableCloneMetrics,
}

#[cfg(any(test, feature = "test-support"))]
impl PortableCloneTestGuard {
    pub fn set<F>(options: PortableCloneTestOptions, hook: F) -> Self
    where
        F: for<'a> FnMut(PortableCloneStage, &'a Path) -> Result<()> + 'static,
    {
        let previous_force = FORCE_PORTABLE.with(|force| force.replace(true));
        let previous_options = TEST_OPTIONS.with(|slot| slot.replace(options));
        let previous_hook = TEST_HOOK.with(|slot| slot.replace(Some(Box::new(hook))));
        let previous_metrics =
            TEST_METRICS.with(|slot| slot.replace(PortableCloneMetrics::default()));
        Self {
            previous_force,
            previous_options,
            previous_hook,
            previous_metrics,
        }
    }

    pub fn metrics(&self) -> PortableCloneMetrics {
        TEST_METRICS.with(std::cell::Cell::get)
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for PortableCloneTestGuard {
    fn drop(&mut self) {
        FORCE_PORTABLE.with(|slot| slot.set(self.previous_force));
        TEST_OPTIONS.with(|slot| slot.replace(self.previous_options));
        TEST_HOOK.with(|slot| slot.replace(self.previous_hook.take()));
        TEST_METRICS.with(|slot| slot.set(self.previous_metrics));
    }
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn forced_for_test() -> bool {
    FORCE_PORTABLE.with(std::cell::Cell::get)
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn clone_checkpoint(stage: PortableCloneStage, path: &Path) -> Result<()> {
    TEST_HOOK.with(|hook| match hook.borrow_mut().as_mut() {
        Some(hook) => hook(stage, path),
        None => Ok(()),
    })
}

#[cfg(not(any(test, feature = "test-support")))]
pub(super) fn clone_checkpoint(_stage: PortableCloneStage, _path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn record_plan_metrics(plan: &ClonePlan, available: u64) {
    record_plan_metrics_with_required(plan, available, plan.required_headroom);
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn record_plan_metrics_with_required(
    plan: &ClonePlan,
    available: u64,
    required_headroom: u64,
) {
    TEST_METRICS.with(|metrics| {
        metrics.set(PortableCloneMetrics {
            planned_files: plan.files.len(),
            logical_bytes: plan.logical_bytes,
            required_headroom,
            available_bytes: available,
            ..metrics.get()
        });
    });
}

#[cfg(not(any(test, feature = "test-support")))]
pub(super) fn record_plan_metrics(_plan: &ClonePlan, _available: u64) {}

#[cfg(not(any(test, feature = "test-support")))]
pub(super) fn record_plan_metrics_with_required(
    _plan: &ClonePlan,
    _available: u64,
    _required_headroom: u64,
) {
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn record_clone_metrics(copied_bytes: u64, copied_files: usize) {
    TEST_METRICS.with(|metrics| {
        metrics.set(PortableCloneMetrics {
            copied_bytes,
            copied_files,
            ..metrics.get()
        });
    });
}

#[cfg(not(any(test, feature = "test-support")))]
pub(super) fn record_clone_metrics(_copied_bytes: u64, _copied_files: usize) {}
