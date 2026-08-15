use std::collections::HashSet;

use acadctl_rpc::{DocumentId, DrawingPath};

use crate::ffi::NativeDocumentSnapshot;

const DOCUMENT_ID_ALPHABET: [char; 16] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'A', 'B', 'C', 'D', 'E', 'F',
];
const DOCUMENT_ID_LENGTH: usize = DocumentId::HEX_WIDTH;

pub struct DocumentRegistry {
    documents: Vec<TrackedDocument>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Document {
    pub id: DocumentId,
    pub display_name: String,
    pub file_path: Option<String>,
    pub modified: bool,
    pub read_only: bool,
}

impl From<Document> for acadctl_rpc::Document {
    fn from(document: Document) -> Self {
        Self {
            id: document.id.to_string(),
            display_name: document.display_name,
            file_path: document.file_path,
            modified: document.modified,
            read_only: document.read_only,
        }
    }
}

struct TrackedDocument {
    native_key: NativeDocumentKey,
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
    pub document: Document,
}

impl DocumentRegistry {
    pub const fn new() -> Self {
        Self {
            documents: Vec::new(),
        }
    }

    pub fn replace_snapshot(&mut self, native_documents: Vec<NativeDocumentSnapshot>) {
        let mut reserved_ids = self
            .documents
            .iter()
            .map(|tracked| tracked.document.id)
            .collect::<HashSet<_>>();
        let mut previous = std::mem::take(&mut self.documents);
        let mut seen_tokens = HashSet::with_capacity(native_documents.len());

        self.documents.reserve(native_documents.len());

        for native in native_documents {
            if !seen_tokens.insert(native.document_token) {
                continue;
            }

            let id = take_document_id(&mut previous, native.document_token)
                .unwrap_or_else(|| new_document_id(&mut reserved_ids));
            self.documents.push(TrackedDocument {
                native_key: NativeDocumentKey {
                    document_token: native.document_token,
                    database_token: native.database_token,
                },
                document: public_document(id, native),
            });
        }
    }

    pub fn list(&self) -> Vec<Document> {
        self.documents
            .iter()
            .map(|tracked| tracked.document.clone())
            .collect()
    }

    pub fn find_by_id(&self, id: DocumentId) -> Option<DocumentTarget> {
        self.documents
            .iter()
            .find(|tracked| tracked.document.id == id)
            .map(document_target)
    }

    pub fn find_by_path(&self, path: &DrawingPath) -> Option<DocumentTarget> {
        self.documents
            .iter()
            .find(|tracked| {
                tracked
                    .document
                    .file_path
                    .as_deref()
                    .is_some_and(|file_path| path.matches(file_path))
            })
            .map(document_target)
    }
}

fn public_document(id: DocumentId, native: NativeDocumentSnapshot) -> Document {
    let name = document_name(native.name, native.named);
    let (display_name, file_path) = if native.named {
        let display_name = std::path::Path::new(&name)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&name)
            .to_owned();
        (display_name, Some(name))
    } else {
        (name, None)
    };

    Document {
        id,
        display_name,
        file_path,
        modified: native.modified,
        read_only: native.read_only,
    }
}

fn document_target(tracked: &TrackedDocument) -> DocumentTarget {
    DocumentTarget {
        native_key: tracked.native_key,
        document: tracked.document.clone(),
    }
}

fn take_document_id(
    documents: &mut Vec<TrackedDocument>,
    document_token: usize,
) -> Option<DocumentId> {
    let position = documents
        .iter()
        .position(|document| document.native_key.document_token == document_token)?;
    Some(documents.swap_remove(position).document.id)
}

fn new_document_id(reserved_ids: &mut HashSet<DocumentId>) -> DocumentId {
    loop {
        let id = nanoid::nanoid!(DOCUMENT_ID_LENGTH, &DOCUMENT_ID_ALPHABET)
            .parse()
            .expect("the document ID alphabet and width are valid");

        if reserved_ids.insert(id) {
            return id;
        }
    }
}

