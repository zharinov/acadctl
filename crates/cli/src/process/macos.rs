use acadctl_rpc::InstanceId;
use std::path::PathBuf;

const AUTODESK_APPLICATIONS: &str = "/Applications/Autodesk";

pub struct AutoCadInstance {
    instance_id: InstanceId,
    application: objc2::rc::Retained<objc2_app_kit::NSRunningApplication>,
}

impl AutoCadInstance {
    pub fn instance_id(&self) -> InstanceId {
        self.instance_id
    }

    fn native_process_id(&self) -> libc::pid_t {
        self.instance_id
            .get()
            .try_into()
            .expect("macOS process IDs fit pid_t")
    }

    pub fn request_termination(&self, force: bool) -> bool {
        if !force {
            return self.application.terminate();
        }

        let native_process_id = self.native_process_id();
        let Some(current) =
            objc2_app_kit::NSRunningApplication::runningApplicationWithProcessIdentifier(
                native_process_id,
            )
        else {
            return false;
        };

        // SAFETY: `kill` receives only an initialized process ID and a valid signal number.
        current == self.application && unsafe { libc::kill(native_process_id, libc::SIGKILL) == 0 }
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

pub(super) fn discover() -> Vec<AutoCadInstance> {
    use objc2_app_kit::NSRunningApplication;

    let system = sysinfo::System::new_all();
    let mut processes = Vec::new();

    for process in system.processes().values() {
        let native_process_id = process.pid().as_u32();
        let Ok(native_process_identifier) = i32::try_from(native_process_id) else {
            continue;
        };

        let Some(instance_id) = InstanceId::new(native_process_id) else {
            continue;
        };

        let Some(application) = NSRunningApplication::runningApplicationWithProcessIdentifier(
            native_process_identifier,
        ) else {
            continue;
        };

        let Some(bundle_identifier) = application.bundleIdentifier() else {
            continue;
        };

        if !is_autocad_bundle_identifier(&bundle_identifier.to_string()) {
            continue;
        }

        processes.push(AutoCadInstance {
            instance_id,
            application,
        });
    }

    processes.sort_unstable_by_key(AutoCadInstance::instance_id);
    processes
}

pub(super) fn launch() -> Result<Option<InstanceId>, String> {
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    use block2::RcBlock;
    use objc2_app_kit::{NSRunningApplication, NSWorkspace, NSWorkspaceOpenConfiguration};
    use objc2_foundation::{NSDate, NSError, NSRunLoop, NSString, NSURL};

    const LAUNCH_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
    const RUN_LOOP_SLICE_SECONDS: f64 = 0.05;

    #[derive(Clone, Copy)]
    enum LaunchCompletion {
        Pending,
        Finished(Option<InstanceId>),
    }

    let application = installed_application()
        .ok_or_else(|| "Could not find an installed AutoCAD application".to_owned())?;
    let application = application
        .to_str()
        .ok_or_else(|| "AutoCAD failed to start.".to_owned())?;
    let application = NSString::from_str(application);
    let application = NSURL::fileURLWithPath_isDirectory(&application, true);
    let configuration = NSWorkspaceOpenConfiguration::configuration();
    let completion = Rc::new(Cell::new(LaunchCompletion::Pending));
    let callback_completion = Rc::clone(&completion);
    let callback = RcBlock::new(
        move |application: *mut NSRunningApplication, _error: *mut NSError| {
            // SAFETY: AppKit owns the callback argument for the duration of this invocation.
            let instance_id = unsafe { application.as_ref() }
                .and_then(|application| u32::try_from(application.processIdentifier()).ok())
                .and_then(InstanceId::new);
            callback_completion.set(LaunchCompletion::Finished(instance_id));
        },
    );

    NSWorkspace::sharedWorkspace().openApplicationAtURL_configuration_completionHandler(
        &application,
        &configuration,
        Some(&callback),
    );

    let deadline = Instant::now() + LAUNCH_REQUEST_TIMEOUT;
    let run_loop = NSRunLoop::currentRunLoop();

    while matches!(completion.get(), LaunchCompletion::Pending) && Instant::now() < deadline {
        run_loop.runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(
            RUN_LOOP_SLICE_SECONDS,
        ));
    }

    match completion.get() {
        LaunchCompletion::Finished(Some(instance_id)) => Ok(Some(instance_id)),
        LaunchCompletion::Pending | LaunchCompletion::Finished(None) => {
            Err("AutoCAD failed to start.".to_owned())
        }
    }
}

fn installed_application() -> Option<PathBuf> {
    std::fs::read_dir(AUTODESK_APPLICATIONS)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let release = entry.file_name().into_string().ok()?;
            let version = release_version(&release)?;
            let application = entry.path().join(format!("{release}.app"));
            let executable = application.join("Contents/MacOS/AutoCAD");

            (application.is_dir() && executable.is_file()).then_some((version, application))
        })
        .max_by_key(|(version, _)| *version)
        .map(|(_, application)| application)
}

fn release_version(release: &str) -> Option<u32> {
    release.strip_prefix("AutoCAD ")?.parse().ok()
}

fn is_autocad_bundle_identifier(identifier: &str) -> bool {
    let Some(version) = identifier.strip_prefix("com.autodesk.AutoCAD") else {
        return false;
    };

    !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
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

    #[test]
    fn recognizes_autocad_release_directories() {
        assert_eq!(release_version("AutoCAD 2027"), Some(2027));
        assert_eq!(release_version("AutoCAD 2027.app"), None);
        assert_eq!(release_version("AutoCAD LT 2027"), None);
    }
}
