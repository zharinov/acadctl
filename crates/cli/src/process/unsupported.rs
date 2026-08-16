use acadctl_rpc::ProcessId;

pub struct AutoCadProcess;

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

pub(super) fn discover() -> Vec<AutoCadProcess> {
    Vec::new()
}
