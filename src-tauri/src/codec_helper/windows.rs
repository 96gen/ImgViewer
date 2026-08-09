#![allow(unsafe_code)]

use std::ffi::{OsStr, c_void};
use std::fs::File;
use std::io::Write;
use std::mem::{size_of, size_of_val};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use imgviewer_codec_protocol::{
    CODEC_HELPER_MEMORY_LIMIT_BYTES, CodecFormat, DecodeRequest, DecodeResponse, ProtocolError,
    read_decode_response, read_ready, write_decode_request, write_hello,
};
use windows_sys::Win32::Foundation::{
    DUPLICATE_SAME_ACCESS, DuplicateHandle, FALSE, GENERIC_WRITE, HANDLE, HANDLE_FLAG_INHERIT,
    INVALID_HANDLE_VALUE, STILL_ACTIVE, SetHandleInformation, TRUE,
};
#[cfg(test)]
use windows_sys::Win32::Foundation::{GetHandleInformation, WAIT_OBJECT_0};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::CancelSynchronousIo;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
#[cfg(test)]
use windows_sys::Win32::System::Threading::WaitForSingleObject;
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess, GetExitCodeProcess,
    InitializeProcThreadAttributeList, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION,
    ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess,
    UpdateProcThreadAttribute,
};

use super::{HelperSession, SessionKiller, SessionLauncher, TransportError};

const HELPER_FILE_NAME: &str = "ImgViewer.CodecHelper.exe";
#[cfg(test)]
pub(super) const TEST_HELPER_FILE_NAME: &str = HELPER_FILE_NAME;
const HELPER_FAILURE_EXIT_CODE: u32 = 70;
#[cfg(test)]
const PROCESS_EXIT_WAIT_MS: u32 = 5_000;

pub(super) struct WindowsLauncher {
    helper_file_name: &'static str,
    job_memory_limit_bytes: usize,
}

impl WindowsLauncher {
    pub(super) const fn production() -> Self {
        Self {
            helper_file_name: HELPER_FILE_NAME,
            job_memory_limit_bytes: CODEC_HELPER_MEMORY_LIMIT_BYTES,
        }
    }

    #[cfg(test)]
    pub(super) const fn for_test(
        helper_file_name: &'static str,
        job_memory_limit_bytes: usize,
    ) -> Self {
        Self {
            helper_file_name,
            job_memory_limit_bytes,
        }
    }
}

impl SessionLauncher for WindowsLauncher {
    fn launch(&self, timeout: Duration) -> Result<Box<dyn HelperSession>, TransportError> {
        Ok(Box::new(WindowsSession::launch(
            timeout,
            self.helper_file_name,
            self.job_memory_limit_bytes,
        )?))
    }
}

struct WindowsSession {
    stdin: File,
    stdout: Option<File>,
    process: OwnedHandle,
    killer: Arc<JobControl>,
}

