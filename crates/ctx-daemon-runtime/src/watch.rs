use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use notify::{
    event::{AccessKind, AccessMode, CreateKind, MetadataKind, ModifyKind, RemoveKind},
    Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
};

use crate::CoalescingWakePayload;

pub const WATCH_EVENT_QUEUE_CAPACITY: usize = 256;
pub const WATCH_DEBOUNCE_QUIET: Duration = Duration::from_millis(250);
pub const WATCH_DEBOUNCE_MAX: Duration = Duration::from_secs(2);

static NEXT_WATCHER_EPOCH: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeWatchIgnore {
    Access,
    AccessTime,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeWatchEvent {
    pub paths: Vec<PathBuf>,
    needs_rescan: bool,
    requires_rearm: bool,
    ignored: Option<NativeWatchIgnore>,
}

impl NativeWatchEvent {
    pub fn ordinary(paths: Vec<PathBuf>) -> Self {
        Self {
            paths,
            needs_rescan: false,
            requires_rearm: false,
            ignored: None,
        }
    }

    pub fn requiring_rearm(paths: Vec<PathBuf>) -> Self {
        Self {
            requires_rearm: true,
            ..Self::ordinary(paths)
        }
    }

    pub fn rescan(paths: Vec<PathBuf>) -> Self {
        Self {
            paths,
            needs_rescan: true,
            requires_rearm: true,
            ignored: None,
        }
    }

    pub fn ignored(paths: Vec<PathBuf>, ignored: NativeWatchIgnore) -> Self {
        Self {
            ignored: Some(ignored),
            ..Self::ordinary(paths)
        }
    }

    pub fn needs_rescan(&self) -> bool {
        self.needs_rescan
    }

    pub fn requires_rearm(&self) -> bool {
        self.requires_rearm
    }

    pub fn ignored_kind(&self) -> Option<NativeWatchIgnore> {
        self.ignored
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeWatchError;

pub type NativeWatchResult = std::result::Result<NativeWatchEvent, NativeWatchError>;

fn normalize_native_watch_event(event: notify::Result<Event>) -> NativeWatchResult {
    let event = event.map_err(|_| NativeWatchError)?;
    let ignored = if matches!(
        event.kind,
        EventKind::Access(kind) if !matches!(kind, AccessKind::Close(AccessMode::Write))
    ) {
        Some(NativeWatchIgnore::Access)
    } else if matches!(
        event.kind,
        EventKind::Modify(ModifyKind::Metadata(MetadataKind::AccessTime))
    ) {
        Some(NativeWatchIgnore::AccessTime)
    } else {
        None
    };
    let requires_rearm = matches!(
        event.kind,
        EventKind::Any
            | EventKind::Other
            | EventKind::Create(CreateKind::Any | CreateKind::Folder | CreateKind::Other)
            | EventKind::Modify(ModifyKind::Any | ModifyKind::Name(_) | ModifyKind::Other)
            | EventKind::Remove(RemoveKind::Any | RemoveKind::Folder | RemoveKind::Other)
    );
    let needs_rescan = event.need_rescan();
    Ok(NativeWatchEvent {
        paths: event.paths,
        needs_rescan,
        requires_rearm,
        ignored,
    })
}

#[derive(Debug, Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub struct WatchWatermark {
    pub epoch: u64,
    pub sequence: u64,
}

impl WatchWatermark {
    fn new(epoch: u64, sequence: u64) -> Self {
        Self { epoch, sequence }
    }
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct NativeWatcherSnapshot {
    pub ingress_overflows: u64,
    pub ingress_disconnects: u64,
    pub coalesced_wakeups: u64,
    pub reconciliations: u64,
    pub forced_rearms: u64,
    pub registration_attempts: u64,
    pub watched_roots: usize,
}

#[derive(Debug, Default)]
struct NativeWatcherCounters {
    ingress_overflows: u64,
    ingress_disconnects: u64,
    coalesced_wakeups: u64,
    reconciliations: u64,
    forced_rearms: u64,
    registration_attempts: u64,
}

enum WatchMessage {
    Event {
        event: NativeWatchResult,
        watermark: WatchWatermark,
    },
    Stop,
}

type EventClassifier<P> = Arc<dyn Fn(NativeWatchResult, WatchWatermark) -> P + Send + Sync>;
type ReconciliationFactory<P> = Arc<dyn Fn(WatchWatermark) -> P + Send + Sync>;
type IgnoreEvent = Arc<dyn Fn(&NativeWatchEvent) -> bool + Send + Sync>;
type ObservePayload<P> = Arc<dyn Fn(&P) + Send + Sync>;
type SignalPayload<P> = Arc<dyn Fn(P) + Send + Sync>;
type RearmOverlapHook = Box<dyn FnMut(&Path)>;

pub struct NativeFileWatcher<P: CoalescingWakePayload> {
    watcher: RecommendedWatcher,
    watched: BTreeMap<PathBuf, bool>,
    counters: Arc<Mutex<NativeWatcherCounters>>,
    sender: mpsc::SyncSender<WatchMessage>,
    accepting_events: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    watcher_epoch: u64,
    callback_sequence: Arc<AtomicU64>,
    ignore_event: IgnoreEvent,
    reconciliation: ReconciliationFactory<P>,
    signal_payload: SignalPayload<P>,
    rearm_pending: bool,
    rearm_overlap_hook: Option<RearmOverlapHook>,
}

impl<P: CoalescingWakePayload> NativeFileWatcher<P> {
    pub fn start(
        thread_name: &str,
        ignore_event: IgnoreEvent,
        classify_event: EventClassifier<P>,
        reconciliation: ReconciliationFactory<P>,
        observe_payload: ObservePayload<P>,
        signal_payload: SignalPayload<P>,
    ) -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(WATCH_EVENT_QUEUE_CAPACITY);
        let counters = Arc::new(Mutex::new(NativeWatcherCounters::default()));
        let accepting_events = Arc::new(AtomicBool::new(true));
        let watcher_epoch = NEXT_WATCHER_EPOCH.fetch_add(1, Ordering::Relaxed);
        let callback_sequence = Arc::new(AtomicU64::new(0));
        let watcher = native_file_watcher(
            &sender,
            &counters,
            &accepting_events,
            watcher_epoch,
            &callback_sequence,
            &ignore_event,
            &reconciliation,
            &signal_payload,
        )?;
        let thread_counters = Arc::clone(&counters);
        let thread_signal_payload = Arc::clone(&signal_payload);
        let thread = thread::Builder::new()
            .name(thread_name.to_owned())
            .spawn(move || {
                watch_event_loop(
                    receiver,
                    thread_counters,
                    classify_event,
                    observe_payload,
                    thread_signal_payload,
                );
            })
            .context("start native filesystem debounce worker")?;
        Ok(Self {
            watcher,
            watched: BTreeMap::new(),
            counters,
            sender,
            accepting_events,
            thread: Some(thread),
            watcher_epoch,
            callback_sequence,
            ignore_event,
            reconciliation,
            signal_payload,
            rearm_pending: false,
            rearm_overlap_hook: None,
        })
    }

    pub fn startup_watermark(&self) -> WatchWatermark {
        WatchWatermark::new(self.watcher_epoch, 0)
    }

    pub fn next_watermark(&self) -> WatchWatermark {
        WatchWatermark::new(
            self.watcher_epoch,
            self.callback_sequence
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                    Some(current.saturating_add(1))
                })
                .unwrap_or_else(|current| current)
                .saturating_add(1),
        )
    }

    pub fn needs_registration(&self, desired: &BTreeMap<PathBuf, bool>, force_rearm: bool) -> bool {
        desired.iter().any(|(path, recursive)| {
            self.replacement_required(force_rearm)
                || self.watched.get(path).copied() != Some(*recursive)
        })
    }

    pub fn replacement_required(&self, force_rearm: bool) -> bool {
        force_rearm || self.rearm_pending
    }

    pub fn reconcile_paths(
        &mut self,
        desired: BTreeMap<PathBuf, bool>,
        force_rearm: bool,
    ) -> Result<()> {
        let mut last_error = None;
        let mut registration_attempts = 0_u64;
        self.rearm_pending |= force_rearm;
        if self.rearm_pending {
            match native_file_watcher(
                &self.sender,
                &self.counters,
                &self.accepting_events,
                self.watcher_epoch,
                &self.callback_sequence,
                &self.ignore_event,
                &self.reconciliation,
                &self.signal_payload,
            ) {
                Ok(mut replacement) => {
                    let mut replacement_ready = true;
                    for (path, recursive) in &desired {
                        registration_attempts = registration_attempts.saturating_add(1);
                        if let Err(error) = replacement.watch(path, recursive_mode(*recursive)) {
                            replacement_ready = false;
                            last_error = Some(anyhow::anyhow!("watch {}: {error}", path.display()));
                        }
                    }
                    if replacement_ready {
                        for path in desired.keys() {
                            if let Some(hook) = self.rearm_overlap_hook.as_mut() {
                                hook(path);
                            }
                        }
                        self.watcher = replacement;
                        self.watched = desired;
                        self.rearm_pending = false;
                    }
                }
                Err(error) => last_error = Some(error),
            }
        } else {
            let stale = self
                .watched
                .keys()
                .filter(|path| !desired.contains_key(*path))
                .cloned()
                .collect::<Vec<_>>();
            for path in stale {
                if let Err(error) = self.watcher.unwatch(&path) {
                    last_error = Some(anyhow::anyhow!("unwatch {}: {error}", path.display()));
                }
                self.watched.remove(&path);
            }
            for (path, recursive) in &desired {
                let current = self.watched.get(path).copied();
                if current == Some(*recursive) {
                    continue;
                }
                if current.is_some() {
                    if let Err(error) = self.watcher.unwatch(path) {
                        last_error = Some(anyhow::anyhow!("unwatch {}: {error}", path.display()));
                    }
                    self.watched.remove(path);
                }
                registration_attempts = registration_attempts.saturating_add(1);
                match self.watcher.watch(path, recursive_mode(*recursive)) {
                    Ok(()) => {
                        self.watched.insert(path.clone(), *recursive);
                    }
                    Err(error) => {
                        last_error = Some(anyhow::anyhow!("watch {}: {error}", path.display()));
                    }
                }
            }
        }
        let mut counters = self.lock_counters();
        counters.reconciliations = counters.reconciliations.saturating_add(1);
        counters.registration_attempts = counters
            .registration_attempts
            .saturating_add(registration_attempts);
        if force_rearm {
            counters.forced_rearms = counters.forced_rearms.saturating_add(1);
        }
        drop(counters);
        match last_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    pub fn snapshot(&self) -> NativeWatcherSnapshot {
        let counters = self.lock_counters();
        NativeWatcherSnapshot {
            ingress_overflows: counters.ingress_overflows,
            ingress_disconnects: counters.ingress_disconnects,
            coalesced_wakeups: counters.coalesced_wakeups,
            reconciliations: counters.reconciliations,
            forced_rearms: counters.forced_rearms,
            registration_attempts: counters.registration_attempts,
            watched_roots: self.watched.len(),
        }
    }

    #[doc(hidden)]
    pub fn install_rearm_overlap_hook(&mut self, hook: impl FnMut(&Path) + 'static) {
        self.rearm_overlap_hook = Some(Box::new(hook));
    }

    pub fn stop(&mut self) {
        if self.accepting_events.swap(false, Ordering::AcqRel) {
            let _ = self.sender.send(WatchMessage::Stop);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    fn lock_counters(&self) -> std::sync::MutexGuard<'_, NativeWatcherCounters> {
        self.counters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl<P: CoalescingWakePayload> Drop for NativeFileWatcher<P> {
    fn drop(&mut self) {
        self.stop();
    }
}

fn recursive_mode(recursive: bool) -> RecursiveMode {
    if recursive {
        RecursiveMode::Recursive
    } else {
        RecursiveMode::NonRecursive
    }
}

#[allow(clippy::too_many_arguments)]
fn native_file_watcher<P: CoalescingWakePayload>(
    sender: &mpsc::SyncSender<WatchMessage>,
    counters: &Arc<Mutex<NativeWatcherCounters>>,
    accepting_events: &Arc<AtomicBool>,
    watcher_epoch: u64,
    callback_sequence: &Arc<AtomicU64>,
    ignore_event: &IgnoreEvent,
    reconciliation: &ReconciliationFactory<P>,
    signal_payload: &SignalPayload<P>,
) -> Result<RecommendedWatcher> {
    let sender = sender.clone();
    let counters = Arc::clone(counters);
    let accepting_events = Arc::clone(accepting_events);
    let sequence = Arc::clone(callback_sequence);
    let ignore_event = Arc::clone(ignore_event);
    let reconciliation = Arc::clone(reconciliation);
    let signal_payload = Arc::clone(signal_payload);
    RecommendedWatcher::new(
        move |event: notify::Result<Event>| {
            forward_native_watch_event(
                &sender,
                &counters,
                &accepting_events,
                watcher_epoch,
                &sequence,
                &ignore_event,
                &reconciliation,
                &signal_payload,
                normalize_native_watch_event(event),
            );
        },
        Config::default(),
    )
    .context("start native filesystem watcher")
}

#[allow(clippy::too_many_arguments)]
fn forward_native_watch_event<P: CoalescingWakePayload>(
    sender: &mpsc::SyncSender<WatchMessage>,
    counters: &Mutex<NativeWatcherCounters>,
    accepting_events: &AtomicBool,
    watcher_epoch: u64,
    sequence: &AtomicU64,
    ignore_event: &IgnoreEvent,
    reconciliation: &ReconciliationFactory<P>,
    signal_payload: &SignalPayload<P>,
    event: NativeWatchResult,
) {
    if !accepting_events.load(Ordering::Acquire)
        || event.as_ref().is_ok_and(|event| ignore_event(event))
    {
        return;
    }
    let watermark = WatchWatermark::new(
        watcher_epoch,
        sequence
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_add(1))
            })
            .unwrap_or_else(|current| current)
            .saturating_add(1),
    );
    match sender.try_send(WatchMessage::Event { event, watermark }) {
        Ok(()) => {}
        Err(mpsc::TrySendError::Full(_)) => {
            let mut counters = counters
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            counters.ingress_overflows = counters.ingress_overflows.saturating_add(1);
            drop(counters);
            signal_payload(reconciliation(watermark));
        }
        Err(mpsc::TrySendError::Disconnected(_)) => {
            let mut counters = counters
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            counters.ingress_disconnects = counters.ingress_disconnects.saturating_add(1);
            drop(counters);
            signal_payload(reconciliation(watermark));
        }
    }
}

fn watch_event_loop<P: CoalescingWakePayload>(
    receiver: mpsc::Receiver<WatchMessage>,
    counters: Arc<Mutex<NativeWatcherCounters>>,
    classify_event: EventClassifier<P>,
    observe_payload: ObservePayload<P>,
    signal_payload: SignalPayload<P>,
) {
    loop {
        let (first, first_watermark) = match receiver.recv() {
            Ok(WatchMessage::Event { event, watermark }) => (event, watermark),
            Ok(WatchMessage::Stop) | Err(_) => return,
        };
        let started = Instant::now();
        let mut relevant = classify_event(first, first_watermark);
        observe_payload(&relevant);
        loop {
            let elapsed = started.elapsed();
            if elapsed >= WATCH_DEBOUNCE_MAX {
                break;
            }
            let timeout = WATCH_DEBOUNCE_QUIET.min(WATCH_DEBOUNCE_MAX - elapsed);
            match receiver.recv_timeout(timeout) {
                Ok(WatchMessage::Event { event, watermark }) => {
                    let payload = classify_event(event, watermark);
                    observe_payload(&payload);
                    relevant.merge(payload);
                }
                Ok(WatchMessage::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                Err(mpsc::RecvTimeoutError::Timeout) => break,
            }
        }
        if !relevant.is_empty() {
            let mut counters = counters
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            counters.coalesced_wakeups = counters.coalesced_wakeups.saturating_add(1);
            drop(counters);
            signal_payload(relevant);
        }
    }
}

pub fn watch_roots<'a>(targets: impl IntoIterator<Item = &'a Path>) -> BTreeMap<PathBuf, bool> {
    let mut roots = BTreeMap::new();
    for target in targets {
        if target.is_dir() {
            roots
                .entry(target.to_path_buf())
                .and_modify(|recursive| *recursive = true)
                .or_insert(true);
        } else if target.is_file() {
            if let Some(parent) = target.parent() {
                roots.entry(parent.to_path_buf()).or_insert(false);
            }
        } else if let Some(existing) = target.ancestors().find(|candidate| candidate.is_dir()) {
            roots.entry(existing.to_path_buf()).or_insert(false);
        }
    }
    roots
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU64;

    use notify::event::{DataChange, Flag};

    use super::*;

    #[derive(Clone, Debug, Default, Eq, PartialEq)]
    struct TestPayload {
        reconciliation: Option<WatchWatermark>,
    }

    impl CoalescingWakePayload for TestPayload {
        fn is_empty(&self) -> bool {
            self.reconciliation.is_none()
        }

        fn merge(&mut self, other: Self) {
            if let Some(watermark) = other.reconciliation {
                self.reconciliation = Some(
                    self.reconciliation
                        .map_or(watermark, |current| current.max(watermark)),
                );
            }
        }
    }

    #[test]
    fn notify_events_are_normalized_without_product_policy() {
        let path = PathBuf::from("/tmp/history.jsonl");
        let access = normalize_native_watch_event(Ok(Event::new(EventKind::Access(
            AccessKind::Read,
        ))
        .add_path(path.clone())))
        .unwrap();
        assert_eq!(access.paths, vec![path.clone()]);
        assert_eq!(access.ignored_kind(), Some(NativeWatchIgnore::Access));
        assert!(!access.needs_rescan());
        assert!(!access.requires_rearm());

        let access_time = normalize_native_watch_event(Ok(Event::new(EventKind::Modify(
            ModifyKind::Metadata(MetadataKind::AccessTime),
        ))
        .add_path(path.clone())))
        .unwrap();
        assert_eq!(
            access_time.ignored_kind(),
            Some(NativeWatchIgnore::AccessTime)
        );

        let write_close = normalize_native_watch_event(Ok(Event::new(EventKind::Access(
            AccessKind::Close(AccessMode::Write),
        ))
        .add_path(path.clone())))
        .unwrap();
        assert_eq!(write_close.ignored_kind(), None);

        let rename = normalize_native_watch_event(Ok(Event::new(EventKind::Modify(
            ModifyKind::Name(notify::event::RenameMode::Both),
        ))
        .add_path(path.clone())))
        .unwrap();
        assert!(rename.requires_rearm());

        let rescan = normalize_native_watch_event(Ok(Event::new(EventKind::Modify(
            ModifyKind::Data(DataChange::Content),
        ))
        .add_path(path)
        .set_flag(Flag::Rescan)))
        .unwrap();
        assert!(rescan.needs_rescan());
    }

    #[test]
    fn full_ingress_fails_closed_into_bounded_reconciliation() {
        let counters = Mutex::new(NativeWatcherCounters::default());
        let accepting_events = AtomicBool::new(true);
        let sequence = AtomicU64::new(0);
        let (sender, receiver) = mpsc::sync_channel(1);
        let ignore_event: IgnoreEvent = Arc::new(|_| false);
        let reconciliation: ReconciliationFactory<TestPayload> =
            Arc::new(|watermark| TestPayload {
                reconciliation: Some(watermark),
            });
        let signaled = Arc::new(Mutex::new(Vec::new()));
        let signal_payload: SignalPayload<TestPayload> = {
            let signaled = Arc::clone(&signaled);
            Arc::new(move |payload| signaled.lock().unwrap().push(payload))
        };

        for _ in 0..2 {
            forward_native_watch_event(
                &sender,
                &counters,
                &accepting_events,
                9,
                &sequence,
                &ignore_event,
                &reconciliation,
                &signal_payload,
                Ok(NativeWatchEvent::ordinary(vec![PathBuf::from(
                    "/tmp/config.toml",
                )])),
            );
        }

        assert_eq!(counters.lock().unwrap().ingress_overflows, 1);
        assert_eq!(
            signaled.lock().unwrap().as_slice(),
            &[TestPayload {
                reconciliation: Some(WatchWatermark::new(9, 2)),
            }]
        );
        match receiver.try_recv().expect("one event remains bounded") {
            WatchMessage::Event { watermark, .. } => {
                assert_eq!(watermark, WatchWatermark::new(9, 1));
            }
            WatchMessage::Stop => panic!("unexpected stop message"),
        }
        assert!(receiver.try_recv().is_err());
    }
}
