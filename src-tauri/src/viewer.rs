use std::collections::HashMap;
use std::fs::File;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Instant;

use parking_lot::{Condvar, Mutex, RwLock};
use rand::random;

use crate::catalog::{Catalog, build_catalog};
use crate::decode::{MAX_DECODE_BYTES, ProductionDecoder, open_read_only};
use crate::error::{ViewerError, code as error_code};
use crate::model::DecodedRender;
use crate::policy::DecodePolicy;
use crate::protocol::{NavigationDirection, RenderDescriptor, ViewerSnapshot, ViewerStatus};

type EventSink = Arc<dyn Fn(ViewerSnapshot) + Send + Sync + 'static>;
const MAX_SAFE_RENDER_ID: u64 = (1_u64 << 53) - 1;

pub(crate) trait Decoder: Send + Sync + 'static {
    fn decode(&self, path: &Path, file: File) -> Result<DecodedRender, ViewerError>;

    fn cancel_current(&self) {}

    fn shutdown(&self) {}
}

impl Decoder for ProductionDecoder {
    fn decode(&self, path: &Path, file: File) -> Result<DecodedRender, ViewerError> {
        ProductionDecoder::decode(self, path, file)
    }

    fn cancel_current(&self) {
        ProductionDecoder::cancel_current(self);
    }

    fn shutdown(&self) {
        ProductionDecoder::shutdown(self);
    }
}

#[derive(Debug)]
struct DecodeJob {
    generation: u64,
    path: PathBuf,
    source: Result<File, ViewerError>,
}

struct RenderCache {
    entries: HashMap<u64, Vec<u8>>,
    used: u64,
    limit: u64,
}

impl RenderCache {
    fn new(limit: u64) -> Self {
        Self {
            entries: HashMap::new(),
            used: 0,
            limit,
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.used = 0;
    }

    fn insert(&mut self, bytes: Vec<u8>) -> Result<u64, ViewerError> {
        let size = bytes.len() as u64;
        if size > self.limit || self.used.saturating_add(size) > self.limit {
            return Err(ViewerError::limit(
                error_code::CACHE_LIMIT_EXCEEDED,
                "轉譯快取超過 512 MiB 上限。",
            )
            .with_parameter("maxBytes", self.limit));
        }
        // JavaScript numbers preserve integers exactly only through 2^53 - 1.
        // Keeping the opaque token in that range prevents invoke argument
        // rounding from turning a valid one-time ID into a cache miss.
        let mut render_id = random::<u64>() & MAX_SAFE_RENDER_ID;
        while render_id == 0 || self.entries.contains_key(&render_id) {
            render_id = random::<u64>() & MAX_SAFE_RENDER_ID;
        }
        self.used += size;
        self.entries.insert(render_id, bytes);
        Ok(render_id)
    }

    fn take(&mut self, render_id: u64) -> Option<Vec<u8>> {
        let bytes = self.entries.remove(&render_id)?;
        self.used = self.used.saturating_sub(bytes.len() as u64);
        Some(bytes)
    }
}

struct ViewerState {
    generation: u64,
    revision: u64,
    files: Vec<PathBuf>,
    index: Option<usize>,
    snapshot: ViewerSnapshot,
    pending: Option<DecodeJob>,
    renders: RenderCache,
    shutdown_requested: bool,
}

impl ViewerState {
    fn new() -> Self {
        Self {
            generation: 0,
            revision: 0,
            files: Vec::new(),
            index: None,
            snapshot: ViewerSnapshot::empty(),
            pending: None,
            renders: RenderCache::new(MAX_DECODE_BYTES),
            shutdown_requested: false,
        }
    }

    fn next_generation(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1).max(1);
        self.generation
    }

    fn next_revision(&mut self) -> u64 {
        self.revision = self.revision.wrapping_add(1).max(1);
        self.revision
    }

    fn schedule_current(&mut self) -> ViewerSnapshot {
        let index = self.index.expect("a selected catalog item");
        let path = self.files[index].clone();
        let generation = self.next_generation();
        let revision = self.next_revision();
        self.renders.clear();
        self.snapshot = ViewerSnapshot::loading(
            generation,
            revision,
            index,
            self.files.len(),
            display_name(&path),
        );
        // This assignment is the replaceable one-item pending queue. If a job
        // is currently decoding, repeated navigation overwrites only this slot
        // and drops the superseded handle. Opening here pins the selected file
        // identity before the worker can observe a path replacement.
        let source = open_read_only(&path);
        self.pending = Some(DecodeJob {
            generation,
            path,
            source,
        });
        self.snapshot.clone()
    }

    fn selected_path(&self) -> Option<&Path> {
        self.index
            .and_then(|index| self.files.get(index))
            .map(PathBuf::as_path)
    }
}

struct Inner {
    state: Mutex<ViewerState>,
    wake_worker: Condvar,
    decoder: Arc<dyn Decoder>,
    policy: DecodePolicy,
    event_sink: RwLock<Option<EventSink>>,
}