fn document_name(mut name: String, named: bool) -> String {
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
        documents.replace_snapshot(vec![named_document(1, "/tmp/house.dwg")]);
        let original_id = documents.list()[0].id;

        documents.replace_snapshot(vec![NativeDocumentSnapshot {
            database_token: 99,
            modified: true,
            read_only: true,
            ..named_document(1, "/tmp/site.dwg")
        }]);

        let listed = documents.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, original_id);
        assert_eq!(listed[0].display_name, "site.dwg");
        assert_eq!(listed[0].file_path.as_deref(), Some("/tmp/site.dwg"));
        assert!(listed[0].modified);
        assert!(listed[0].read_only);
        assert_eq!(
            documents
                .find_by_id(original_id)
                .unwrap()
                .native_key
                .database_token,
            99
        );
    }

    #[test]
    fn follows_native_order_and_removes_closed_documents() {
        let mut documents = DocumentRegistry::new();
        documents.replace_snapshot(vec![
            named_document(1, "/tmp/house.dwg"),
            named_document(2, "/tmp/site.dwg"),
        ]);
        let original = documents.list();

        documents.replace_snapshot(vec![
            named_document(2, "/tmp/site.dwg"),
            unnamed_document(3, "Drawing1.dwg"),
        ]);

        let listed = documents.list();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, original[1].id);
        assert_eq!(listed[0].display_name, "site.dwg");
        assert_eq!(listed[0].file_path.as_deref(), Some("/tmp/site.dwg"));
        assert_ne!(listed[1].id, original[0].id);
        assert_ne!(listed[1].id, original[1].id);
        assert_eq!(listed[1].display_name, "Drawing1");
        assert_eq!(listed[1].file_path, None);
    }

    #[test]
    fn strips_drawing_suffix_only_from_unnamed_documents() {
        assert_eq!(document_name("Drawing1.DWG".into(), false), "Drawing1");
        assert_eq!(
            document_name("/tmp/house.DWG".into(), true),
            "/tmp/house.DWG"
        );
        assert_eq!(document_name("図面".into(), false), "図面");
    }

    #[test]
    fn finds_documents_by_id_and_named_path() {
        let house = drawing_path("house");
        let other = drawing_path("other");
        let mut documents = DocumentRegistry::new();
        documents.replace_snapshot(vec![
            named_document(1, house.as_str()),
            unnamed_document(2, "Drawing1.dwg"),
        ]);
        let listed = documents.list();

        let by_id = documents.find_by_id(listed[0].id).unwrap();
        assert_eq!(
            by_id.native_key,
            NativeDocumentKey {
                document_token: 1,
                database_token: 101,
            }
        );
        assert_eq!(
            by_id.document.display_name,
            house.as_path().file_name().unwrap()
        );
        assert_eq!(by_id.document.file_path.as_deref(), Some(house.as_str()));
        assert_eq!(
            documents
                .find_by_path(&house)
                .unwrap()
                .native_key
                .document_token,
            1
        );
        assert!(documents.find_by_path(&other).is_none());
    }

    #[test]
    fn document_ids_are_fixed_width_and_unambiguous() {
        let id = new_document_id(&mut HashSet::new());

        assert_eq!(id.to_string().len(), DocumentId::HEX_WIDTH);
    }

    fn named_document(document_token: usize, name: &str) -> NativeDocumentSnapshot {
        NativeDocumentSnapshot {
            document_token,
            database_token: document_token + 100,
            name: name.into(),
            named: true,
            modified: false,
            read_only: false,
        }
    }

    fn unnamed_document(document_token: usize, name: &str) -> NativeDocumentSnapshot {
        NativeDocumentSnapshot {
            named: false,
            ..named_document(document_token, name)
        }
    }

    fn drawing_path(name: &str) -> DrawingPath {
        let path = std::env::temp_dir().join(format!(
            "acadctl-documents-{}-{name}.dwg",
            std::process::id()
        ));
        std::fs::write(&path, []).unwrap();
        let drawing = DrawingPath::canonicalize(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        drawing
    }
}
