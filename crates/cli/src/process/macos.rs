use acadctl_rpc::InstanceId;

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
}
