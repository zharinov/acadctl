use acadctl_rpc::ProcessId;

pub struct AutoCadProcess {
    process_id: ProcessId,
    handle: windows_sys::Win32::Foundation::HANDLE,
}

impl AutoCadProcess {
    pub fn process_id(&self) -> ProcessId {
        self.process_id
    }

    pub fn request_termination(&self, force: bool) -> bool {
        if !force {
            return self.request_windows_close();
        }

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
    }

    pub fn has_exited(&self) -> bool {
        unsafe {
            windows_sys::Win32::System::Threading::WaitForSingleObject(self.handle, 0)
                == windows_sys::Win32::Foundation::WAIT_OBJECT_0
        }
    }

    fn request_windows_close(&self) -> bool {
        use windows_sys::Win32::Foundation::{HWND, LPARAM};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
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

            if !same_process {
                return 1;
            }

            if unsafe { PostMessageW(window, WM_CLOSE, 0, 0) } != 0 {
                request.sent = true;
            }

            1
        }

        let mut request = CloseRequest {
            process_id: self.process_id.get(),
            original: self.handle,
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
}

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

impl Drop for AutoCadProcess {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.handle) };
    }
}

pub(super) fn discover() -> Vec<AutoCadProcess> {
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

        if !is_autocad {
            unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };

            continue;
        }

        processes.push(AutoCadProcess { process_id, handle });
    }

    processes.sort_unstable_by_key(AutoCadProcess::process_id);
    processes
}
