#![deny(unsafe_code)]

use std::fs::File;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use imgviewer_codec_core::{DecodedRgba8, encode_rgba8_png_checked};
use imgviewer_codec_protocol::{
    CODEC_HELPER_DECODE_DEADLINE_MS, CodecFormat, DecodeResponse, WireErrorCode,
};
use parking_lot::Mutex;

use crate::error::ViewerError;
use crate::model::DecodedRender;

#[cfg(windows)]
mod windows;

const DEFAULT_HARD_TIMEOUT: Duration = Duration::from_millis(CODEC_HELPER_DECODE_DEADLINE_MS);
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
        format: CodecFormat,
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

pub(crate) struct CodecHelperClient {
    launcher: Arc<dyn SessionLauncher>,
    session: Mutex<Option<Box<dyn HelperSession>>>,
    active: Mutex<Option<ActiveRequest>>,
    cancel_epoch: AtomicU64,
    next_request_id: AtomicU64,
    hard_timeout: Duration,
}

impl Default for CodecHelperClient {
    fn default() -> Self {
        Self::new()
    }
}

impl CodecHelperClient {
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

    pub(crate) fn decode(
        &self,
        format: CodecFormat,
        file: File,
    ) -> Result<DecodedRender, ViewerError> {
        self.decode_with_renderer(format, file, response_to_render_checked)
    }

    fn decode_with_renderer(
        &self,
        format: CodecFormat,
        file: File,
        render_response: impl FnOnce(
            DecodeResponse,
            &mut dyn FnMut() -> Result<(), ViewerError>,
        ) -> Result<DecodedRender, ViewerError>,
    ) -> Result<DecodedRender, ViewerError> {
        let expected_length = file
            .metadata()
            .map_err(|_| ViewerError::io("無法取得圖片檔案大小。"))?
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
        if session_slot
            .as_ref()
            .is_some_and(|session| session.killer().is_killed())
        {
            // A cancellation can linearize immediately after a completed
            // request. Never hand that already-terminated Job to a newer
            // selection; no image transaction has started, so a clean launch
            // here is not a retry of untrusted input.
            session_slot.take();
        }
        if session_slot.is_none() {
            let remaining = remaining(self.hard_timeout, started)?;
            let session = self
                .launcher
                .launch(remaining)
                .map_err(|error| transport_viewer_error(error, self.hard_timeout))?;
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
            return Err(transport_viewer_error(
                TransportError::Cancelled,
                self.hard_timeout,
            ));
        }

        let transport_result = remaining(self.hard_timeout, started).and_then(|timeout| {
            session
                .transact(format, file, request_id, expected_length, timeout)
                .map_err(|error| transport_viewer_error(error, self.hard_timeout))
        });
        let (render_result, transport_failed) = match transport_result {
            Ok(response) => {
                let mut checkpoint = || {
                    if self.cancel_epoch.load(Ordering::SeqCst) != initial_epoch {
                        return Err(transport_viewer_error(
                            TransportError::Cancelled,
                            self.hard_timeout,
                        ));
                    }
                    remaining(self.hard_timeout, started).map(|_| ())
                };
                (
                    catch_unwind(AssertUnwindSafe(|| {
                        render_response(response, &mut checkpoint)
                    }))
                    .unwrap_or_else(|_| Err(ViewerError::decoder_panic())),
                    false,
                )
            }
            Err(error) => (Err(error), true),
        };

        // `cancel_current` takes this same lock before advancing the epoch.
        // This makes completion and cancellation a linearizable boundary: a
        // cancel either owns this request and poisons its Job, or observes that
        // the request has already completed and leaves the reusable session
        // alive for the next generation.
        let cancelled = {
            let mut active = self.active.lock();
            let cancelled = self.cancel_epoch.load(Ordering::SeqCst) != initial_epoch;
            if active
                .as_ref()
                .is_some_and(|request| request.request_id == request_id)
            {
                active.take();
            }
            cancelled
        };
        let timed_out = started.elapsed() >= self.hard_timeout;
        let session_killed = killer.is_killed();
        if cancelled || timed_out || transport_failed || session_killed {
            // Do not retry the same untrusted image. A subsequent navigation
            // lazily creates a clean helper session. The renderer is included
            // in both the deadline and cancellation interval, so even a valid
            // helper response cannot publish after either boundary.
            killer.kill();
            session_slot.take();
        }

        if cancelled {
            return Err(transport_viewer_error(
                TransportError::Cancelled,
                self.hard_timeout,
            ));
        }
        if timed_out {
            return Err(transport_viewer_error(
                TransportError::Timeout,
                self.hard_timeout,
            ));
        }
        render_result
    }

