#![deny(unsafe_code)]

use std::fs::File;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use imgviewer_codec_protocol::{DecodeResponse, WireErrorCode};
use parking_lot::Mutex;

use crate::error::ViewerError;
use crate::model::DecodedRender;

#[cfg(windows)]
mod windows;

const DEFAULT_HARD_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_INPUT_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TransportError {
    Unavailable,
    Io,
    Protocol,
    Disconnected,
    Timeout,
    Cancelled,
}

trait SessionKiller: Send + Sync {
    fn kill(&self);
    fn is_killed(&self) -> bool;
}

trait HelperSession: Send {
    fn killer(&self) -> Arc<dyn SessionKiller>;

    fn transact(
        &mut self,
        file: File,
        request_id: u64,
        expected_length: u64,
        timeout: Duration,
    ) -> Result<DecodeResponse, TransportError>;

    #[cfg(all(test, windows))]
    fn terminate_process_for_test(&self) -> Result<(), TransportError> {
        Err(TransportError::Unavailable)
    }
}

trait SessionLauncher: Send + Sync {
    fn launch(&self, timeout: Duration) -> Result<Box<dyn HelperSession>, TransportError>;
}

struct ActiveRequest {
    request_id: u64,
    killer: Arc<dyn SessionKiller>,
}

pub(crate) struct HeifHelperClient {
    launcher: Arc<dyn SessionLauncher>,
    session: Mutex<Option<Box<dyn HelperSession>>>,
    active: Mutex<Option<ActiveRequest>>,
    cancel_epoch: AtomicU64,
    next_request_id: AtomicU64,
    hard_timeout: Duration,
}

impl Default for HeifHelperClient {
    fn default() -> Self {
        Self::new()
    }
}

impl HeifHelperClient {
    pub(crate) fn new() -> Self {
        Self::with_launcher(default_launcher(), DEFAULT_HARD_TIMEOUT)
    }

    fn with_launcher(launcher: Arc<dyn SessionLauncher>, hard_timeout: Duration) -> Self {
        Self {
            launcher,
            session: Mutex::new(None),
            active: Mutex::new(None),
            cancel_epoch: AtomicU64::new(0),
            next_request_id: AtomicU64::new(1),
            hard_timeout,
        }
    }

    pub(crate) fn decode(&self, file: File) -> Result<DecodedRender, ViewerError> {
        let expected_length = file
            .metadata()
            .map_err(|_| ViewerError::io("無法取得 HEIC/HEIF 檔案大小。"))?
            .len();
        if expected_length > MAX_INPUT_BYTES {
            return Err(
                ViewerError::limit("file_too_large", "檔案超過 256 MiB 上限。")
                    .with_parameter("maxBytes", MAX_INPUT_BYTES)
                    .with_parameter("observedBytes", expected_length),
            );
        }

        let started = Instant::now();
        let initial_epoch = self.cancel_epoch.load(Ordering::SeqCst);
        let request_id = self.next_request_id();
        let mut session_slot = self.session.lock();
        if session_slot.is_none() {
            let remaining = remaining(self.hard_timeout, started)?;
            let session = self
                .launcher
                .launch(remaining)
                .map_err(transport_viewer_error)?;
            *session_slot = Some(session);
        }

        let session = session_slot
            .as_mut()
            .expect("helper session was installed above");
        let killer = session.killer();
        {
            let mut active = self.active.lock();
            *active = Some(ActiveRequest {
                request_id,
                killer: Arc::clone(&killer),
            });
        }

        // Cancellation can happen while the process launcher is still
        // creating and constraining the helper. Rechecking after publishing
        // the kill switch closes that lost-cancel window.
        if self.cancel_epoch.load(Ordering::SeqCst) != initial_epoch {
            killer.kill();
            clear_active(&self.active, request_id);
            session_slot.take();
            return Err(transport_viewer_error(TransportError::Cancelled));
        }

        let result = remaining(self.hard_timeout, started).and_then(|timeout| {
            session
                .transact(file, request_id, expected_length, timeout)
                .map_err(transport_viewer_error)
        });
        let cancelled =
            killer.is_killed() || self.cancel_epoch.load(Ordering::SeqCst) != initial_epoch;
        clear_active(&self.active, request_id);

        if cancelled {
            killer.kill();
            session_slot.take();
            return Err(transport_viewer_error(TransportError::Cancelled));
        }

        match result {
            Ok(response) => response_to_render(response),
            Err(error) => {
                // Do not retry the same untrusted image. A subsequent
                // navigation will lazily create a clean helper session.
                killer.kill();
                session_slot.take();
                Err(error)
            }
        }
    }

