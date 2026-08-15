use std::time::Duration;

use acadctl_rpc::{
    Document, DocumentServiceClient, ExecutionServiceClient, ListRequest, ProcessId,
};
use futures_util::stream::{FuturesUnordered, StreamExt};
use tokio::time::timeout;
use tonic::Code;
use tonic::transport::Channel;

const LIST_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Instance {
    pub process_id: ProcessId,
    pub documents: Result<Vec<Document>, QueryError>,
}

pub enum QueryError {
    CannotConnect,
    TimedOut,
    OutdatedPlugin,
    RequestFailed(String),
}

pub async fn list() -> std::io::Result<Vec<Instance>> {
    let mut pending = acadctl_rpc::discover()?
        .into_iter()
        .map(query)
        .collect::<FuturesUnordered<_>>();

    let mut instances = Vec::new();
    while let Some(result) = pending.next().await {
        if let Some(instance) = result {
            instances.push(instance);
        }
    }
    instances.sort_unstable_by_key(|instance| instance.process_id);

    Ok(instances)
}

async fn query(process_id: ProcessId) -> Option<Instance> {
    let documents = match timeout(LIST_TIMEOUT, query_documents(process_id)).await {
        Ok(result) => result,
        Err(_) => Err(QueryError::TimedOut),
    };

    if matches!(documents, Err(QueryError::CannotConnect)) {
        return None;
    }

    Some(Instance {
        process_id,
        documents,
    })
}

async fn query_documents(process_id: ProcessId) -> Result<Vec<Document>, QueryError> {
    let mut client = connect_documents(process_id).await?;
    let listed = client
        .list(ListRequest {})
        .await
        .map_err(|status| {
            if status.code() == Code::Unimplemented {
                QueryError::OutdatedPlugin
            } else {
                QueryError::RequestFailed(status.message().to_owned())
            }
        })?
        .into_inner();
    Ok(listed.documents)
}

pub async fn connect_documents(
    process_id: ProcessId,
) -> Result<DocumentServiceClient<Channel>, QueryError> {
    acadctl_rpc::connect_documents(process_id)
        .await
        .map_err(|_| QueryError::CannotConnect)
}

pub async fn connect_execution(
    process_id: ProcessId,
) -> Result<ExecutionServiceClient<Channel>, QueryError> {
    acadctl_rpc::connect_execution(process_id)
        .await
        .map_err(|_| QueryError::CannotConnect)
}

#[cfg(target_os = "macos")]
pub struct AutoCadProcess {
    process_id: ProcessId,
    application: objc2::rc::Retained<objc2_app_kit::NSRunningApplication>,
}

#[cfg(target_os = "macos")]
impl AutoCadProcess {
    pub fn process_id(&self) -> ProcessId {
        self.process_id
    }

    fn native_process_id(&self) -> libc::pid_t {
        self.process_id
            .get()
            .try_into()
            .expect("macOS process IDs fit pid_t")
    }

    pub fn request_termination(&self, force: bool) -> bool {
        if force {
            let process_id = self.native_process_id();
            let Some(current) =
                objc2_app_kit::NSRunningApplication::runningApplicationWithProcessIdentifier(
                    process_id,
                )
            else {
                return false;
            };
            current == self.application && unsafe { libc::kill(process_id, libc::SIGKILL) == 0 }
        } else {
            self.application.terminate()
        }
    }

    pub fn has_exited(&self) -> bool {
        use objc2_app_kit::NSRunningApplication;

        self.application.isTerminated()
            || NSRunningApplication::runningApplicationWithProcessIdentifier(
                self.native_process_id(),
            )
            .is_none_or(|current| current != self.application)
    }
}

#[cfg(target_os = "macos")]
pub fn autocad_processes() -> Vec<AutoCadProcess> {
    use objc2_app_kit::NSRunningApplication;

    let system = sysinfo::System::new_all();
    let mut processes = Vec::new();
    for process in system.processes().values() {
        let raw_process_id = process.pid().as_u32();
        let Ok(native_process_id) = i32::try_from(raw_process_id) else {
            continue;
        };
        let Some(process_id) = ProcessId::new(raw_process_id) else {
            continue;
        };
        let Some(application) =
            NSRunningApplication::runningApplicationWithProcessIdentifier(native_process_id)
        else {
            continue;
        };
        let Some(bundle_identifier) = application.bundleIdentifier() else {
            continue;
        };
        if !is_autocad_bundle_identifier(&bundle_identifier.to_string()) {
            continue;
        }
        processes.push(AutoCadProcess {
            process_id,
            application,
        });
    }
    processes.sort_unstable_by_key(AutoCadProcess::process_id);
    processes
}

#[cfg(target_os = "macos")]
fn is_autocad_bundle_identifier(identifier: &str) -> bool {
    identifier
        .strip_prefix("com.autodesk.AutoCAD")
        .is_some_and(|version| {
            !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit())
        })
}

