use std::process::ExitCode;
use std::time::Duration;

use sysinfo::{Pid, ProcessesToUpdate, System};
use tokio::time::{Instant, sleep};

use super::fail;
use crate::instances::autocad_process_ids;

const EXIT_TIMEOUT: Duration = Duration::from_secs(5);
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(50);

pub async fn run(requested_process_id: Option<u32>, force: bool) -> ExitCode {
    let process_ids = autocad_process_ids();
    let process_id = match select_process_id(&process_ids, requested_process_id) {
        Ok(process_id) => process_id,
        Err(error) => return fail(error),
    };

    if !request_termination(process_id, force) {
        let action = if force { "force" } else { "ask" };
        return fail(format!(
            "Could not {action} AutoCAD process {process_id} to quit."
        ));
    }

    if wait_until_stopped(process_id).await {
        ExitCode::SUCCESS
    } else if force {
        fail(format!(
            "AutoCAD process {process_id} did not terminate within 5 seconds."
        ))
    } else {
        fail(format!(
            "AutoCAD process {process_id} did not exit within 5 seconds. Run `acadctl kill {process_id} --force` to terminate it immediately."
        ))
    }
}

fn select_process_id(
    process_ids: &[u32],
    requested_process_id: Option<u32>,
) -> Result<u32, String> {
    match requested_process_id {
        Some(process_id) if process_ids.contains(&process_id) => Ok(process_id),
        Some(process_id) => Err(format!("AutoCAD process {process_id} is not running.")),
        None => match process_ids {
            [process_id] => Ok(*process_id),
            [] => Err("AutoCAD is not running.".into()),
            process_ids => Err(format!(
                "More than one AutoCAD instance is running ({}). Use `acadctl kill <pid>`.",
                process_ids
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        },
    }
}

async fn wait_until_stopped(process_id: u32) -> bool {
    let deadline = Instant::now() + EXIT_TIMEOUT;
    let process_id = Pid::from_u32(process_id);
    let mut system = System::new();
    loop {
        system.refresh_processes(ProcessesToUpdate::Some(&[process_id]), true);
        if system.process(process_id).is_none() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        sleep(EXIT_POLL_INTERVAL).await;
    }
}

#[cfg(target_os = "macos")]
fn request_termination(process_id: u32, force: bool) -> bool {
    use objc2_app_kit::NSRunningApplication;

    let Ok(process_id) = i32::try_from(process_id) else {
        return false;
    };
    if force {
        return force_terminate_process(process_id as u32);
    }
    if NSRunningApplication::runningApplicationWithProcessIdentifier(process_id)
        .is_some_and(|application| application.terminate())
    {
        return true;
    }
    send_quit_apple_event(process_id)
}

#[cfg(target_os = "macos")]
fn send_quit_apple_event(process_id: i32) -> bool {
    use objc2_foundation::{NSAppleEventDescriptor, NSAppleEventSendOptions};

    const CORE_EVENT_CLASS: u32 = u32::from_be_bytes(*b"aevt");
    const QUIT_APPLICATION: u32 = u32::from_be_bytes(*b"quit");

    let target = NSAppleEventDescriptor::descriptorWithProcessIdentifier(process_id);
    let event = NSAppleEventDescriptor::appleEventWithEventClass_eventID_targetDescriptor_returnID_transactionID(
        CORE_EVENT_CLASS,
        QUIT_APPLICATION,
        Some(&target),
        -1,
        0,
    );
    event
        .sendEventWithOptions_timeout_error(NSAppleEventSendOptions::NoReply, 1.0)
        .is_ok()
}

#[cfg(target_os = "macos")]
fn force_terminate_process(process_id: u32) -> bool {
    use sysinfo::Signal;

    System::new_all()
        .process(Pid::from_u32(process_id))
        .and_then(|process| process.kill_with(Signal::Kill))
        .unwrap_or(false)
}

#[cfg(windows)]
fn request_termination(process_id: u32, force: bool) -> bool {
    if force {
        force_terminate_windows_process(process_id)
    } else {
        request_windows_close(process_id)
    }
}

#[cfg(windows)]
fn request_windows_close(process_id: u32) -> bool {
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, PostMessageW, WM_CLOSE,
    };

    struct CloseRequest {
        process_id: u32,
        sent: bool,
    }

    unsafe extern "system" fn close_window(window: HWND, request: LPARAM) -> i32 {
        let request = unsafe { &mut *(request as *mut CloseRequest) };
        let mut process_id = 0;
        unsafe { GetWindowThreadProcessId(window, &mut process_id) };
        if process_id == request.process_id && unsafe { PostMessageW(window, WM_CLOSE, 0, 0) } != 0
        {
            request.sent = true;
        }
        1
    }

    let mut request = CloseRequest {
        process_id,
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

#[cfg(windows)]
fn force_terminate_windows_process(process_id: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};

    let process = unsafe { OpenProcess(PROCESS_TERMINATE, 0, process_id) };
    if process.is_null() {
        return false;
    }
    let terminated = unsafe { TerminateProcess(process, 1) } != 0;
    unsafe { CloseHandle(process) };
    terminated
}

#[cfg(not(any(target_os = "macos", windows)))]
fn request_termination(process_id: u32, force: bool) -> bool {
    use sysinfo::Signal;

    let system = System::new_all();
    let Some(process) = system.process(Pid::from_u32(process_id)) else {
        return false;
    };
    process
        .kill_with(if force { Signal::Kill } else { Signal::Term })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_the_only_autocad_process() {
        assert_eq!(select_process_id(&[123], None).unwrap(), 123);
        assert_eq!(select_process_id(&[123, 456], Some(456)).unwrap(), 456);
    }

    #[test]
    fn requires_an_exact_process_when_selection_is_ambiguous() {
        assert_eq!(
            select_process_id(&[], None).unwrap_err(),
            "AutoCAD is not running."
        );
        assert_eq!(
            select_process_id(&[123, 456], None).unwrap_err(),
            "More than one AutoCAD instance is running (123, 456). Use `acadctl kill <pid>`."
        );
        assert_eq!(
            select_process_id(&[123], Some(456)).unwrap_err(),
            "AutoCAD process 456 is not running."
        );
    }
}
