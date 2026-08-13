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

    extern "Rust" {
        fn start_rpc_server() -> String;

        fn mark_documents_dirty();

        fn take_documents_dirty() -> bool;

        fn replace_documents(documents: Vec<NativeDocumentState>);

        fn stop_rpc_server();
    }
}

mod documents;
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

fn stop_rpc_server() {
    rpc_server::stop();
}
