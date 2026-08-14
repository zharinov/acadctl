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
    native_key: NativeDocumentKey,
    named: bool,
    document: Document,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NativeDocumentKey {
    pub document_token: usize,
    pub database_token: usize,
}

#[derive(Clone)]
pub struct DocumentTarget {
    pub native_key: NativeDocumentKey,
    pub named: bool,
    pub document: Document,
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
                native_key: NativeDocumentKey {
                    document_token: native.token,
                    database_token: native.database_token,
                },
                named: native.named,
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

    pub fn find_by_id(&self, id: &str) -> Option<DocumentTarget> {
        self.documents
            .iter()
            .find(|tracked| tracked.document.id == id)
            .map(document_target)
    }

    pub fn find_by_path(&self, path: &str) -> Option<DocumentTarget> {
        self.documents
            .iter()
            .find(|tracked| tracked.named && paths_equal(&tracked.document.path, path))
            .map(document_target)
    }
}

fn document_target(tracked: &TrackedDocument) -> DocumentTarget {
    DocumentTarget {
        native_key: tracked.native_key,
        named: tracked.named,
        document: tracked.document.clone(),
    }
}

#[cfg(windows)]
fn paths_equal(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

#[cfg(not(windows))]
fn paths_equal(left: &str, right: &str) -> bool {
    left == right
}

fn take_document_id(documents: &mut Vec<TrackedDocument>, native_token: usize) -> Option<String> {
    let position = documents
        .iter()
        .position(|document| document.native_key.document_token == native_token)?;
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
            database_token: 99,
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
        assert_eq!(
            documents
                .find_by_id(&original_id)
                .unwrap()
                .native_key
                .database_token,
            99
        );
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
    fn finds_documents_by_id_and_named_path() {
        let mut documents = DocumentRegistry::new();
        documents.replace(vec![
            named_document(1, "/tmp/house.dwg"),
            unnamed_document(2, "Drawing1.dwg"),
        ]);
        let listed = documents.list();

        let by_id = documents.find_by_id(&listed[0].id).unwrap();
        assert_eq!(
            by_id.native_key,
            NativeDocumentKey {
                document_token: 1,
                database_token: 101,
            }
        );
        assert!(by_id.named);
        assert_eq!(by_id.document.path, "/tmp/house.dwg");
        assert_eq!(
            documents
                .find_by_path("/tmp/house.dwg")
                .unwrap()
                .native_key
                .document_token,
            1
        );
        assert!(documents.find_by_path("Drawing1").is_none());
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
            database_token: token + 100,
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
