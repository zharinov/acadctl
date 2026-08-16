use acadctl_rpc::InstanceId;

pub struct AutoCadInstance;

impl AutoCadInstance {
    pub fn instance_id(&self) -> InstanceId {
        unreachable!("AutoCAD process discovery is unsupported on this platform")
    }

    pub fn request_termination(&self, _force: bool) -> bool {
        false
    }

    pub fn has_exited(&self) -> bool {
        true
    }
}

pub(super) fn discover() -> Vec<AutoCadInstance> {
    Vec::new()
}
