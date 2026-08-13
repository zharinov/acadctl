#[cxx::bridge(namespace = "acadctl")]
mod ffi {
    struct DocumentState {
        path: String,
        modified: bool,
    }

    extern "Rust" {
        fn start_rpc_server() -> String;

        fn update_documents(documents: Vec<DocumentState>);

        fn stop_rpc_server();
    }
}

mod rpc_server;

fn start_rpc_server() -> String {
    rpc_server::start().err().unwrap_or_default()
}

fn update_documents(documents: Vec<ffi::DocumentState>) {
    rpc_server::set_documents(documents);
}

fn stop_rpc_server() {
    rpc_server::stop();
}