struct WorkerLifecycle {
    inner: Arc<Inner>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl WorkerLifecycle {
    fn shutdown(&self) {
        let Some(worker) = self.worker.lock().take() else {
            return;
        };
        {
            let mut state = self.inner.state.lock();
            state.shutdown_requested = true;
            state.pending = None;
            state.renders.clear();
        }
        // A helper decode can be blocked in native code or pipe I/O. Terminate
        // its constrained Job before joining the worker so shutdown remains
        // bounded.
        self.inner.decoder.shutdown();
        self.inner.wake_worker.notify_all();

        // The worker never owns WorkerLifecycle, so normal shutdown cannot run
        // on the worker itself. Keep this guard for defensive idempotence.
        if worker.thread().id() != thread::current().id()
            && let Err(payload) = worker.join()
        {
            eprintln!(
                "ImgViewer decode worker terminated unexpectedly: {}",
                panic_payload_name(payload.as_ref())
            );
        }
    }
}

impl Drop for WorkerLifecycle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Clone)]
pub struct ViewerController {
    inner: Arc<Inner>,
    lifecycle: Arc<WorkerLifecycle>,
}

impl Default for ViewerController {
    fn default() -> Self {
        Self::new()
    }
}

impl ViewerController {
    pub fn new() -> Self {
        Self::with_decoder(Arc::new(ProductionDecoder::default()))
    }

    fn with_decoder(decoder: Arc<dyn Decoder>) -> Self {
        Self::with_decoder_and_policy(decoder, DecodePolicy::default())
    }

    fn with_decoder_and_policy(decoder: Arc<dyn Decoder>, policy: DecodePolicy) -> Self {
        let inner = Arc::new(Inner {
            state: Mutex::new(ViewerState::new()),
            wake_worker: Condvar::new(),
            decoder,
            policy,
            event_sink: RwLock::new(None),
        });
        let worker_inner = Arc::clone(&inner);
        let worker = thread::Builder::new()
            .name("imgviewer-decode".to_owned())
            .spawn(move || decode_worker(worker_inner))
            .expect("failed to start the image decode worker");
        let lifecycle = Arc::new(WorkerLifecycle {
            inner: Arc::clone(&inner),
            worker: Mutex::new(Some(worker)),
        });
        Self { inner, lifecycle }
    }

    pub fn set_event_sink(&self, sink: impl Fn(ViewerSnapshot) + Send + Sync + 'static) {
        *self.inner.event_sink.write() = Some(Arc::new(sink));
    }

    pub fn open_path(&self, path: impl AsRef<Path>) -> ViewerSnapshot {
        let path = path.as_ref();
        match build_catalog(path) {
            Ok(Catalog { files, index }) => {
                let snapshot = {
                    let mut state = self.inner.state.lock();
                    if state.shutdown_requested {
                        return state.snapshot.clone();
                    }
                    // Keep the worker behind the state lock while cancelling.
                    // Otherwise a just-finished decode can take the newly
                    // published pending job before `cancel_current`, causing
                    // the cancellation intended for the old generation to
                    // terminate the new request instead.
                    self.inner.decoder.cancel_current();
                    state.files = files;
                    state.index = Some(index);
                    state.schedule_current()
                };
                self.inner.wake_worker.notify_one();
                snapshot
            }
            Err(error) => {
                let mut state = self.inner.state.lock();
                if state.shutdown_requested {
                    return state.snapshot.clone();
                }
                self.inner.decoder.cancel_current();
                let generation = state.next_generation();
                let revision = state.next_revision();
                state.files.clear();
                state.index = None;
                state.pending = None;
                state.renders.clear();
                state.snapshot = ViewerSnapshot::open_error(
                    generation,
                    revision,
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned()),
                    error,
                );
                state.snapshot.clone()
            }
        }
    }

    pub fn navigate(&self, direction: NavigationDirection) -> ViewerSnapshot {
        let mut scheduled = false;
        let snapshot = {
            let mut state = self.inner.state.lock();
            if state.shutdown_requested {
                return state.snapshot.clone();
            }
            let Some(current) = state.index else {
                return state.snapshot.clone();
            };
            let target = match direction {
                NavigationDirection::Previous if current > 0 => Some(current - 1),
                NavigationDirection::Next if current + 1 < state.files.len() => Some(current + 1),
                _ => None,
            };
            if let Some(target) = target {
                // Cancellation is ordered before the replacement job becomes
                // observable to the worker. See the corresponding open-path
                // ordering above.
                self.inner.decoder.cancel_current();
                state.index = Some(target);
                scheduled = true;
                state.schedule_current()
            } else {
                state.snapshot.clone()
            }
        };
        if scheduled {
            self.inner.wake_worker.notify_one();
        }
        snapshot
    }

    pub fn current_snapshot(&self) -> ViewerSnapshot {
        self.inner.state.lock().snapshot.clone()
    }

    pub fn take_render(&self, render_id: u64) -> Option<Vec<u8>> {
        self.inner.state.lock().renders.take(render_id)
    }

    /// Stops the decode worker and waits for an in-process decode to return.
    ///
    /// The deadline is intentionally a soft policy until native codecs run in
    /// a helper process: Rust cannot safely terminate a thread inside codec
    /// code. Calling this method is idempotent.
    pub fn shutdown(&self) {
        self.lifecycle.shutdown();
    }
}