#[cfg(windows)]
pub struct AutoCadProcess {
    process_id: ProcessId,
    handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl AutoCadProcess {
    pub fn process_id(&self) -> ProcessId {
        self.process_id
    }

    pub fn request_termination(&self, force: bool) -> bool {
        if force {
            use windows_sys::Win32::System::Threading::{
                OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, TerminateProcess,
            };

            let termination = unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
                    0,
                    self.process_id.get(),
                )
            };
            if termination.is_null() {
                return false;
            }
            let terminated = same_windows_process(self.handle, termination)
                && unsafe { TerminateProcess(termination, 1) != 0 };
            unsafe { windows_sys::Win32::Foundation::CloseHandle(termination) };
            terminated
        } else {
            request_windows_close(self.process_id, self.handle)
        }
    }

    pub fn has_exited(&self) -> bool {
        unsafe {
            windows_sys::Win32::System::Threading::WaitForSingleObject(self.handle, 0)
                == windows_sys::Win32::Foundation::WAIT_OBJECT_0
        }
    }
}

#[cfg(windows)]
fn same_windows_process(
    left: windows_sys::Win32::Foundation::HANDLE,
    right: windows_sys::Win32::Foundation::HANDLE,
) -> bool {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::GetProcessTimes;

    fn creation_time(process: windows_sys::Win32::Foundation::HANDLE) -> Option<u64> {
        let mut created = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut exited = created;
        let mut kernel = created;
        let mut user = created;
        (unsafe {
            GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) != 0
        })
        .then_some((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
    }

    creation_time(left)
        .zip(creation_time(right))
        .is_some_and(|(left_created, right_created)| left_created == right_created)
}

#[cfg(windows)]
impl Drop for AutoCadProcess {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.handle) };
    }
}

#[cfg(windows)]
pub fn autocad_processes() -> Vec<AutoCadProcess> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::path::PathBuf;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        QueryFullProcessImageNameW,
    };

    let system = sysinfo::System::new_all();
    let mut processes = Vec::new();
    for process in system.processes().values() {
        let Some(process_id) = ProcessId::new(process.pid().as_u32()) else {
            continue;
        };
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                process_id.get(),
            )
        };
        if handle.is_null() {
            continue;
        }

        let mut image = vec![0_u16; 32_768];
        let mut length = image.len() as u32;
        let queried =
            unsafe { QueryFullProcessImageNameW(handle, 0, image.as_mut_ptr(), &mut length) != 0 };
        let is_autocad = queried
            && PathBuf::from(OsString::from_wide(&image[..length as usize]))
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("acad.exe"));
        if is_autocad {
            processes.push(AutoCadProcess { process_id, handle });
        } else {
            unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
        }
    }
    processes.sort_unstable_by_key(AutoCadProcess::process_id);
    processes
}

#[cfg(windows)]
fn request_windows_close(
    process_id: ProcessId,
    original: windows_sys::Win32::Foundation::HANDLE,
) -> bool {
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, PostMessageW, WM_CLOSE,
    };

    struct CloseRequest {
        process_id: u32,
        original: windows_sys::Win32::Foundation::HANDLE,
        sent: bool,
    }

    unsafe extern "system" fn close_window(window: HWND, request: LPARAM) -> i32 {
        let request = unsafe { &mut *(request as *mut CloseRequest) };
        let mut process_id = 0;
        unsafe { GetWindowThreadProcessId(window, &mut process_id) };
        if process_id != request.process_id {
            return 1;
        }
        let current = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
        if current.is_null() {
            return 1;
        }
        let same_process = same_windows_process(request.original, current);
        unsafe { windows_sys::Win32::Foundation::CloseHandle(current) };
        if same_process && unsafe { PostMessageW(window, WM_CLOSE, 0, 0) } != 0 {
            request.sent = true;
        }
        1
    }

    let mut request = CloseRequest {
        process_id: process_id.get(),
        original,
        sent: false,
    };
    unsafe {
        EnumWindows(
            Some(close_window),
            (&mut request as *mut CloseRequest) as LPARAM,
        )
    };
    request.sent
}

#[cfg(not(any(target_os = "macos", windows)))]
pub struct AutoCadProcess;

#[cfg(not(any(target_os = "macos", windows)))]
impl AutoCadProcess {
    pub fn process_id(&self) -> ProcessId {
        unreachable!("AutoCAD process discovery is unsupported on this platform")
    }

    pub fn request_termination(&self, _force: bool) -> bool {
        false
    }

    pub fn has_exited(&self) -> bool {
        true
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn autocad_processes() -> Vec<AutoCadProcess> {
    Vec::new()
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn recognizes_only_the_main_macos_autocad_bundle() {
        assert!(is_autocad_bundle_identifier("com.autodesk.AutoCAD2027"));
        assert!(!is_autocad_bundle_identifier("com.autodesk.AutoCAD"));
        assert!(!is_autocad_bundle_identifier(
            "com.autodesk.AutoCAD2027.AcQuickLookPreviewer"
        ));
        assert!(!is_autocad_bundle_identifier("com.autodesk.AutoCADLT2027"));
    }
}
