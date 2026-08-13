#[cxx::bridge(namespace = "acadctl")]
mod ffi {
    struct DocumentState {
        id: String,
        path: String,
        modified: bool,
        read_only: bool,
    }

    extern "Rust" {
        fn new_document_id() -> String;

        fn start_rpc_server() -> String;

        fn update_documents(documents: Vec<DocumentState>);

        fn stop_rpc_server();
    }
}

mod rpc_server;

const DOCUMENT_ID_LENGTH: usize = 6;
const DOCUMENT_ID_ALPHABET: [char; 31] = [
    '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'j', 'k', 'm',
    'n', 'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z',
];

fn new_document_id() -> String {
    nanoid::nanoid!(DOCUMENT_ID_LENGTH, &DOCUMENT_ID_ALPHABET)
}

fn start_rpc_server() -> String {
    rpc_server::start().err().unwrap_or_default()
}

fn update_documents(documents: Vec<ffi::DocumentState>) {
    rpc_server::set_documents(documents);
}

fn stop_rpc_server() {
    rpc_server::stop();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_ids_are_fixed_width_and_unambiguous() {
        let id = new_document_id();

        assert_eq!(id.len(), DOCUMENT_ID_LENGTH);
        assert!(
            id.chars()
                .all(|character| DOCUMENT_ID_ALPHABET.contains(&character))
        );
    }
}