    pub(crate) fn cancel_current(&self) {
        self.cancel_epoch.fetch_add(1, Ordering::SeqCst);
        let killer = self
            .active
            .lock()
            .as_ref()
            .map(|request| Arc::clone(&request.killer));
        if let Some(killer) = killer {
            killer.kill();
        }
    }

    pub(crate) fn shutdown(&self) {
        self.cancel_current();
        if let Some(session) = self.session.lock().take() {
            session.killer().kill();
        }
    }

    fn next_request_id(&self) -> u64 {
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        if request_id == 0 {
            self.next_request_id.fetch_add(1, Ordering::Relaxed)
        } else {
            request_id
        }
    }
}

impl Drop for HeifHelperClient {
    fn drop(&mut self) {
        if let Some(session) = self.session.get_mut().take() {
            session.killer().kill();
        }
    }
}

fn clear_active(active: &Mutex<Option<ActiveRequest>>, request_id: u64) {
    let mut active = active.lock();
    if active
        .as_ref()
        .is_some_and(|request| request.request_id == request_id)
    {
        active.take();
    }
}

fn remaining(limit: Duration, started: Instant) -> Result<Duration, ViewerError> {
    limit
        .checked_sub(started.elapsed())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| ViewerError::deadline_exceeded(limit.as_millis() as u64))
}

fn response_to_render(response: DecodeResponse) -> Result<DecodedRender, ViewerError> {
    match response {
        DecodeResponse::Success(success) => Ok(DecodedRender {
            bytes: success.png,
            mime_type: "image/png",
            width: success.width,
            height: success.height,
            animated: false,
        }),
        DecodeResponse::Error(error) => Err(wire_viewer_error(error.code, error.arg0, error.arg1)),
    }
}

fn wire_viewer_error(code: WireErrorCode, arg0: u64, arg1: u64) -> ViewerError {
    match code {
        WireErrorCode::CorruptImage => ViewerError::corrupt("HEIC/HEIF 圖片資料已損毀。"),
        WireErrorCode::FormatMismatch => {
            ViewerError::new("format_mismatch", "檔案內容與 HEIC/HEIF 格式不符。")
        }
        WireErrorCode::FileTooLarge => {
            ViewerError::limit("file_too_large", "檔案超過安全輸入上限。")
                .with_parameter("observedBytes", arg0)
                .with_parameter("maxBytes", arg1)
        }
        WireErrorCode::DimensionsExceeded => {
            ViewerError::limit("dimensions_exceeded", "圖片超過安全尺寸或像素上限。")
                .with_parameter("width", arg0)
                .with_parameter("height", arg1)
        }
        WireErrorCode::DecodeLimitExceeded => {
            ViewerError::limit("decode_limit_exceeded", "圖片解碼超過安全記憶體上限。")
                .with_parameter("observedBytes", arg0)
                .with_parameter("maxBytes", arg1)
        }
        WireErrorCode::UnsupportedBitDepth => {
            ViewerError::new("unsupported_bit_depth", "不支援這張 HEIC/HEIF 的位元深度。")
                .with_parameter("bitDepth", arg0)
        }
        WireErrorCode::UnsupportedColorProfile => ViewerError::new(
            "unsupported_color_profile",
            "不支援這張 HEIC/HEIF 的色彩描述。",
        ),
        WireErrorCode::IoError => ViewerError::io("HEIC/HEIF helper 無法讀取已開啟的檔案。"),
        WireErrorCode::InternalDecoderError => ViewerError::new(
            "codec_helper_internal_error",
            "HEIC/HEIF helper 發生內部錯誤。",
        ),
        WireErrorCode::NotImplemented => ViewerError::new(
            "codec_helper_not_ready",
            "HEIC/HEIF helper 尚未提供解碼功能。",
        ),
        WireErrorCode::InvalidHandle => ViewerError::new(
            "codec_helper_invalid_handle",
            "HEIC/HEIF helper 拒絕無效的唯讀檔案控制代碼。",
        ),
    }
}

