use std::collections::HashSet;

use acadctl_rpc::Document;

use crate::ffi::NativeDocumentSnapshot;

const DOCUMENT_ID_LENGTH: usize = 6;
const DOCUMENT_ID_ALPHABET: [char; 31] = [
    '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'j', 'k', 'm',
    'n', 'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z',
];

pub(crate) fn valid_document_id(id: &str) -> bool {
    id.len() == DOCUMENT_ID_LENGTH
        && id
            .chars()
            .all(|character| DOCUMENT_ID_ALPHABET.contains(&character))
}

pub struct DocumentRegistry {
    documents: Vec<TrackedDocument>,
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

    pub fn replace(&mut self, native_documents: Vec<NativeDocumentSnapshot>) {
        let mut reserved_ids = self
            .documents
            .iter()
            .map(|tracked| tracked.document.id.clone())
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

    pub fn find_by_id(&self, id: &str) -> Option<DocumentTarget> {
        self.documents
            .iter()
            .find(|tracked| tracked.document.id == id)
            .map(document_target)
    }

    pub fn find_by_path(&self, path: &str) -> Option<DocumentTarget> {
        self.documents
            .iter()
            .find(|tracked| {
                tracked
                    .document
                    .file_path
                    .as_deref()
                    .is_some_and(|file_path| paths_equal(file_path, path))
            })
            .map(document_target)
    }

    pub fn native_keys(&self) -> impl Iterator<Item = NativeDocumentKey> + '_ {
        self.documents.iter().map(|tracked| tracked.native_key)
    }

    pub(crate) fn resolve_native_key(
        &self,
        document_token: usize,
        database_token: usize,
    ) -> Option<NativeDocumentKey> {
        if document_token == 0 && database_token == 0 {
            return None;
        }

        let mut matches = self.documents.iter().filter(|tracked| {
            (document_token == 0 || tracked.native_key.document_token == document_token)
                && (database_token == 0 || tracked.native_key.database_token == database_token)
        });
        let native_key = matches.next()?.native_key;
        matches.next().is_none().then_some(native_key)
    }
}

fn public_document(id: String, native: NativeDocumentSnapshot) -> Document {
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

#[cfg(windows)]
fn paths_equal(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

#[cfg(not(windows))]
fn paths_equal(left: &str, right: &str) -> bool {
    left == right
}

fn take_document_id(documents: &mut Vec<TrackedDocument>, document_token: usize) -> Option<String> {
    let position = documents
        .iter()
        .position(|document| document.native_key.document_token == document_token)?;
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
    fn validates_public_document_ids_exactly() {
        assert!(valid_document_id("2az789"));
        assert!(!valid_document_id(""));
        assert!(!valid_document_id("2az78"));
        assert!(!valid_document_id("2az7890"));
        assert!(!valid_document_id("2az7i9"));
        assert!(!valid_document_id("2AZ789"));
    }

    #[test]
    fn preserves_identity_while_replacing_native_state() {
        let mut documents = DocumentRegistry::new();
        documents.replace(vec![named_document(1, "/tmp/house.dwg")]);
        let original_id = documents.list()[0].id.clone();

        documents.replace(vec![NativeDocumentSnapshot {
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
                .find_by_id(&original_id)
                .unwrap()
                .native_key
                .database_token,
            99
        );
    }

    #[test]
    fn resolves_only_current_native_document_generations() {
        let mut documents = DocumentRegistry::new();
        documents.replace(vec![
            named_document(1, "/tmp/one.dwg"),
            named_document(2, "/tmp/two.dwg"),
        ]);

        let first = NativeDocumentKey {
            document_token: 1,
            database_token: 101,
        };
        assert_eq!(documents.resolve_native_key(1, 101), Some(first));
        assert_eq!(documents.resolve_native_key(1, 0), Some(first));
        assert_eq!(documents.resolve_native_key(0, 101), Some(first));
        assert_eq!(documents.resolve_native_key(1, 999), None);
        assert_eq!(documents.resolve_native_key(0, 0), None);

        let mut duplicate_database = named_document(3, "/tmp/three.dwg");
        duplicate_database.database_token = 101;
        documents.replace(vec![named_document(1, "/tmp/one.dwg"), duplicate_database]);
        assert_eq!(documents.resolve_native_key(0, 101), None);
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
        assert_eq!(by_id.document.display_name, "house.dwg");
        assert_eq!(by_id.document.file_path.as_deref(), Some("/tmp/house.dwg"));
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
}