impl WindowsSession {
    fn launch(
        timeout: Duration,
        helper_file_name: &str,
        job_memory_limit_bytes: usize,
    ) -> Result<Self, TransportError> {
        let executable = std::env::current_exe().map_err(|_| TransportError::Unavailable)?;
        let helper_path = helper_path_from_executable(&executable, helper_file_name)?;
        let helper_directory = helper_path
            .parent()
            .ok_or(TransportError::Unavailable)?
            .to_path_buf();
        let job = create_job(job_memory_limit_bytes)?;
        let input_pipe = create_pipe()?;
        let output_pipe = create_pipe()?;
        // The explicit process handle list is the primary inheritance
        // boundary. Clearing inheritance on both parent-owned ends is
        // defense-in-depth against a future launch path that omits it.
        clear_inherit(&input_pipe.write)?;
        clear_inherit(&output_pipe.read)?;
        let stderr = open_inheritable_null()?;
        let inherited_handles = [
            raw_handle(&input_pipe.read),
            raw_handle(&output_pipe.write),
            raw_handle(&stderr),
        ];
        let attributes = ProcThreadAttributeList::new(&inherited_handles)?;

        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb =
            u32::try_from(size_of::<STARTUPINFOEXW>()).expect("STARTUPINFOEXW size fits u32");
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = inherited_handles[0];
        startup.StartupInfo.hStdOutput = inherited_handles[1];
        startup.StartupInfo.hStdError = inherited_handles[2];
        startup.lpAttributeList = attributes.as_ptr();

        let application = wide_null(helper_path.as_os_str());
        let mut command_line = quoted_command_line(&helper_path);
        let current_directory = wide_null(helper_directory.as_os_str());
        let mut process_info = PROCESS_INFORMATION::default();
        // SAFETY: all pointers refer to live, correctly sized structures and
        // NUL-terminated UTF-16 buffers for the duration of CreateProcessW.
        let created = unsafe {
            CreateProcessW(
                application.as_ptr(),
                command_line.as_mut_ptr(),
                null(),
                null(),
                TRUE,
                CREATE_SUSPENDED | CREATE_NO_WINDOW | EXTENDED_STARTUPINFO_PRESENT,
                null(),
                current_directory.as_ptr(),
                &startup.StartupInfo,
                &mut process_info,
            )
        };
        if created == FALSE {
            return Err(TransportError::Unavailable);
        }

        // SAFETY: CreateProcessW returned two valid owned handles on success.
        let process = unsafe { OwnedHandle::from_raw_handle(process_info.hProcess) };
        // SAFETY: CreateProcessW returned a valid primary-thread handle.
        let primary_thread = unsafe { OwnedHandle::from_raw_handle(process_info.hThread) };
        // SAFETY: process and job are valid handles owned by this function.
        let assigned = unsafe { AssignProcessToJobObject(raw_handle(&job), raw_handle(&process)) };
        if assigned == FALSE {
            // SAFETY: the suspended process is valid and has not executed any
            // application code; termination prevents an unconstrained helper.
            unsafe {
                TerminateProcess(raw_handle(&process), HELPER_FAILURE_EXIT_CODE);
            }
            return Err(TransportError::Unavailable);
        }
        // SAFETY: the primary thread is still suspended and owned here.
        if unsafe { ResumeThread(raw_handle(&primary_thread)) } == u32::MAX {
            // SAFETY: the process is assigned to this job; terminating the job
            // closes the only possible execution path before returning.
            unsafe {
                TerminateJobObject(raw_handle(&job), HELPER_FAILURE_EXIT_CODE);
            }
            return Err(TransportError::Unavailable);
        }
        drop(primary_thread);
        drop(input_pipe.read);
        drop(output_pipe.write);
        drop(stderr);

        let killer = Arc::new(JobControl {
            job,
            killed: AtomicBool::new(false),
        });
        let mut session = Self {
            stdin: file_from_handle(input_pipe.write),
            stdout: Some(file_from_handle(output_pipe.read)),
            process,
            killer,
        };
        if let Err(error) = write_hello(&mut session.stdin).map_err(map_protocol_error) {
            return Err(session.classify_transport_failure(error));
        }
        if session.stdin.flush().is_err() {
            return Err(session.classify_transport_failure(TransportError::Io));
        }
        session.read_ready_with_timeout(timeout)?;
        Ok(session)
    }

    fn read_ready_with_timeout(&mut self, timeout: Duration) -> Result<(), TransportError> {
        let mut stdout = self
            .stdout
            .take()
            .ok_or_else(|| self.classify_transport_failure(TransportError::Protocol))?;
        let (sender, receiver) = mpsc::sync_channel(1);
        let reader = spawn_reader("imgviewer-helper-handshake", move || {
            let result = read_ready(&mut stdout);
            let _ = sender.send((stdout, result));
        })?;
        match receiver.recv_timeout(timeout) {
            Ok((stdout, result)) => {
                if join_reader(reader).is_err() {
                    return Err(self.classify_transport_failure(TransportError::Protocol));
                }
                self.stdout = Some(stdout);
                result
                    .map_err(map_protocol_error)
                    .map_err(|error| self.classify_transport_failure(error))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.killer.kill();
                cancel_and_join_reader(reader)?;
                Err(TransportError::Timeout)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let error = self.classify_transport_failure(TransportError::Protocol);
                self.killer.kill();
                let _ = join_reader(reader);
                Err(error)
            }
        }
    }

    fn duplicate_file_into_child(&self, file: &File) -> Result<u64, TransportError> {
        let mut remote_handle: HANDLE = null_mut();
        // SAFETY: the source file and both process handles are valid. The new
        // child handle is non-inheritable and retains the source's read-only
        // access because DUPLICATE_SAME_ACCESS is used.
        let duplicated = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                file.as_raw_handle(),
                raw_handle(&self.process),
                &mut remote_handle,
                0,
                FALSE,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if duplicated == FALSE || remote_handle.is_null() {
            return Err(TransportError::Io);
        }
        Ok(remote_handle as usize as u64)
    }

