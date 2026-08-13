use std::collections::HashSet;

use acadctl_rpc::Document;

use crate::ffi::NativeDocumentState;

const DOCUMENT_ID_LENGTH: usize = 6;
const DOCUMENT_ID_ALPHABET: [char; 31] = [
    '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'j', 'k', 'm',
    'n', 'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z',
];

pub struct DocumentRegistry {
    documents: Vec<TrackedDocument>,
}

struct TrackedDocument {
    native_token: usize,
    document: Document,
}

impl DocumentRegistry {
    pub const fn new() -> Self {
        Self {
            documents: Vec::new(),
        }
    }

    pub fn replace(&mut self, native_documents: Vec<NativeDocumentState>) {
        let mut reserved_ids = self
            .documents
            .iter()
            .map(|tracked| tracked.document.id.clone())
            .collect::<HashSet<_>>();
        let mut previous = std::mem::take(&mut self.documents);
        let mut seen_tokens = HashSet::with_capacity(native_documents.len());

        self.documents.reserve(native_documents.len());
        for native in native_documents {
            if !seen_tokens.insert(native.token) {
                continue;
            }
            let id = take_document_id(&mut previous, native.token)
                .unwrap_or_else(|| new_document_id(&mut reserved_ids));
            self.documents.push(TrackedDocument {
                native_token: native.token,
                document: Document {
                    id,
                    path: document_path(native.name, native.named),
                    modified: native.modified,
                    read_only: native.read_only,
                },
            });
        }
    }

    pub fn list(&self) -> Vec<Document> {
        self.documents
            .iter()
            .map(|tracked| tracked.document.clone())
            .collect()
    }
}

fn take_document_id(documents: &mut Vec<TrackedDocument>, native_token: usize) -> Option<String> {
    let position = documents
        .iter()
        .position(|document| document.native_token == native_token)?;
    Some(documents.swap_remove(position).document.id)
}

fn new_document_id(reserved_ids: &mut HashSet<String>) -> String {
    loop {
        let id = nanoid::nanoid!(DOCUMENT_ID_LENGTH, &DOCUMENT_ID_ALPHABET);
        if reserved_ids.insert(id.clone()) {
            return id;
        }
    }
}

fn document_path(mut name: String, named: bool) -> String {
    if !named
        && name
            .get(name.len().saturating_sub(4)..)
            .is_some_and(|suffix| suffix.eq_ignore_ascii_case(".dwg"))
    {
        name.truncate(name.len() - 4);
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_identity_while_replacing_native_state() {
        let mut documents = DocumentRegistry::new();
        documents.replace(vec![named_document(1, "/tmp/house.dwg")]);
        let original_id = documents.list()[0].id.clone();

        documents.replace(vec![NativeDocumentState {
            modified: true,
            read_only: true,
            ..named_document(1, "/tmp/site.dwg")
        }]);

        let listed = documents.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, original_id);
        assert_eq!(listed[0].path, "/tmp/site.dwg");
        assert!(listed[0].modified);
        assert!(listed[0].read_only);
    }

    #[test]
    fn follows_native_order_and_removes_closed_documents() {
        let mut documents = DocumentRegistry::new();
        documents.replace(vec![
            named_document(1, "/tmp/house.dwg"),
            named_document(2, "/tmp/site.dwg"),
        ]);
        let original = documents.list();

        documents.replace(vec![
            named_document(2, "/tmp/site.dwg"),
            unnamed_document(3, "Drawing1.dwg"),
        ]);

        let listed = documents.list();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, original[1].id);
        assert_eq!(listed[0].path, "/tmp/site.dwg");
        assert_ne!(listed[1].id, original[0].id);
        assert_ne!(listed[1].id, original[1].id);
        assert_eq!(listed[1].path, "Drawing1");
    }

    #[test]
    fn strips_drawing_suffix_only_from_unnamed_documents() {
        assert_eq!(document_path("Drawing1.DWG".into(), false), "Drawing1");
        assert_eq!(
            document_path("/tmp/house.DWG".into(), true),
            "/tmp/house.DWG"
        );
        assert_eq!(document_path("図面".into(), false), "図面");
    }

    #[test]
    fn document_ids_are_fixed_width_and_unambiguous() {
        let id = new_document_id(&mut HashSet::new());

        assert_eq!(id.len(), DOCUMENT_ID_LENGTH);
        assert!(
            id.chars()
                .all(|character| DOCUMENT_ID_ALPHABET.contains(&character))
        );
    }

    fn named_document(token: usize, name: &str) -> NativeDocumentState {
        NativeDocumentState {
            token,
            name: name.into(),
            named: true,
            modified: false,
            read_only: false,
        }
    }

    fn unnamed_document(token: usize, name: &str) -> NativeDocumentState {
        NativeDocumentState {
            named: false,
            ..named_document(token, name)
        }
    }
}