fn decode_worker(inner: Arc<Inner>) {
    loop {
        let job = {
            let mut state = inner.state.lock();
            while state.pending.is_none() && !state.shutdown_requested {
                inner.wake_worker.wait(&mut state);
            }
            if state.shutdown_requested {
                return;
            }
            state.pending.take().expect("pending job after wait")
        };

        let is_current = {
            let state = inner.state.lock();
            !state.shutdown_requested
                && state.generation == job.generation
                && state.selected_path() == Some(job.path.as_path())
        };
        if !is_current {
            continue;
        }

        let DecodeJob {
            generation,
            path,
            source,
        } = job;
        let result = decode_with_policy(&inner, &path, source);
        let snapshot = {
            let mut state = inner.state.lock();
            if state.shutdown_requested {
                return;
            }
            if state.generation != generation || state.selected_path() != Some(path.as_path()) {
                // A newer cursor won the race. Do not write bytes, errors, or
                // tokens from this obsolete generation into shared state.
                None
            } else {
                let revision = state.next_revision();
                state.snapshot.revision = revision;
                match result {
                    Ok(decoded) => {
                        let descriptor = match state.renders.insert(decoded.bytes) {
                            Ok(render_id) => Some(RenderDescriptor {
                                render_id,
                                mime_type: decoded.mime_type.to_owned(),
                                width: decoded.width,
                                height: decoded.height,
                                animated: decoded.animated,
                            }),
                            Err(error) => {
                                state.snapshot.status = ViewerStatus::Error;
                                state.snapshot.render = None;
                                state.snapshot.error = Some(error);
                                None
                            }
                        };
                        if let Some(descriptor) = descriptor {
                            state.snapshot.status = ViewerStatus::Ready;
                            state.snapshot.render = Some(descriptor);
                            state.snapshot.error = None;
                        }
                    }
                    Err(error) => {
                        state.renders.clear();
                        state.snapshot.status = ViewerStatus::Error;
                        state.snapshot.render = None;
                        state.snapshot.error = Some(error);
                    }
                }
                Some(state.snapshot.clone())
            }
        };

        if let Some(snapshot) = snapshot {
            let sink = inner.event_sink.read().clone();
            if let Some(sink) = sink
                && catch_unwind(AssertUnwindSafe(|| sink(snapshot))).is_err()
            {
                eprintln!("ImgViewer ignored a panic from the snapshot event sink.");
            }
        }
    }
}

fn decode_with_policy(
    inner: &Inner,
    path: &Path,
    source: Result<File, ViewerError>,
) -> Result<DecodedRender, ViewerError> {
    let file = source?;
    // Queue latency belongs to an older in-process decode and must not consume
    // the latest image's own decode budget. The hard-isolation milestone will
    // enforce the same boundary in the helper process.
    let deadline = inner.policy.deadline_from(Instant::now());
    if deadline.is_expired(Instant::now()) {
        return Err(ViewerError::deadline_exceeded(deadline.limit_ms()));
    }

    let result = catch_unwind(AssertUnwindSafe(|| inner.decoder.decode(path, file)))
        .unwrap_or_else(|_| Err(ViewerError::decoder_panic()));

    if deadline.is_expired(Instant::now()) {
        Err(ViewerError::deadline_exceeded(deadline.limit_ms()))
    } else {
        result
    }
}