    fn classify_transport_failure(&self, error: TransportError) -> TransportError {
        if !matches!(
            error,
            TransportError::Io | TransportError::Protocol | TransportError::Disconnected
        ) {
            return error;
        }

        let mut exit_code = 0_u32;
        // SAFETY: process is a live owned process handle and exit_code is a
        // valid output pointer. Query failure leaves the original transport
        // classification intact.
        let queried = unsafe { GetExitCodeProcess(raw_handle(&self.process), &mut exit_code) };
        classify_transport_failure_with_exit_code(error, (queried != FALSE).then_some(exit_code))
    }
}

fn classify_transport_failure_with_exit_code(
    error: TransportError,
    exit_code: Option<u32>,
) -> TransportError {
    if exit_code.is_some_and(|exit_code| exit_code != STILL_ACTIVE as u32) {
        TransportError::Disconnected
    } else {
        error
    }
}

impl HelperSession for WindowsSession {
    fn killer(&self) -> Arc<dyn SessionKiller> {
        self.killer.clone()
    }

    fn transact(
        &mut self,
        format: CodecFormat,
        file: File,
        request_id: u64,
        expected_length: u64,
        timeout: Duration,
    ) -> Result<DecodeResponse, TransportError> {
        if self.killer.is_killed() {
            return Err(TransportError::Cancelled);
        }
        let duplicated_handle = self
            .duplicate_file_into_child(&file)
            .map_err(|error| self.classify_transport_failure(error))?;
        if let Err(error) = write_decode_request(
            &mut self.stdin,
            DecodeRequest {
                request_id,
                duplicated_handle,
                expected_length,
                format,
            },
        )
        .map_err(map_protocol_error)
        {
            return Err(self.classify_transport_failure(error));
        }
        if self.stdin.flush().is_err() {
            return Err(self.classify_transport_failure(TransportError::Io));
        }
        drop(file);

        let mut stdout = self
            .stdout
            .take()
            .ok_or_else(|| self.classify_transport_failure(TransportError::Protocol))?;
        let (sender, receiver) = mpsc::sync_channel(1);
        let reader = spawn_reader("imgviewer-helper-response", move || {
            let result = read_decode_response(&mut stdout, request_id);
            let _ = sender.send((stdout, result));
        })?;
        match receiver.recv_timeout(timeout) {
            Ok((stdout, result)) => {
                if join_reader(reader).is_err() {
                    return Err(self.classify_transport_failure(TransportError::Protocol));
                }
                self.stdout = Some(stdout);
                result
                    .map_err(map_protocol_error)
                    .map_err(|error| self.classify_transport_failure(error))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.killer.kill();
                cancel_and_join_reader(reader)?;
                Err(TransportError::Timeout)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let error = self.classify_transport_failure(TransportError::Protocol);
                self.killer.kill();
                let _ = join_reader(reader);
                Err(error)
            }
        }
    }

    #[cfg(test)]
    fn terminate_process_for_test(&self) -> Result<(), TransportError> {
        // SAFETY: process is a live owned process handle. This deliberately
        // bypasses JobControl's killed flag so the next transaction observes
        // an unexpected child death through the real broker transport.
        if unsafe { TerminateProcess(raw_handle(&self.process), HELPER_FAILURE_EXIT_CODE) } == FALSE
        {
            return Err(TransportError::Io);
        }
        // SAFETY: waiting on the owned process handle is read-only and bounded.
        if unsafe { WaitForSingleObject(raw_handle(&self.process), PROCESS_EXIT_WAIT_MS) }
            != WAIT_OBJECT_0
        {
            return Err(TransportError::Timeout);
        }
        Ok(())
    }
}

impl Drop for WindowsSession {
    fn drop(&mut self) {
        self.killer.kill();
    }
}

struct JobControl {
    job: OwnedHandle,
    killed: AtomicBool,
}

impl SessionKiller for JobControl {
    fn kill(&self) {
        if !self.killed.swap(true, Ordering::SeqCst) {
            // SAFETY: job remains owned by self for the duration of this call.
            unsafe {
                TerminateJobObject(raw_handle(&self.job), HELPER_FAILURE_EXIT_CODE);
            }
        }
    }

    fn is_killed(&self) -> bool {
        self.killed.load(Ordering::SeqCst)
    }
}

impl Drop for JobControl {
    fn drop(&mut self) {
        self.kill();
    }
}

struct PipePair {
    read: OwnedHandle,
    write: OwnedHandle,
}

fn create_pipe() -> Result<PipePair, TransportError> {
    let security = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>())
            .expect("SECURITY_ATTRIBUTES size fits u32"),
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: TRUE,
    };
    let mut read: HANDLE = null_mut();
    let mut write: HANDLE = null_mut();
    // SAFETY: output pointers and SECURITY_ATTRIBUTES are valid and writable.
    if unsafe { CreatePipe(&mut read, &mut write, &security, 0) } == FALSE {
        return Err(TransportError::Io);
    }
    Ok(PipePair {
        read: owned_handle(read)?,
        write: owned_handle(write)?,
    })
}