fn transport_viewer_error(error: TransportError) -> ViewerError {
    match error {
        TransportError::Unavailable => ViewerError::new(
            "codec_helper_unavailable",
            "找不到或無法啟動 HEIC/HEIF helper。",
        ),
        TransportError::Io => {
            ViewerError::new("codec_helper_io_error", "無法與 HEIC/HEIF helper 通訊。")
        }
        TransportError::Protocol => ViewerError::new(
            "codec_helper_protocol_error",
            "HEIC/HEIF helper 回傳無效資料。",
        ),
        TransportError::Disconnected => {
            ViewerError::new("codec_helper_crashed", "HEIC/HEIF helper 已意外終止。")
        }
        TransportError::Timeout => {
            ViewerError::deadline_exceeded(DEFAULT_HARD_TIMEOUT.as_millis() as u64)
        }
        TransportError::Cancelled => {
            ViewerError::new("decode_cancelled", "圖片解碼已由較新的選取取代。")
        }
    }
}

#[cfg(windows)]
fn default_launcher() -> Arc<dyn SessionLauncher> {
    Arc::new(windows::WindowsLauncher)
}

#[cfg(not(windows))]
fn default_launcher() -> Arc<dyn SessionLauncher> {
    Arc::new(UnsupportedLauncher)
}

#[cfg(not(windows))]
struct UnsupportedLauncher;