    pub(crate) fn cancel_current(&self) {
        let killer = {
            let active = self.active.lock();
            self.cancel_epoch.fetch_add(1, Ordering::SeqCst);
            active.as_ref().map(|request| Arc::clone(&request.killer))
        };
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

impl Drop for CodecHelperClient {
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

#[cfg(test)]
fn response_to_render(response: DecodeResponse) -> Result<DecodedRender, ViewerError> {
    response_to_render_checked(response, &mut || Ok(()))
}

fn response_to_render_checked(
    response: DecodeResponse,
    check: &mut dyn FnMut() -> Result<(), ViewerError>,
) -> Result<DecodedRender, ViewerError> {
    match response {
        DecodeResponse::Success(success) => encode_rgba8_png_checked(
            DecodedRgba8 {
                rgba: success.rgba,
                width: success.width,
                height: success.height,
            },
            check,
        ),
        DecodeResponse::Error(error) => Err(wire_viewer_error(error.code, error.arg0, error.arg1)),
    }
}

fn wire_viewer_error(code: WireErrorCode, arg0: u64, arg1: u64) -> ViewerError {
    match code {
        WireErrorCode::CorruptImage => ViewerError::corrupt("圖片資料已損毀。"),
        WireErrorCode::FormatMismatch => {
            ViewerError::new("format_mismatch", "檔案內容與指定的圖片格式不符。")
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
            ViewerError::new("unsupported_bit_depth", "不支援這張圖片的位元深度。")
                .with_parameter("bitDepth", arg0)
        }
        WireErrorCode::UnsupportedColorProfile => {
            ViewerError::new("unsupported_color_profile", "不支援這張圖片的色彩描述。")
        }
        WireErrorCode::IoError => ViewerError::io("圖片解碼 helper 無法讀取已開啟的檔案。"),
        WireErrorCode::InternalDecoderError => ViewerError::new(
            "codec_helper_internal_error",
            "圖片解碼 helper 發生內部錯誤。",
        ),
        WireErrorCode::NotImplemented => ViewerError::new(
            "codec_helper_not_ready",
            "圖片解碼 helper 尚未提供這種格式的解碼功能。",
        ),
        WireErrorCode::InvalidHandle => ViewerError::new(
            "codec_helper_invalid_handle",
            "圖片解碼 helper 拒絕無效的唯讀檔案控制代碼。",
        ),
    }
}

fn transport_viewer_error(error: TransportError, hard_timeout: Duration) -> ViewerError {
    match error {
        TransportError::Unavailable => ViewerError::new(
            "codec_helper_unavailable",
            "找不到或無法啟動圖片解碼 helper。",
        ),
        TransportError::Io => {
            ViewerError::new("codec_helper_io_error", "無法與圖片解碼 helper 通訊。")
        }
        TransportError::Protocol => ViewerError::new(
            "codec_helper_protocol_error",
            "圖片解碼 helper 回傳無效資料。",
        ),
        TransportError::Disconnected => {
            ViewerError::new("codec_helper_crashed", "圖片解碼 helper 已意外終止。")
        }
        TransportError::Timeout => ViewerError::deadline_exceeded(hard_timeout.as_millis() as u64),
        TransportError::Cancelled => {
            ViewerError::new("decode_cancelled", "圖片解碼已由較新的選取取代。")
        }
    }
}

#[cfg(windows)]
fn default_launcher() -> Arc<dyn SessionLauncher> {
    Arc::new(windows::WindowsLauncher::production())
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
        formats: Arc<Mutex<Vec<CodecFormat>>>,
    }

    impl HelperSession for MockSession {
        fn killer(&self) -> Arc<dyn SessionKiller> {
            self.killer.clone()
        }

        fn transact(
            &mut self,
            format: CodecFormat,
            _file: File,
            request_id: u64,
            _expected_length: u64,
            _timeout: Duration,
        ) -> Result<DecodeResponse, TransportError> {
            self.formats.lock().push(format);
            match self.behavior {
                Behavior::Success => Ok(DecodeResponse::Success(DecodeSuccess {
                    request_id,
                    width: 1,
                    height: 1,
                    rgba: vec![12, 34, 56, 255],
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
        formats: Arc<Mutex<Vec<CodecFormat>>>,
    }

    impl MockLauncher {
        fn new(behaviors: impl IntoIterator<Item = Behavior>) -> Self {
            Self {
                behaviors: Mutex::new(behaviors.into_iter().collect()),
                launches: AtomicUsize::new(0),
                killers: Mutex::new(Vec::new()),
                formats: Arc::new(Mutex::new(Vec::new())),
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
            Ok(Box::new(MockSession {
                behavior,
                killer,
                formats: Arc::clone(&self.formats),
            }))
        }
    }

    fn source_file() -> File {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"fixture").unwrap();
        file
    }

    fn four_row_success(response: DecodeResponse) -> DecodeResponse {
        match response {
            DecodeResponse::Success(mut success) => {
                success.height = 4;
                success.rgba = [12, 34, 56, 255].repeat(4);
                DecodeResponse::Success(success)
            }
            error => error,
        }
    }

    #[test]
    fn production_helper_timeout_is_exactly_thirty_seconds() {
        assert_eq!(DEFAULT_HARD_TIMEOUT, Duration::from_secs(30));
    }

    #[test]
    fn crash_and_timeout_poison_the_session_and_next_decode_restarts_lazily() {
        for failure in [Behavior::Crash, Behavior::Timeout] {
            let launcher = Arc::new(MockLauncher::new([failure, Behavior::Success]));
            let client =
                CodecHelperClient::with_launcher(launcher.clone(), Duration::from_millis(100));

            let first = client.decode(CodecFormat::Heif, source_file()).unwrap_err();
            let expected_code = match failure {
                Behavior::Crash => "codec_helper_crashed",
                Behavior::Timeout => "decode_deadline_exceeded",
                Behavior::Success => unreachable!(),
            };
            assert_eq!(first.code, expected_code);
            if matches!(failure, Behavior::Timeout) {
                assert_eq!(first.parameters["limitMs"], 100);
            }
            assert!(launcher.killers.lock()[0].is_killed());

            let second = client.decode(CodecFormat::Tiff, source_file()).unwrap();
            assert_eq!((second.width, second.height), (1, 1));
            assert_eq!(launcher.launches.load(Ordering::SeqCst), 2);
        }
    }

    #[test]
    fn heif_and_tiff_share_one_persistent_session_and_forward_the_format() {
        let launcher = Arc::new(MockLauncher::new([Behavior::Success]));
        let client = CodecHelperClient::with_launcher(launcher.clone(), Duration::from_millis(100));

        client.decode(CodecFormat::Tiff, source_file()).unwrap();
        client.decode(CodecFormat::Heif, source_file()).unwrap();

        assert_eq!(launcher.launches.load(Ordering::SeqCst), 1);
        assert_eq!(
            launcher.formats.lock().as_slice(),
            &[CodecFormat::Tiff, CodecFormat::Heif]
        );
    }

    #[test]
    fn killed_idle_session_is_replaced_before_a_new_image_transaction() {
        let launcher = Arc::new(MockLauncher::new([Behavior::Success, Behavior::Success]));
        let client = CodecHelperClient::with_launcher(launcher.clone(), Duration::from_millis(100));

        client.decode(CodecFormat::Tiff, source_file()).unwrap();
        launcher.killers.lock()[0].kill();

        let recovered = client.decode(CodecFormat::Heif, source_file()).unwrap();
        assert_eq!((recovered.width, recovered.height), (1, 1));
        assert_eq!(launcher.launches.load(Ordering::SeqCst), 2);
        assert!(!launcher.killers.lock()[1].is_killed());
    }

    #[test]
    fn cancellation_interrupts_trusted_render_between_png_rows() {
        let launcher = Arc::new(MockLauncher::new([Behavior::Success]));
        let client = Arc::new(CodecHelperClient::with_launcher(
            launcher.clone(),
            Duration::from_secs(1),
        ));
        let (renderer_entered_tx, renderer_entered_rx) = mpsc::channel();
        let (release_renderer_tx, release_renderer_rx) = mpsc::channel();
        let checkpoint_count = Arc::new(AtomicUsize::new(0));
        let decode_checkpoint_count = Arc::clone(&checkpoint_count);
        let decode_client = Arc::clone(&client);
        let decode = thread::spawn(move || {
            decode_client.decode_with_renderer(
                CodecFormat::Tiff,
                source_file(),
                move |response, client_check| {
                    let mut row_check = || {
                        let call = decode_checkpoint_count.fetch_add(1, Ordering::SeqCst) + 1;
                        if call == 3 {
                            renderer_entered_tx.send(()).unwrap();
                            release_renderer_rx
                                .recv_timeout(Duration::from_secs(5))
                                .unwrap();
                        }
                        client_check()
                    };
                    response_to_render_checked(four_row_success(response), &mut row_check)
                },
            )
        });

        renderer_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        client.cancel_current();
        release_renderer_tx.send(()).unwrap();

        let error = decode.join().unwrap().unwrap_err();
        assert_eq!(error.code, "decode_cancelled");
        assert_eq!(checkpoint_count.load(Ordering::SeqCst), 3);
        assert!(launcher.killers.lock()[0].is_killed());
        assert!(client.session.lock().is_none());
    }

    #[test]
    fn hard_deadline_interrupts_trusted_render_between_png_rows() {
        let launcher = Arc::new(MockLauncher::new([Behavior::Success]));
        let client = Arc::new(CodecHelperClient::with_launcher(
            launcher.clone(),
            Duration::from_millis(250),
        ));
        let (renderer_entered_tx, renderer_entered_rx) = mpsc::channel();
        let (release_renderer_tx, release_renderer_rx) = mpsc::channel();
        let checkpoint_count = Arc::new(AtomicUsize::new(0));
        let decode_checkpoint_count = Arc::clone(&checkpoint_count);
        let decode_client = Arc::clone(&client);
        let decode = thread::spawn(move || {
            decode_client.decode_with_renderer(
                CodecFormat::Heif,
                source_file(),
                move |response, client_check| {
                    let mut row_check = || {
                        let call = decode_checkpoint_count.fetch_add(1, Ordering::SeqCst) + 1;
                        if call == 3 {
                            renderer_entered_tx.send(()).unwrap();
                            release_renderer_rx
                                .recv_timeout(Duration::from_secs(5))
                                .unwrap();
                            return client_check();
                        }
                        Ok(())
                    };
                    response_to_render_checked(four_row_success(response), &mut row_check)
                },
            )
        });

        renderer_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        thread::sleep(Duration::from_millis(300));
        release_renderer_tx.send(()).unwrap();

        let error = decode.join().unwrap().unwrap_err();
        assert_eq!(error.code, "decode_deadline_exceeded");
        assert_eq!(error.parameters["limitMs"], 250);
        assert_eq!(checkpoint_count.load(Ordering::SeqCst), 3);
        assert!(launcher.killers.lock()[0].is_killed());
        assert!(client.session.lock().is_none());
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
                formats: Arc::new(Mutex::new(Vec::new())),
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
        let client = Arc::new(CodecHelperClient::with_launcher(
            launcher,
            Duration::from_secs(2),
        ));
        let decode_client = Arc::clone(&client);
        let decode = thread::spawn(move || decode_client.decode(CodecFormat::Heif, source_file()));

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
    const FAULT_HELPER_FILE_NAME: &str = "ImgViewer.CodecFaultHelper.exe";
    #[cfg(windows)]
    const FAULT_JOB_MEMORY_LIMIT_BYTES: usize = 128 * 1024 * 1024;

    #[cfg(windows)]
    struct CountingWindowsLauncher {
        inner: windows::WindowsLauncher,
        launches: AtomicUsize,
        killers: Mutex<Vec<Arc<dyn SessionKiller>>>,
    }

    #[cfg(windows)]
    impl Default for CountingWindowsLauncher {
        fn default() -> Self {
            Self::new(windows::WindowsLauncher::production())
        }
    }

    #[cfg(windows)]
    impl CountingWindowsLauncher {
        fn new(inner: windows::WindowsLauncher) -> Self {
            Self {
                inner,
                launches: AtomicUsize::new(0),
                killers: Mutex::new(Vec::new()),
            }
        }
    }

    #[cfg(windows)]
    impl SessionLauncher for CountingWindowsLauncher {
        fn launch(&self, timeout: Duration) -> Result<Box<dyn HelperSession>, TransportError> {
            self.launches.fetch_add(1, Ordering::SeqCst);
            let session = self.inner.launch(timeout)?;
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
    fn decode_path_with_delete_guard(
        client: &CodecHelperClient,
        format: CodecFormat,
        path: &Path,
    ) -> Result<DecodedRender, ViewerError> {
        let file = OpenOptions::new()
            .read(true)
            // Omitting FILE_SHARE_DELETE makes removal a direct proof that
            // both the broker handle and its child duplicate were released.
            .share_mode(FILE_SHARE_READ)
            .open(path)
            .unwrap();
        let result = client.decode(format, file);
        fs::remove_file(path).expect("helper and broker must release the read-only file handle");
        result
    }

    #[cfg(windows)]
    fn decode_fixture_with_delete_guard(
        client: &CodecHelperClient,
        format: CodecFormat,
        directory: &Path,
        fixture_name: &str,
        copy_name: &str,
    ) -> Result<DecodedRender, ViewerError> {
        let path = directory.join(copy_name);
        fs::copy(fixture(fixture_name), &path).unwrap();
        decode_path_with_delete_guard(client, format, &path)
    }

    #[cfg(windows)]
    fn decode_marker_with_delete_guard(
        client: &CodecHelperClient,
        directory: &Path,
        marker: &[u8],
        copy_name: &str,
    ) -> Result<DecodedRender, ViewerError> {
        let path = directory.join(copy_name);
        fs::write(&path, marker).unwrap();
        decode_path_with_delete_guard(client, CodecFormat::Tiff, &path)
    }

    #[cfg(windows)]
    fn assert_png_render(render: &DecodedRender, dimensions: (u32, u32)) {
        assert_eq!(render.mime_type, "image/png");
        assert_eq!((render.width, render.height), dimensions);
        assert!(!render.animated);
        assert!(render.bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        let decoded = image::load_from_memory(&render.bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), dimensions);
    }

    #[cfg(windows)]
    fn assert_primary_second_render(render: &DecodedRender) {
        assert_png_render(render, (3, 5));
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
    fn assert_all_helpers_killed(launcher: &CountingWindowsLauncher) {
        assert!(
            launcher
                .killers
                .lock()
                .iter()
                .all(|killer| killer.is_killed()),
            "every helper Job Object must be terminated before test exit"
        );
    }

    #[cfg(windows)]
    fn assert_staged_helper(file_name: &str) {
        let helper_path = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join(file_name);
        assert!(
            helper_path.is_file(),
            "stage the real helper at {}",
            helper_path.display()
        );
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires a HEIC+TIFF ImgViewer.CodecHelper.exe staged beside the test binary"]
    fn real_helper_process_decodes_persistently_and_recovers_after_crash() {
        assert_staged_helper(windows::TEST_HELPER_FILE_NAME);

        let launcher = Arc::new(CountingWindowsLauncher::default());
        let client = CodecHelperClient::with_launcher(launcher.clone(), Duration::from_secs(10));
        let directory = tempfile::tempdir().unwrap();

        let first_tiff = decode_fixture_with_delete_guard(
            &client,
            CodecFormat::Tiff,
            directory.path(),
            "two-page.tiff",
            "first.tiff",
        )
        .unwrap();
        assert_png_render(&first_tiff, (5, 3));
        let heif = decode_fixture_with_delete_guard(
            &client,
            CodecFormat::Heif,
            directory.path(),
            "primary-second.heic",
            "middle.heic",
        )
        .unwrap();
        assert_primary_second_render(&heif);
        let second_tiff = decode_fixture_with_delete_guard(
            &client,
            CodecFormat::Tiff,
            directory.path(),
            "two-page.tiff",
            "second.tiff",
        )
        .unwrap();
        assert_png_render(&second_tiff, (5, 3));
        assert_eq!(
            launcher.launches.load(Ordering::SeqCst),
            1,
            "TIFF -> HEIF -> TIFF must share one persistent helper"
        );

        for cycle in 0..20 {
            let launches_before_failure = launcher.launches.load(Ordering::SeqCst);
            client
                .session
                .lock()
                .as_ref()
                .expect("successful decodes retain the helper session")
                .terminate_process_for_test()
                .unwrap();

            let crash_error = decode_fixture_with_delete_guard(
                &client,
                CodecFormat::Tiff,
                directory.path(),
                "two-page.tiff",
                &format!("crash-{cycle}.tiff"),
            )
            .unwrap_err();
            assert_eq!(crash_error.code, "codec_helper_crashed");
            assert_eq!(
                launcher.launches.load(Ordering::SeqCst),
                launches_before_failure,
                "the failed image must not be retried automatically in cycle {cycle}"
            );

            let recovered = decode_fixture_with_delete_guard(
                &client,
                CodecFormat::Tiff,
                directory.path(),
                "two-page.tiff",
                &format!("recovered-{cycle}.tiff"),
            )
            .unwrap();
            assert_png_render(&recovered, (5, 3));
            assert_eq!(
                launcher.launches.load(Ordering::SeqCst),
                launches_before_failure + 1,
                "the next request must lazily launch a clean helper in cycle {cycle}"
            );
        }

        client.shutdown();
        assert_all_helpers_killed(&launcher);
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires ImgViewer.CodecFaultHelper.exe staged beside the test binary"]
    fn real_fault_helper_hang_times_out_once_then_recovers_lazily() {
        assert_staged_helper(FAULT_HELPER_FILE_NAME);
        let launcher = Arc::new(CountingWindowsLauncher::new(
            windows::WindowsLauncher::for_test(
                FAULT_HELPER_FILE_NAME,
                FAULT_JOB_MEMORY_LIMIT_BYTES,
            ),
        ));
        let client = CodecHelperClient::with_launcher(launcher.clone(), Duration::from_secs(1));
        let directory = tempfile::tempdir().unwrap();

        let error = decode_marker_with_delete_guard(
            &client,
            directory.path(),
            b"IMGVIEWER_FAULT_HANG_V1",
            "hang.tiff",
        )
        .unwrap_err();
        assert_eq!(error.code, "decode_deadline_exceeded");
        assert_eq!(launcher.launches.load(Ordering::SeqCst), 1);

        let recovered = decode_marker_with_delete_guard(
            &client,
            directory.path(),
            b"IMGVIEWER_FAULT_OK_TIFF_V1",
            "hang-recovered.tiff",
        )
        .unwrap();
        assert_png_render(&recovered, (1, 1));
        assert_eq!(launcher.launches.load(Ordering::SeqCst), 2);

        client.shutdown();
        assert_all_helpers_killed(&launcher);
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires ImgViewer.CodecFaultHelper.exe staged beside the test binary"]
    fn real_fault_helper_job_oom_crashes_once_then_recovers_lazily() {
        assert_staged_helper(FAULT_HELPER_FILE_NAME);
        let launcher = Arc::new(CountingWindowsLauncher::new(
            windows::WindowsLauncher::for_test(
                FAULT_HELPER_FILE_NAME,
                FAULT_JOB_MEMORY_LIMIT_BYTES,
            ),
        ));
        let client = CodecHelperClient::with_launcher(launcher.clone(), Duration::from_secs(10));
        let directory = tempfile::tempdir().unwrap();

        let error = decode_marker_with_delete_guard(
            &client,
            directory.path(),
            b"IMGVIEWER_FAULT_OOM_V1",
            "oom.tiff",
        )
        .unwrap_err();
        assert_eq!(error.code, "codec_helper_crashed");
        assert_eq!(launcher.launches.load(Ordering::SeqCst), 1);

        let recovered = decode_marker_with_delete_guard(
            &client,
            directory.path(),
            b"IMGVIEWER_FAULT_OK_TIFF_V1",
            "oom-recovered.tiff",
        )
        .unwrap();
        assert_png_render(&recovered, (1, 1));
        assert_eq!(launcher.launches.load(Ordering::SeqCst), 2);

        client.shutdown();
        assert_all_helpers_killed(&launcher);
    }
}