fn clear_inherit(handle: &OwnedHandle) -> Result<(), TransportError> {
    // SAFETY: handle is a valid owned pipe handle. The mask changes only its
    // HANDLE_FLAG_INHERIT bit and does not affect access rights.
    if unsafe { SetHandleInformation(raw_handle(handle), HANDLE_FLAG_INHERIT, 0) } == FALSE {
        return Err(TransportError::Io);
    }
    Ok(())
}

fn open_inheritable_null() -> Result<OwnedHandle, TransportError> {
    let security = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>())
            .expect("SECURITY_ATTRIBUTES size fits u32"),
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: TRUE,
    };
    let nul = wide_null(OsStr::new("NUL"));
    // SAFETY: the path is NUL-terminated and SECURITY_ATTRIBUTES is valid.
    let handle = unsafe {
        CreateFileW(
            nul.as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &security,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(TransportError::Io);
    }
    owned_handle(handle)
}

fn create_job(memory_limit_bytes: usize) -> Result<OwnedHandle, TransportError> {
    // SAFETY: null security/name requests an unnamed job with defaults.
    let raw_job = unsafe { CreateJobObjectW(null(), null()) };
    let job = owned_handle(raw_job)?;
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_PROCESS_MEMORY
        | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
        | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    limits.BasicLimitInformation.ActiveProcessLimit = 1;
    limits.ProcessMemoryLimit = memory_limit_bytes;
    // SAFETY: limits points to a correctly sized structure for the requested
    // JobObjectExtendedLimitInformation information class.
    let configured = unsafe {
        SetInformationJobObject(
            raw_handle(&job),
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            u32::try_from(size_of_val(&limits)).expect("job limit structure size fits u32"),
        )
    };
    if configured == FALSE {
        return Err(TransportError::Unavailable);
    }
    Ok(job)
}

struct ProcThreadAttributeList {
    storage: Vec<usize>,
    pointer: windows_sys::Win32::System::Threading::LPPROC_THREAD_ATTRIBUTE_LIST,
}

impl ProcThreadAttributeList {
    fn new(handles: &[HANDLE]) -> Result<Self, TransportError> {
        let mut bytes = 0_usize;
        // SAFETY: the documented sizing call uses a null list and writes only
        // the required allocation size.
        unsafe {
            InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut bytes);
        }
        if bytes == 0 {
            return Err(TransportError::Unavailable);
        }
        let words = bytes.div_ceil(size_of::<usize>());
        let mut storage = vec![0_usize; words];
        let pointer = storage.as_mut_ptr().cast();
        // SAFETY: storage is pointer-aligned, live, and at least the byte size
        // returned by the sizing call.
        if unsafe { InitializeProcThreadAttributeList(pointer, 1, 0, &mut bytes) } == FALSE {
            return Err(TransportError::Unavailable);
        }
        // SAFETY: pointer is initialized and handles points to inheritable
        // handles that remain live through CreateProcessW.
        let updated = unsafe {
            UpdateProcThreadAttribute(
                pointer,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_ptr().cast::<c_void>(),
                size_of_val(handles),
                null_mut(),
                null(),
            )
        };
        if updated == FALSE {
            // SAFETY: pointer was successfully initialized above.
            unsafe {
                DeleteProcThreadAttributeList(pointer);
            }
            return Err(TransportError::Unavailable);
        }
        Ok(Self { storage, pointer })
    }

    fn as_ptr(&self) -> windows_sys::Win32::System::Threading::LPPROC_THREAD_ATTRIBUTE_LIST {
        let _keep_alive = &self.storage;
        self.pointer
    }
}

impl Drop for ProcThreadAttributeList {
    fn drop(&mut self) {
        // SAFETY: pointer was initialized exactly once and is deleted once.
        unsafe {
            DeleteProcThreadAttributeList(self.pointer);
        }
    }
}

fn owned_handle(handle: HANDLE) -> Result<OwnedHandle, TransportError> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(TransportError::Io);
    }
    // SAFETY: caller transfers ownership of a newly created Win32 handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

fn file_from_handle(handle: OwnedHandle) -> File {
    let raw = handle.as_raw_handle();
    std::mem::forget(handle);
    // SAFETY: ownership was removed from OwnedHandle and transferred exactly
    // once into File.
    unsafe { File::from_raw_handle(raw) }
}