#[cfg(not(windows))]
impl SessionLauncher for UnsupportedLauncher {
    fn launch(&self, _timeout: Duration) -> Result<Box<dyn HelperSession>, TransportError> {
        Err(TransportError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use imgviewer_codec_protocol::{DecodeError, DecodeSuccess};
    use std::collections::VecDeque;
    #[cfg(windows)]
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    #[cfg(windows)]
    use std::os::windows::fs::OpenOptionsExt;
    #[cfg(windows)]
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use std::sync::mpsc;
    use std::thread;
    #[cfg(windows)]
    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

    #[derive(Clone, Copy)]
    enum Behavior {
        Success,
        Crash,
        Timeout,
    }

    #[derive(Default)]
    struct MockKiller {
        killed: AtomicBool,
    }

    impl SessionKiller for MockKiller {
        fn kill(&self) {
            self.killed.store(true, Ordering::SeqCst);
        }

        fn is_killed(&self) -> bool {
            self.killed.load(Ordering::SeqCst)
        }
    }

    struct MockSession {
        behavior: Behavior,
        killer: Arc<MockKiller>,
    }

    impl HelperSession for MockSession {
        fn killer(&self) -> Arc<dyn SessionKiller> {
            self.killer.clone()
        }

        fn transact(
            &mut self,
            _file: File,
            request_id: u64,
            _expected_length: u64,
            _timeout: Duration,
        ) -> Result<DecodeResponse, TransportError> {
            match self.behavior {
                Behavior::Success => Ok(DecodeResponse::Success(DecodeSuccess {
                    request_id,
                    width: 1,
                    height: 1,
                    png: b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01".to_vec(),
                })),
                Behavior::Crash => Err(TransportError::Disconnected),
                Behavior::Timeout => Err(TransportError::Timeout),
            }
        }
    }

    struct MockLauncher {
        behaviors: Mutex<VecDeque<Behavior>>,
        launches: AtomicUsize,
        killers: Mutex<Vec<Arc<MockKiller>>>,
    }

    impl MockLauncher {
        fn new(behaviors: impl IntoIterator<Item = Behavior>) -> Self {
            Self {
                behaviors: Mutex::new(behaviors.into_iter().collect()),
                launches: AtomicUsize::new(0),
                killers: Mutex::new(Vec::new()),
            }
        }
    }

    impl SessionLauncher for MockLauncher {
        fn launch(&self, _timeout: Duration) -> Result<Box<dyn HelperSession>, TransportError> {
            self.launches.fetch_add(1, Ordering::SeqCst);
            let behavior = self
                .behaviors
                .lock()
                .pop_front()
                .ok_or(TransportError::Unavailable)?;
            let killer = Arc::new(MockKiller::default());
            self.killers.lock().push(Arc::clone(&killer));
            Ok(Box::new(MockSession { behavior, killer }))
        }
    }

    fn source_file() -> File {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"fixture").unwrap();
        file
    }

    #[test]
    fn crash_and_timeout_poison_the_session_and_next_decode_restarts_lazily() {
        for failure in [Behavior::Crash, Behavior::Timeout] {
            let launcher = Arc::new(MockLauncher::new([failure, Behavior::Success]));
            let client =
                HeifHelperClient::with_launcher(launcher.clone(), Duration::from_millis(100));

            let first = client.decode(source_file()).unwrap_err();
            let expected_code = match failure {
                Behavior::Crash => "codec_helper_crashed",
                Behavior::Timeout => "decode_deadline_exceeded",
                Behavior::Success => unreachable!(),
            };
            assert_eq!(first.code, expected_code);
            assert!(launcher.killers.lock()[0].is_killed());

            let second = client.decode(source_file()).unwrap();
            assert_eq!((second.width, second.height), (1, 1));
            assert_eq!(launcher.launches.load(Ordering::SeqCst), 2);
        }
    }

    struct BlockingLauncher {
        entered: Mutex<Option<mpsc::Sender<()>>>,
        release: Mutex<mpsc::Receiver<()>>,
        killer: Arc<MockKiller>,
    }

    impl SessionLauncher for BlockingLauncher {
        fn launch(&self, _timeout: Duration) -> Result<Box<dyn HelperSession>, TransportError> {
            if let Some(entered) = self.entered.lock().take() {
                entered.send(()).unwrap();
            }
            self.release.lock().recv().unwrap();
            Ok(Box::new(MockSession {
                behavior: Behavior::Success,
                killer: Arc::clone(&self.killer),
            }))
        }
    }

    #[test]
    fn cancel_epoch_closes_the_lost_cancel_window_during_launch() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let killer = Arc::new(MockKiller::default());
        let launcher = Arc::new(BlockingLauncher {
            entered: Mutex::new(Some(entered_tx)),
            release: Mutex::new(release_rx),
            killer: Arc::clone(&killer),
        });
        let client = Arc::new(HeifHelperClient::with_launcher(
            launcher,
            Duration::from_secs(2),
        ));
        let decode_client = Arc::clone(&client);
        let decode = thread::spawn(move || decode_client.decode(source_file()));

        entered_rx.recv().unwrap();
        client.cancel_current();
        release_tx.send(()).unwrap();
        let error = decode.join().unwrap().unwrap_err();
        assert_eq!(error.code, "decode_cancelled");
        assert!(killer.is_killed());
    }

    #[test]
    fn wire_errors_map_to_stable_sanitized_viewer_errors() {
        let response = DecodeResponse::Error(DecodeError {
            request_id: 1,
            code: WireErrorCode::DecodeLimitExceeded,
            arg0: 600,
            arg1: 512,
        });
        let error = response_to_render(response).unwrap_err();
        assert_eq!(error.code, "decode_limit_exceeded");
        assert_eq!(error.parameters["observedBytes"], 600);
        assert_eq!(error.parameters["maxBytes"], 512);
        assert!(!error.message.contains('\\'));
        assert!(!error.message.contains('/'));
    }

    #[cfg(windows)]
    #[derive(Default)]
    struct CountingWindowsLauncher {
        launches: AtomicUsize,
        killers: Mutex<Vec<Arc<dyn SessionKiller>>>,
    }

