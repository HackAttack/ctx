use super::*;

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloneStage {
    BeforeFile,
    AfterSourceOpen,
    BeforeHardlink,
    BeforeCopy,
    AfterFile,
    BeforeCleanup,
}

#[cfg(not(any(test, feature = "test-support")))]
#[derive(Debug, Clone, Copy)]
pub(super) enum CloneStage {
    BeforeFile,
    AfterSourceOpen,
    BeforeHardlink,
    BeforeCopy,
    AfterFile,
    BeforeCleanup,
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Copy, Default)]
pub struct CloneTestOptions {
    pub force_copy: bool,
    pub force_reflink_fallback: bool,
    pub force_hardlink_fallback: bool,
    pub available_bytes: Option<u64>,
    pub rechecked_available_bytes: Option<u64>,
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CloneMetrics {
    pub planned_files: usize,
    pub logical_bytes: u64,
    pub required_headroom: u64,
    pub available_bytes: u64,
    pub copied_bytes: u64,
    pub linked_files: usize,
    pub copied_files: usize,
}

#[cfg(any(test, feature = "test-support"))]
pub(super) type CloneTestHook = Box<dyn for<'a> FnMut(CloneStage, &'a Path) -> Result<()>>;

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    pub(super) static TEST_CLONE_OPTIONS: std::cell::RefCell<CloneTestOptions> = const {
        std::cell::RefCell::new(CloneTestOptions {
            force_copy: false,
            force_reflink_fallback: false,
            force_hardlink_fallback: false,
            available_bytes: None,
            rechecked_available_bytes: None,
        })
    };
    static TEST_CLONE_HOOK: std::cell::RefCell<Option<CloneTestHook>> =
        std::cell::RefCell::new(None);
    static TEST_CLONE_METRICS: std::cell::Cell<CloneMetrics> = const {
        std::cell::Cell::new(CloneMetrics {
            planned_files: 0,
            logical_bytes: 0,
            required_headroom: 0,
            available_bytes: 0,
            copied_bytes: 0,
            linked_files: 0,
            copied_files: 0,
        })
    };
}

#[cfg(any(test, feature = "test-support"))]
pub struct CloneTestHookGuard {
    previous_options: CloneTestOptions,
    previous_hook: Option<CloneTestHook>,
    previous_metrics: CloneMetrics,
}

#[cfg(any(test, feature = "test-support"))]
impl CloneTestHookGuard {
    pub fn set<F>(options: CloneTestOptions, hook: F) -> Self
    where
        F: for<'a> FnMut(CloneStage, &'a Path) -> Result<()> + 'static,
    {
        let previous_options = TEST_CLONE_OPTIONS.with(|slot| slot.replace(options));
        let previous_hook = TEST_CLONE_HOOK.with(|slot| slot.replace(Some(Box::new(hook))));
        let previous_metrics =
            TEST_CLONE_METRICS.with(|slot| slot.replace(CloneMetrics::default()));
        Self {
            previous_options,
            previous_hook,
            previous_metrics,
        }
    }

    pub fn metrics(&self) -> CloneMetrics {
        TEST_CLONE_METRICS.with(std::cell::Cell::get)
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for CloneTestHookGuard {
    fn drop(&mut self) {
        TEST_CLONE_OPTIONS.with(|slot| slot.replace(self.previous_options));
        TEST_CLONE_HOOK.with(|slot| slot.replace(self.previous_hook.take()));
        TEST_CLONE_METRICS.with(|slot| slot.set(self.previous_metrics));
    }
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn force_copy_fallback() -> bool {
    TEST_CLONE_OPTIONS.with(|options| options.borrow().force_copy)
}

#[cfg(not(any(test, feature = "test-support")))]
pub(super) fn force_copy_fallback() -> bool {
    false
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn force_reflink_fallback() -> bool {
    TEST_CLONE_OPTIONS.with(|options| options.borrow().force_reflink_fallback)
}

#[cfg(not(any(test, feature = "test-support")))]
pub(super) fn force_reflink_fallback() -> bool {
    false
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn force_hardlink_fallback() -> bool {
    TEST_CLONE_OPTIONS.with(|options| options.borrow().force_hardlink_fallback)
}

#[cfg(not(any(test, feature = "test-support")))]
pub(super) fn force_hardlink_fallback() -> bool {
    false
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn clone_checkpoint(stage: CloneStage, path: &Path) -> Result<()> {
    TEST_CLONE_HOOK.with(|hook| match hook.borrow_mut().as_mut() {
        Some(hook) => hook(stage, path),
        None => Ok(()),
    })
}

#[cfg(not(any(test, feature = "test-support")))]
pub(super) fn clone_checkpoint(_stage: CloneStage, _path: &Path) -> Result<()> {
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
    TEST_CLONE_METRICS.with(|metrics| {
        metrics.set(CloneMetrics {
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
pub(super) fn record_clone_metrics(copied_bytes: u64, linked_files: usize, copied_files: usize) {
    TEST_CLONE_METRICS.with(|metrics| {
        metrics.set(CloneMetrics {
            copied_bytes,
            linked_files,
            copied_files,
            ..metrics.get()
        });
    });
}

#[cfg(not(any(test, feature = "test-support")))]
pub(super) fn record_clone_metrics(_copied_bytes: u64, _linked_files: usize, _copied_files: usize) {
}