fn raw_handle(handle: &OwnedHandle) -> HANDLE {
    handle.as_raw_handle()
}

fn helper_path_from_executable(
    executable: &Path,
    helper_file_name: &str,
) -> Result<PathBuf, TransportError> {
    executable
        .parent()
        .map(|directory| directory.join(helper_file_name))
        .ok_or(TransportError::Unavailable)
}

fn wide_null(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

fn quoted_command_line(path: &Path) -> Vec<u16> {
    let mut command = Vec::with_capacity(path.as_os_str().len() + 3);
    command.push(u16::from(b'"'));
    command.extend(path.as_os_str().encode_wide());
    command.push(u16::from(b'"'));
    command.push(0);
    command
}

fn spawn_reader(
    name: &str,
    operation: impl FnOnce() + Send + 'static,
) -> Result<JoinHandle<()>, TransportError> {
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(operation)
        .map_err(|_| TransportError::Unavailable)
}

fn join_reader(reader: JoinHandle<()>) -> Result<(), TransportError> {
    reader.join().map_err(|_| TransportError::Disconnected)
}

fn cancel_and_join_reader(reader: JoinHandle<()>) -> Result<(), TransportError> {
    // SAFETY: JoinHandleExt exposes a borrowed live thread handle. Cancelling
    // synchronous pipe I/O makes the subsequent join bounded even if process
    // termination did not close the pipe promptly.
    let cancelled = unsafe { CancelSynchronousIo(reader.as_raw_handle()) };
    if cancelled == FALSE {
        // The read may already have completed. Avoid an unbounded join in the
        // exceptional case where neither job termination nor cancellation was
        // observable; dropping JoinHandle detaches rather than blocking.
        drop(reader);
        return Ok(());
    }
    join_reader(reader)
}

fn map_protocol_error(error: ProtocolError) -> TransportError {
    match error {
        ProtocolError::Io(_) => TransportError::Io,
        _ => TransportError::Protocol,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_path_is_fixed_next_to_the_main_executable() {
        let executable = Path::new(r"C:\Portable\ImgViewer.exe");
        assert_eq!(
            helper_path_from_executable(executable, HELPER_FILE_NAME).unwrap(),
            PathBuf::from(r"C:\Portable\ImgViewer.CodecHelper.exe")
        );
    }

    #[test]
    fn job_contract_uses_exact_memory_and_process_limits() {
        let launcher = WindowsLauncher::production();
        assert_eq!(
            launcher.job_memory_limit_bytes,
            CODEC_HELPER_MEMORY_LIMIT_BYTES
        );
        assert_eq!(launcher.helper_file_name, HELPER_FILE_NAME);
        let flags = JOB_OBJECT_LIMIT_PROCESS_MEMORY
            | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
            | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        assert_ne!(flags & JOB_OBJECT_LIMIT_PROCESS_MEMORY, 0);
        assert_ne!(flags & JOB_OBJECT_LIMIT_ACTIVE_PROCESS, 0);
        assert_ne!(flags & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, 0);
    }

    #[test]
    fn process_exit_classification_preserves_live_failures_and_maps_dead_children() {
        for error in [TransportError::Io, TransportError::Protocol] {
            assert_eq!(
                classify_transport_failure_with_exit_code(error, Some(STILL_ACTIVE as u32)),
                error
            );
            assert_eq!(
                classify_transport_failure_with_exit_code(error, None),
                error
            );
            assert_eq!(
                classify_transport_failure_with_exit_code(error, Some(HELPER_FAILURE_EXIT_CODE)),
                TransportError::Disconnected
            );
        }
    }

    #[test]
    fn parent_pipe_ends_have_inheritance_cleared() {
        let pipe = create_pipe().unwrap();
        clear_inherit(&pipe.write).unwrap();
        let mut flags = u32::MAX;
        // SAFETY: pipe.write is live and flags is a valid output pointer.
        assert_ne!(
            unsafe { GetHandleInformation(raw_handle(&pipe.write), &mut flags) },
            FALSE
        );
        assert_eq!(flags & HANDLE_FLAG_INHERIT, 0);
    }

    #[test]
    fn command_line_contains_only_the_fixed_quoted_executable() {
        let path = Path::new(r"C:\Portable Folder\ImgViewer.CodecHelper.exe");
        let command = String::from_utf16(
            &quoted_command_line(path)
                .into_iter()
                .take_while(|value| *value != 0)
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(command, r#""C:\Portable Folder\ImgViewer.CodecHelper.exe""#);
    }
}