    #[cfg(windows)]
    impl SessionLauncher for CountingWindowsLauncher {
        fn launch(&self, timeout: Duration) -> Result<Box<dyn HelperSession>, TransportError> {
            self.launches.fetch_add(1, Ordering::SeqCst);
            let session = windows::WindowsLauncher.launch(timeout)?;
            self.killers.lock().push(session.killer());
            Ok(session)
        }
    }

    #[cfg(windows)]
    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures")
            .join(name)
    }

    #[cfg(windows)]
    fn decode_fixture_with_delete_guard(
        client: &HeifHelperClient,
        directory: &Path,
        copy_name: &str,
    ) -> Result<DecodedRender, ViewerError> {
        let path = directory.join(copy_name);
        fs::copy(fixture("primary-second.heic"), &path).unwrap();
        let file = OpenOptions::new()
            .read(true)
            // Omitting FILE_SHARE_DELETE makes removal a direct proof that
            // both the broker handle and its child duplicate were released.
            .share_mode(FILE_SHARE_READ)
            .open(&path)
            .unwrap();
        let result = client.decode(file);
        fs::remove_file(&path).expect("helper and broker must release the read-only file handle");
        result
    }

    #[cfg(windows)]
    fn assert_primary_second_render(render: &DecodedRender) {
        assert_eq!(render.mime_type, "image/png");
        assert_eq!((render.width, render.height), (3, 5));
        assert!(!render.animated);
        assert!(render.bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        let pixels = image::load_from_memory(&render.bytes).unwrap().to_rgb8();
        let primary_pixel = pixels.get_pixel(0, 0).0;
        assert!(
            primary_pixel[2] > 150
                && primary_pixel[2] > primary_pixel[0]
                && primary_pixel[2] > primary_pixel[1],
            "expected the blue designated primary item, got {primary_pixel:?}"
        );
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires a HEIC-enabled ImgViewer.CodecHelper.exe staged beside the test binary"]
    fn real_helper_process_decodes_persistently_and_recovers_after_crash() {
        let helper_path = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join(windows::TEST_HELPER_FILE_NAME);
        assert!(
            helper_path.is_file(),
            "stage the real helper at {}",
            helper_path.display()
        );

        let launcher = Arc::new(CountingWindowsLauncher::default());
        let client = HeifHelperClient::with_launcher(launcher.clone(), Duration::from_secs(10));
        let directory = tempfile::tempdir().unwrap();

        let first =
            decode_fixture_with_delete_guard(&client, directory.path(), "first-primary.heic")
                .unwrap();
        assert_primary_second_render(&first);
        let second =
            decode_fixture_with_delete_guard(&client, directory.path(), "second-primary.heic")
                .unwrap();
        assert_primary_second_render(&second);
        assert_eq!(
            launcher.launches.load(Ordering::SeqCst),
            1,
            "two successful requests must share one persistent helper"
        );

        client
            .session
            .lock()
            .as_ref()
            .expect("successful decodes retain the helper session")
            .terminate_process_for_test()
            .unwrap();
        let crash_error =
            decode_fixture_with_delete_guard(&client, directory.path(), "after-crash.heic")
                .unwrap_err();
        assert!(
            matches!(
                crash_error.code.as_str(),
                "codec_helper_io_error" | "codec_helper_crashed"
            ),
            "unexpected crash error: {}",
            crash_error.code
        );
        assert_eq!(
            launcher.launches.load(Ordering::SeqCst),
            1,
            "the failed image must not be retried automatically"
        );

        let recovered =
            decode_fixture_with_delete_guard(&client, directory.path(), "recovered.heic").unwrap();
        assert_primary_second_render(&recovered);
        assert_eq!(
            launcher.launches.load(Ordering::SeqCst),
            2,
            "the next decode must lazily launch a clean helper"
        );

        client.shutdown();
        assert!(
            launcher
                .killers
                .lock()
                .iter()
                .all(|killer| killer.is_killed()),
            "every helper Job Object must be terminated before test exit"
        );
    }
}
