use std::sync::atomic::{AtomicBool, Ordering};

#[cxx::bridge(namespace = "acadctl")]
mod ffi {
    struct NativeDocumentState {
        token: usize,
        name: String,
        named: bool,
        modified: bool,
        read_only: bool,
    }

    #[derive(Debug)]
    #[repr(u8)]
    enum NativeActionKind {
        None,
        Open,
        Save,
        Close,
    }

    #[derive(Debug)]
    #[repr(u8)]
    enum NativeActionResultKind {
        Success,
        DocumentGone,
        Unnamed,
        ReadOnly,
        Dirty,
        OpenFailed,
        LockFailed,
        SaveFailed,
        CloseFailed,
    }

    struct NativeAction {
        request_id: u64,
        kind: NativeActionKind,
        document_token: usize,
        path: String,
        discard: bool,
    }

    struct NativeActionResult {
        kind: NativeActionResultKind,
        native_status: i32,
        native_detail: String,
    }

    extern "Rust" {
        fn start_rpc_server() -> String;

        fn mark_documents_dirty();

        fn take_documents_dirty() -> bool;

        fn replace_documents(documents: Vec<NativeDocumentState>);

        fn take_native_action() -> NativeAction;

        fn complete_native_action(request_id: u64, result: NativeActionResult);

        fn stop_rpc_server();
    }
}

mod documents;
mod native_actions;
mod rpc_server;

static DOCUMENTS_DIRTY: AtomicBool = AtomicBool::new(false);

fn start_rpc_server() -> String {
    rpc_server::start().err().unwrap_or_default()
}

fn mark_documents_dirty() {
    DOCUMENTS_DIRTY.store(true, Ordering::Relaxed);
}

fn take_documents_dirty() -> bool {
    DOCUMENTS_DIRTY.swap(false, Ordering::Relaxed)
}

fn replace_documents(documents: Vec<ffi::NativeDocumentState>) {
    rpc_server::replace_documents(documents);
}

fn take_native_action() -> ffi::NativeAction {
    native_actions::take()
}

fn complete_native_action(request_id: u64, result: ffi::NativeActionResult) {
    native_actions::complete(request_id, result);
}

fn stop_rpc_server() {
    rpc_server::stop();
}