fn panic_payload_name(payload: &(dyn std::any::Any + Send)) -> &'static str {
    if payload.is::<&'static str>() {
        "static string panic"
    } else if payload.is::<String>() {
        "string panic"
    } else {
        "non-string panic"
    }
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ColorType, ImageEncoder};
    use std::fs::{self, File};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::time::{Duration, Instant};

    struct ControlledDecoder {
        started: Sender<String>,
        release_first: Mutex<Receiver<()>>,
    }

    impl Decoder for ControlledDecoder {
        fn decode(&self, path: &Path, _file: File) -> Result<DecodedRender, ViewerError> {
            let name = display_name(path);
            self.started.send(name.clone()).unwrap();
            if name == "1.jpg" {
                self.release_first
                    .lock()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
            }
            Ok(DecodedRender {
                bytes: name.as_bytes().to_vec(),
                mime_type: "image/jpeg",
                width: 1,
                height: 1,
                animated: false,
            })
        }
    }

    fn wait_until_ready(controller: &ViewerController) -> ViewerSnapshot {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = controller.current_snapshot();
            if snapshot.status != ViewerStatus::Loading {
                return snapshot;
            }
            assert!(Instant::now() < deadline, "decoder did not finish");
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn boundaries_stop_without_wrapping_or_advancing_generation() {
        let directory = tempfile::tempdir().unwrap();
        File::create(directory.path().join("1.jpg")).unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let controller = ViewerController::with_decoder(Arc::new(ControlledDecoder {
            started: started_tx,
            release_first: Mutex::new(release_rx),
        }));
        let first = controller.open_path(directory.path().join("1.jpg"));
        let stopped = controller.navigate(NavigationDirection::Previous);
        assert_eq!(first.generation, stopped.generation);
        assert_eq!(first.revision, stopped.revision);
        assert_eq!(stopped.index, Some(0));
        assert!(!stopped.can_previous);
        assert!(!stopped.can_next);
        // Keep the worker-side send from becoming a flaky disconnected error.
        let _ = started_rx.recv_timeout(Duration::from_secs(1));
        let _ = release_tx.send(());
    }

    #[test]
    fn rapid_navigation_discards_running_result_and_overwrites_pending_job() {
        let directory = tempfile::tempdir().unwrap();
        for name in ["1.jpg", "2.jpg", "3.jpg"] {
            File::create(directory.path().join(name)).unwrap();
        }
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let controller = ViewerController::with_decoder(Arc::new(ControlledDecoder {
            started: started_tx,
            release_first: Mutex::new(release_rx),
        }));

        let first = controller.open_path(directory.path().join("1.jpg"));
        assert_eq!(first.generation, 1);
        assert_eq!(first.revision, 1);
        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            "1.jpg"
        );
        let second = controller.navigate(NavigationDirection::Next);
        let third = controller.navigate(NavigationDirection::Next);
        assert_eq!(second.revision, 2);
        assert_eq!(third.revision, 3);
        assert_eq!(second.file_name.as_deref(), Some("2.jpg"));
        assert_eq!(third.file_name.as_deref(), Some("3.jpg"));
        release_tx.send(()).unwrap();

        // The pending 2.jpg job was replaced before the single worker became free.
        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            "3.jpg"
        );
        assert!(started_rx.recv_timeout(Duration::from_millis(50)).is_err());
        let ready = wait_until_ready(&controller);
        assert_eq!(ready.generation, third.generation);
        assert_eq!(ready.revision, 4);
        assert_eq!(ready.index, Some(2));
        assert_eq!(ready.file_name.as_deref(), Some("3.jpg"));
        let token = ready.render.unwrap().render_id;
        {
            let state = controller.inner.state.lock();
            assert_eq!(state.renders.entries.len(), 1);
            assert_eq!(state.renders.used, b"3.jpg".len() as u64);
        }
        assert_eq!(controller.take_render(token).unwrap(), b"3.jpg");
        assert_eq!(controller.inner.state.lock().renders.used, 0);
    }

    struct GatedProductionDecoder {
        started: Sender<String>,
        release_first: Mutex<Receiver<()>>,
    }

    impl Decoder for GatedProductionDecoder {
        fn decode(&self, path: &Path, file: File) -> Result<DecodedRender, ViewerError> {
            let name = display_name(path);
            self.started.send(name.clone()).unwrap();
            if name == "1.png" {
                self.release_first
                    .lock()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
            }
            ProductionDecoder::default().decode(path, file)
        }
    }

    fn test_png_bytes(color: [u8; 4]) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(&color, 1, 1, ColorType::Rgba8.into())
            .unwrap();
        bytes
    }

    #[test]
    fn scheduled_file_handle_survives_path_replacement_and_is_released_after_decode() {
        let directory = tempfile::tempdir().unwrap();
        let first_path = directory.path().join("1.png");
        let second_path = directory.path().join("2.png");
        let pinned_path = directory.path().join("pinned-original.bin");
        let selected_original = test_png_bytes([1, 2, 3, 255]);
        let path_replacement = test_png_bytes([200, 100, 50, 255]);
        fs::write(&first_path, test_png_bytes([9, 8, 7, 255])).unwrap();
        fs::write(&second_path, &selected_original).unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let controller = ViewerController::with_decoder(Arc::new(GatedProductionDecoder {
            started: started_tx,
            release_first: Mutex::new(release_rx),
        }));

        controller.open_path(&first_path);
        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            "1.png"
        );
        controller.navigate(NavigationDirection::Next);

        // navigate() has already opened 2.png read-only and placed that exact
        // handle in the pending job. Replacing the directory entry must not
        // redirect the worker to these newer bytes.
        fs::rename(&second_path, &pinned_path).unwrap();
        fs::write(&second_path, path_replacement).unwrap();
        release_tx.send(()).unwrap();

        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            "2.png"
        );
        let ready = wait_until_ready(&controller);
        assert_eq!(ready.status, ViewerStatus::Ready);
        assert_eq!(ready.file_name.as_deref(), Some("2.png"));
        assert_eq!(
            controller.take_render(ready.render.unwrap().render_id),
            Some(selected_original)
        );

        // The decoder consumes and drops the sole job handle before publishing
        // Ready, so the original file can now be removed on Windows.
        fs::remove_file(&pinned_path).unwrap();
    }

    #[test]
    fn render_tokens_are_one_time_and_cache_has_a_hard_limit() {
        let mut cache = RenderCache::new(4);
        let bytes = vec![1, 2, 3, 4];
        let allocation = bytes.as_ptr();
        let token = cache.insert(bytes).unwrap();
        assert!(token != 0);
        assert!(token <= MAX_SAFE_RENDER_ID);
        let taken = cache.take(token).unwrap();
        assert_eq!(taken, vec![1, 2, 3, 4]);
        assert_eq!(
            taken.as_ptr(),
            allocation,
            "render bytes must move, not copy"
        );
        assert_eq!(cache.take(token), None);
        assert_eq!(
            cache.insert(vec![0; 5]).unwrap_err().code,
            "cache_limit_exceeded"
        );
    }

    #[test]
    fn clearing_render_cache_releases_all_unread_tokens_and_accounting() {
        let mut cache = RenderCache::new(8);
        let first = cache.insert(vec![1, 2, 3]).unwrap();
        let second = cache.insert(vec![4, 5]).unwrap();
        assert_eq!(cache.entries.len(), 2);
        assert_eq!(cache.used, 5);

        cache.clear();

        assert!(cache.entries.is_empty());
        assert_eq!(cache.used, 0);
        assert_eq!(cache.take(first), None);
        assert_eq!(cache.take(second), None);
    }

    #[test]
    fn render_tokens_are_unique_and_exactly_representable_in_javascript() {
        let mut cache = RenderCache::new(8);
        let first = cache.insert(vec![1]).unwrap();
        let second = cache.insert(vec![2]).unwrap();
        assert_ne!(first, second);
        assert!((1..=MAX_SAFE_RENDER_ID).contains(&first));
        assert!((1..=MAX_SAFE_RENDER_ID).contains(&second));
    }

    struct FileNameDecoder;

    impl Decoder for FileNameDecoder {
        fn decode(&self, path: &Path, _file: File) -> Result<DecodedRender, ViewerError> {
            Ok(DecodedRender {
                bytes: display_name(path).into_bytes(),
                mime_type: "image/png",
                width: 1,
                height: 1,
                animated: false,
            })
        }
    }

    #[test]
    fn deleted_file_is_recoverable_and_navigation_continues() {
        let directory = tempfile::tempdir().unwrap();
        let first_path = directory.path().join("1.png");
        let second_path = directory.path().join("2.png");
        fs::write(&first_path, b"one").unwrap();
        fs::write(&second_path, b"two").unwrap();
        let controller = ViewerController::with_decoder(Arc::new(FileNameDecoder));
        controller.open_path(&first_path);
        assert_eq!(wait_until_ready(&controller).status, ViewerStatus::Ready);

        fs::remove_file(&second_path).unwrap();
        controller.navigate(NavigationDirection::Next);
        let error = wait_until_ready(&controller);
        assert_eq!(error.status, ViewerStatus::Error);
        assert!(error.can_previous);
        assert_eq!(error.file_name.as_deref(), Some("2.png"));
        assert_eq!(
            error.error.as_ref().map(|error| error.code.as_str()),
            Some(error_code::IO_ERROR)
        );

        controller.navigate(NavigationDirection::Previous);
        let recovered = wait_until_ready(&controller);
        assert_eq!(recovered.status, ViewerStatus::Ready);
        assert_eq!(recovered.file_name.as_deref(), Some("1.png"));
    }

    #[cfg(windows)]
    #[test]
    fn reparse_replacement_after_catalog_is_rejected_before_decoder_reads_target() {
        use std::os::windows::fs::symlink_file;

        const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;

        let directory = tempfile::tempdir().unwrap();
        let first_path = directory.path().join("1.png");
        let second_path = directory.path().join("2.png");
        let target_path = directory.path().join("private-target.bin");
        fs::write(&first_path, b"first").unwrap();
        fs::write(&second_path, b"ordinary-at-catalog-time").unwrap();
        fs::write(&target_path, b"must-not-reach-decoder").unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let controller = ViewerController::with_decoder(Arc::new(CountingDecoder {
            calls: Arc::clone(&calls),
        }));

        controller.open_path(&first_path);
        assert_eq!(wait_until_ready(&controller).status, ViewerStatus::Ready);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        fs::remove_file(&second_path).unwrap();
        match symlink_file(&target_path, &second_path) {
            Ok(()) => {}
            Err(error) if error.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD) => {
                eprintln!("skipping symlink assertion without Windows privilege: {error}");
                return;
            }
            Err(error) => panic!("failed to create test symlink: {error}"),
        }

        controller.navigate(NavigationDirection::Next);
        let rejected = wait_until_ready(&controller);
        assert_eq!(rejected.status, ViewerStatus::Error);
        assert_eq!(rejected.file_name.as_deref(), Some("2.png"));
        assert_eq!(
            rejected.error.as_ref().map(|error| error.code.as_str()),
            Some("reparse_point_not_allowed")
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "reparse target reached decoder code"
        );

        controller.navigate(NavigationDirection::Previous);
        assert_eq!(wait_until_ready(&controller).status, ViewerStatus::Ready);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[cfg(windows)]
    #[test]
    fn ancestor_directory_replacement_after_catalog_is_rejected_before_open() {
        use std::os::windows::fs::symlink_dir;
        use std::process::Command;

        const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;

        let root = tempfile::tempdir().unwrap();
        let catalog_directory = root.path().join("photos");
        let original_directory = root.path().join("photos-original");
        let redirect_directory = root.path().join("redirect");
        fs::create_dir(&catalog_directory).unwrap();
        fs::create_dir(&redirect_directory).unwrap();
        let first_path = catalog_directory.join("1.png");
        let second_path = catalog_directory.join("2.png");
        fs::write(&first_path, b"first").unwrap();
        fs::write(&second_path, b"ordinary-at-catalog-time").unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let controller = ViewerController::with_decoder(Arc::new(CountingDecoder {
            calls: Arc::clone(&calls),
        }));
        controller.open_path(&first_path);
        assert_eq!(wait_until_ready(&controller).status, ViewerStatus::Ready);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        fs::rename(&catalog_directory, &original_directory).unwrap();
        match symlink_dir(&redirect_directory, &catalog_directory) {
            Ok(()) => {}
            Err(error) if error.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD) => {
                // NTFS directory junctions exercise the same ancestor reparse
                // policy without requiring the symbolic-link privilege.
                let output = Command::new("cmd.exe")
                    .args(["/d", "/c", "mklink", "/J"])
                    .arg(&catalog_directory)
                    .arg(&redirect_directory)
                    .output()
                    .unwrap();
                if !output.status.success() {
                    fs::rename(&original_directory, &catalog_directory).unwrap();
                    panic!(
                        "failed to create test junction after symlink privilege error {error}: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
            }
            Err(error) => {
                fs::rename(&original_directory, &catalog_directory).unwrap();
                panic!("failed to create test directory symlink: {error}");
            }
        }

        controller.navigate(NavigationDirection::Next);
        let rejected = wait_until_ready(&controller);
        assert_eq!(rejected.status, ViewerStatus::Error);
        assert_eq!(rejected.file_name.as_deref(), Some("2.png"));
        assert_eq!(
            rejected.error.as_ref().map(|error| error.code.as_str()),
            Some("reparse_point_not_allowed")
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "ancestor reparse target reached decoder code"
        );

        fs::remove_dir(&catalog_directory).unwrap();
        fs::rename(&original_directory, &catalog_directory).unwrap();
    }

    #[test]
    fn navigating_invalidates_an_unread_render_token_immediately() {
        let directory = tempfile::tempdir().unwrap();
        let first_path = directory.path().join("1.png");
        let second_path = directory.path().join("2.png");
        fs::write(&first_path, b"one").unwrap();
        fs::write(&second_path, b"two").unwrap();
        let controller = ViewerController::with_decoder(Arc::new(FileNameDecoder));

        controller.open_path(&first_path);
        let first = wait_until_ready(&controller);
        let old_token = first.render.unwrap().render_id;
        controller.navigate(NavigationDirection::Next);

        assert_eq!(controller.take_render(old_token), None);
        let second = wait_until_ready(&controller);
        assert_eq!(second.file_name.as_deref(), Some("2.png"));
    }

    #[test]
    fn every_published_snapshot_transition_advances_revision_independently() {
        let directory = tempfile::tempdir().unwrap();
        let first_path = directory.path().join("1.png");
        let second_path = directory.path().join("2.png");
        fs::write(&first_path, b"one").unwrap();
        fs::write(&second_path, b"two").unwrap();
        let controller = ViewerController::with_decoder(Arc::new(FileNameDecoder));

        let first_loading = controller.open_path(&first_path);
        let first_ready = wait_until_ready(&controller);
        let second_loading = controller.navigate(NavigationDirection::Next);
        let second_ready = wait_until_ready(&controller);
        let stopped = controller.navigate(NavigationDirection::Next);

        assert_eq!(
            [
                first_loading.revision,
                first_ready.revision,
                second_loading.revision,
                second_ready.revision,
            ],
            [1, 2, 3, 4]
        );
        assert_eq!(first_loading.generation, first_ready.generation);
        assert_eq!(second_loading.generation, second_ready.generation);
        assert_eq!(stopped.revision, second_ready.revision);
        assert_eq!(stopped.generation, second_ready.generation);
    }

    #[test]
    fn open_errors_advance_generation_and_revision_without_exposing_a_path() {
        let controller = ViewerController::with_decoder(Arc::new(FileNameDecoder));
        let first = controller.open_path("unsupported.bmp");
        let second = controller.open_path("another.bmp");

        assert_eq!((first.generation, first.revision), (1, 1));
        assert_eq!((second.generation, second.revision), (2, 2));
        assert_eq!(second.file_name.as_deref(), Some("another.bmp"));
        assert!(
            second
                .error
                .as_ref()
                .is_some_and(|error| error.parameters.is_empty())
        );
    }

    struct PanicOnceDecoder {
        calls: AtomicUsize,
    }

    impl Decoder for PanicOnceDecoder {
        fn decode(&self, path: &Path, _file: File) -> Result<DecodedRender, ViewerError> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                panic!("simulated codec panic");
            }
            Ok(DecodedRender {
                bytes: display_name(path).into_bytes(),
                mime_type: "image/png",
                width: 1,
                height: 1,
                animated: false,
            })
        }
    }

    #[test]
    fn decoder_panic_becomes_a_recoverable_error_and_worker_survives() {
        let directory = tempfile::tempdir().unwrap();
        let first_path = directory.path().join("1.png");
        let second_path = directory.path().join("2.png");
        fs::write(&first_path, b"one").unwrap();
        fs::write(&second_path, b"two").unwrap();
        let controller = ViewerController::with_decoder(Arc::new(PanicOnceDecoder {
            calls: AtomicUsize::new(0),
        }));

        controller.open_path(&first_path);
        let failed = wait_until_ready(&controller);
        assert_eq!(failed.status, ViewerStatus::Error);
        assert_eq!(
            failed.error.as_ref().map(|error| error.code.as_str()),
            Some(error_code::DECODER_PANIC)
        );

        controller.navigate(NavigationDirection::Next);
        let recovered = wait_until_ready(&controller);
        assert_eq!(recovered.status, ViewerStatus::Ready);
        assert_eq!(recovered.file_name.as_deref(), Some("2.png"));
        assert_eq!(recovered.revision, 4);
    }

    struct CountingDecoder {
        calls: Arc<AtomicUsize>,
    }

    impl Decoder for CountingDecoder {
        fn decode(&self, path: &Path, _file: File) -> Result<DecodedRender, ViewerError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(DecodedRender {
                bytes: display_name(path).into_bytes(),
                mime_type: "image/png",
                width: 1,
                height: 1,
                animated: false,
            })
        }
    }

    #[test]
    fn expired_pending_jobs_fail_closed_and_latest_cursor_wins() {
        let directory = tempfile::tempdir().unwrap();
        let first_path = directory.path().join("1.png");
        let second_path = directory.path().join("2.png");
        fs::write(&first_path, b"one").unwrap();
        fs::write(&second_path, b"two").unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let controller = ViewerController::with_decoder_and_policy(
            Arc::new(CountingDecoder {
                calls: Arc::clone(&calls),
            }),
            DecodePolicy::with_max_decode_duration(Duration::ZERO),
        );

        controller.open_path(&first_path);
        let latest_loading = controller.navigate(NavigationDirection::Next);
        let latest = wait_until_ready(&controller);

        assert_eq!(latest.generation, latest_loading.generation);
        assert_eq!(latest.file_name.as_deref(), Some("2.png"));
        assert_eq!(latest.status, ViewerStatus::Error);
        let error = latest.error.unwrap();
        assert_eq!(error.code, error_code::DECODE_DEADLINE_EXCEEDED);
        assert_eq!(error.parameters["limitMs"], 0);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "an already-expired job must not enter decoder code"
        );
    }

    struct SlowDecoder;

    impl Decoder for SlowDecoder {
        fn decode(&self, path: &Path, _file: File) -> Result<DecodedRender, ViewerError> {
            thread::sleep(Duration::from_millis(15));
            Ok(DecodedRender {
                bytes: display_name(path).into_bytes(),
                mime_type: "image/png",
                width: 1,
                height: 1,
                animated: false,
            })
        }
    }

    #[test]
    fn decode_that_returns_after_soft_deadline_is_not_published() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("1.png");
        fs::write(&path, b"one").unwrap();
        let controller = ViewerController::with_decoder_and_policy(
            Arc::new(SlowDecoder),
            DecodePolicy::with_max_decode_duration(Duration::from_millis(1)),
        );

        controller.open_path(&path);
        let expired = wait_until_ready(&controller);

        assert_eq!(expired.status, ViewerStatus::Error);
        assert_eq!(
            expired.error.as_ref().map(|error| error.code.as_str()),
            Some(error_code::DECODE_DEADLINE_EXCEEDED)
        );
        assert!(expired.render.is_none());
    }

    #[test]
    fn stale_timeout_cannot_replace_a_newer_successful_decode() {
        let directory = tempfile::tempdir().unwrap();
        for name in ["1.jpg", "2.jpg"] {
            File::create(directory.path().join(name)).unwrap();
        }
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let controller = ViewerController::with_decoder_and_policy(
            Arc::new(ControlledDecoder {
                started: started_tx,
                release_first: Mutex::new(release_rx),
            }),
            DecodePolicy::with_max_decode_duration(Duration::from_millis(100)),
        );

        controller.open_path(directory.path().join("1.jpg"));
        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            "1.jpg"
        );
        let latest_loading = controller.navigate(NavigationDirection::Next);
        // This wait exhausts the first decode's budget while the latest job is
        // pending. The latest job must receive a fresh budget when it starts.
        thread::sleep(Duration::from_millis(125));
        release_tx.send(()).unwrap();

        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            "2.jpg"
        );
        let latest = wait_until_ready(&controller);
        assert_eq!(latest.status, ViewerStatus::Ready);
        assert_eq!(latest.generation, latest_loading.generation);
        assert_eq!(latest.file_name.as_deref(), Some("2.jpg"));
        assert_eq!(
            controller.take_render(latest.render.unwrap().render_id),
            Some(b"2.jpg".to_vec())
        );
    }

    struct LifecycleDecoder {
        cancels: Arc<AtomicUsize>,
        shutdowns: Arc<AtomicUsize>,
    }

    impl Decoder for LifecycleDecoder {
        fn decode(&self, path: &Path, _file: File) -> Result<DecodedRender, ViewerError> {
            Ok(DecodedRender {
                bytes: display_name(path).into_bytes(),
                mime_type: "image/png",
                width: 1,
                height: 1,
                animated: false,
            })
        }

        fn cancel_current(&self) {
            self.cancels.fetch_add(1, Ordering::SeqCst);
        }

        fn shutdown(&self) {
            self.shutdowns.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn selection_changes_cancel_active_decoder_and_shutdown_precedes_join() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("1.png");
        let second = directory.path().join("2.png");
        fs::write(&first, b"one").unwrap();
        fs::write(&second, b"two").unwrap();
        let cancels = Arc::new(AtomicUsize::new(0));
        let shutdowns = Arc::new(AtomicUsize::new(0));
        let controller = ViewerController::with_decoder(Arc::new(LifecycleDecoder {
            cancels: Arc::clone(&cancels),
            shutdowns: Arc::clone(&shutdowns),
        }));

        controller.open_path(&first);
        wait_until_ready(&controller);
        controller.navigate(NavigationDirection::Next);
        wait_until_ready(&controller);
        controller.navigate(NavigationDirection::Next);
        assert_eq!(
            cancels.load(Ordering::SeqCst),
            2,
            "opening and successful navigation cancel; a stopped boundary does not"
        );

        controller.open_path(directory.path().join("missing.bmp"));
        assert_eq!(cancels.load(Ordering::SeqCst), 3);
        controller.shutdown();
        controller.shutdown();
        assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
    }

    struct BlockingCancelDecoder {
        started: Sender<String>,
        release_first: Mutex<Receiver<()>>,
        cancel_calls: AtomicUsize,
        second_cancel_entered: Sender<()>,
        release_second_cancel: Mutex<Receiver<()>>,
    }

    impl Decoder for BlockingCancelDecoder {
        fn decode(&self, path: &Path, _file: File) -> Result<DecodedRender, ViewerError> {
            let name = display_name(path);
            self.started.send(name.clone()).unwrap();
            if name == "1.jpg" {
                self.release_first
                    .lock()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
            }
            Ok(DecodedRender {
                bytes: name.into_bytes(),
                mime_type: "image/jpeg",
                width: 1,
                height: 1,
                animated: false,
            })
        }

        fn cancel_current(&self) {
            let call = self.cancel_calls.fetch_add(1, Ordering::SeqCst) + 1;
            if call == 2 {
                self.second_cancel_entered.send(()).unwrap();
                self.release_second_cancel
                    .lock()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
            }
        }
    }

    #[test]
    fn replacement_job_is_not_observable_until_old_request_cancel_finishes() {
        let directory = tempfile::tempdir().unwrap();
        for name in ["1.jpg", "2.jpg"] {
            File::create(directory.path().join(name)).unwrap();
        }
        let (started_tx, started_rx) = mpsc::channel();
        let (release_first_tx, release_first_rx) = mpsc::channel();
        let (cancel_entered_tx, cancel_entered_rx) = mpsc::channel();
        let (release_cancel_tx, release_cancel_rx) = mpsc::channel();
        let controller = ViewerController::with_decoder(Arc::new(BlockingCancelDecoder {
            started: started_tx,
            release_first: Mutex::new(release_first_rx),
            cancel_calls: AtomicUsize::new(0),
            second_cancel_entered: cancel_entered_tx,
            release_second_cancel: Mutex::new(release_cancel_rx),
        }));

        controller.open_path(directory.path().join("1.jpg"));
        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            "1.jpg"
        );

        let navigation_controller = controller.clone();
        let navigation =
            thread::spawn(move || navigation_controller.navigate(NavigationDirection::Next));
        cancel_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        release_first_tx.send(()).unwrap();

        assert!(
            started_rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "the worker observed the replacement job before old-request cancellation completed"
        );
        release_cancel_tx.send(()).unwrap();
        let loading = navigation.join().unwrap();
        assert_eq!(loading.file_name.as_deref(), Some("2.jpg"));
        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            "2.jpg"
        );
        assert_eq!(wait_until_ready(&controller).status, ViewerStatus::Ready);
    }

    #[test]
    fn explicit_shutdown_joins_worker_and_releases_its_shared_state() {
        let controller = ViewerController::with_decoder(Arc::new(FileNameDecoder));
        let inner = Arc::downgrade(&controller.inner);

        controller.shutdown();
        controller.shutdown();
        drop(controller);

        assert!(
            inner.upgrade().is_none(),
            "worker or lifecycle retained ViewerState after shutdown"
        );
    }
}
