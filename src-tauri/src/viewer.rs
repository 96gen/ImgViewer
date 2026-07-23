use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use parking_lot::{Condvar, Mutex, RwLock};
use rand::random;

use crate::catalog::{Catalog, build_catalog};
use crate::decode::{MAX_DECODE_BYTES, ProductionDecoder};
use crate::model::{
    DecodedRender, NavigationDirection, RenderDescriptor, ViewerError, ViewerSnapshot, ViewerStatus,
};

type EventSink = Arc<dyn Fn(ViewerSnapshot) + Send + Sync + 'static>;
const MAX_SAFE_RENDER_ID: u64 = (1_u64 << 53) - 1;

pub(crate) trait Decoder: Send + Sync + 'static {
    fn decode(&self, path: &Path) -> Result<DecodedRender, ViewerError>;
}

impl Decoder for ProductionDecoder {
    fn decode(&self, path: &Path) -> Result<DecodedRender, ViewerError> {
        ProductionDecoder::decode(self, path)
    }
}

#[derive(Clone, Debug)]
struct DecodeJob {
    generation: u64,
    path: PathBuf,
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
                "cache_limit_exceeded",
                "轉譯快取超過 512 MiB 上限。",
            ));
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
    files: Vec<PathBuf>,
    index: Option<usize>,
    snapshot: ViewerSnapshot,
    pending: Option<DecodeJob>,
    renders: RenderCache,
}

impl ViewerState {
    fn new() -> Self {
        Self {
            generation: 0,
            files: Vec::new(),
            index: None,
            snapshot: ViewerSnapshot::empty(),
            pending: None,
            renders: RenderCache::new(MAX_DECODE_BYTES),
        }
    }

    fn next_generation(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1).max(1);
        self.generation
    }

    fn schedule_current(&mut self) -> ViewerSnapshot {
        let index = self.index.expect("a selected catalog item");
        let path = self.files[index].clone();
        let generation = self.next_generation();
        self.renders.clear();
        self.snapshot =
            ViewerSnapshot::loading(generation, index, self.files.len(), display_name(&path));
        // This assignment is the replaceable one-item pending queue. If a job
        // is currently decoding, repeated navigation overwrites only this slot.
        self.pending = Some(DecodeJob { generation, path });
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
    event_sink: RwLock<Option<EventSink>>,
}

#[derive(Clone)]
pub struct ViewerController {
    inner: Arc<Inner>,
}

impl Default for ViewerController {
    fn default() -> Self {
        Self::new()
    }
}

impl ViewerController {
    pub fn new() -> Self {
        Self::with_decoder(Arc::new(ProductionDecoder))
    }

    fn with_decoder(decoder: Arc<dyn Decoder>) -> Self {
        let inner = Arc::new(Inner {
            state: Mutex::new(ViewerState::new()),
            wake_worker: Condvar::new(),
            decoder,
            event_sink: RwLock::new(None),
        });
        let worker_inner = Arc::clone(&inner);
        thread::Builder::new()
            .name("imgviewer-decode".to_owned())
            .spawn(move || decode_worker(worker_inner))
            .expect("failed to start the image decode worker");
        Self { inner }
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
                    state.files = files;
                    state.index = Some(index);
                    state.schedule_current()
                };
                self.inner.wake_worker.notify_one();
                snapshot
            }
            Err(error) => {
                let mut state = self.inner.state.lock();
                let generation = state.next_generation();
                state.files.clear();
                state.index = None;
                state.pending = None;
                state.renders.clear();
                state.snapshot = ViewerSnapshot::open_error(
                    generation,
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
            let Some(current) = state.index else {
                return state.snapshot.clone();
            };
            let target = match direction {
                NavigationDirection::Previous if current > 0 => Some(current - 1),
                NavigationDirection::Next if current + 1 < state.files.len() => Some(current + 1),
                _ => None,
            };
            if let Some(target) = target {
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
}

fn decode_worker(inner: Arc<Inner>) {
    loop {
        let job = {
            let mut state = inner.state.lock();
            while state.pending.is_none() {
                inner.wake_worker.wait(&mut state);
            }
            state.pending.take().expect("pending job after wait")
        };

        let result = inner.decoder.decode(&job.path);
        let snapshot = {
            let mut state = inner.state.lock();
            if state.generation != job.generation
                || state.selected_path() != Some(job.path.as_path())
            {
                // A newer cursor won the race. Do not write bytes, errors, or
                // tokens from this obsolete generation into shared state.
                None
            } else {
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
            if let Some(sink) = sink {
                sink(snapshot);
            }
        }
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
    use std::fs::{self, File};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::time::{Duration, Instant};

    struct ControlledDecoder {
        started: Sender<String>,
        release_first: Mutex<Receiver<()>>,
    }

    impl Decoder for ControlledDecoder {
        fn decode(&self, path: &Path) -> Result<DecodedRender, ViewerError> {
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
        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            "1.jpg"
        );
        let second = controller.navigate(NavigationDirection::Next);
        let third = controller.navigate(NavigationDirection::Next);
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
        fn decode(&self, path: &Path) -> Result<DecodedRender, ViewerError> {
            if !path.exists() {
                return Err(ViewerError::io("檔案已被刪除。"));
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

        controller.navigate(NavigationDirection::Previous);
        let recovered = wait_until_ready(&controller);
        assert_eq!(recovered.status, ViewerStatus::Ready);
        assert_eq!(recovered.file_name.as_deref(), Some("1.png"));
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
}
