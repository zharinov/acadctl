use std::collections::HashSet;

use acadctl_rpc::{DocId, DrawingPath};
use camino::{Utf8Path, Utf8PathBuf};

use crate::ffi::NativeDocSnapshot;

const DOCUMENT_ID_ALPHABET: [char; 16] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'A', 'B', 'C', 'D', 'E', 'F',
];
const DOCUMENT_ID_LENGTH: usize = DocId::HEX_WIDTH;

pub struct DocRegistry {
    documents: Vec<TrackedDoc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Doc {
    pub id: DocId,
    name: DocName,
    pub modified: bool,
    pub read_only: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum DocName {
    Named(Utf8PathBuf),
    Unnamed(String),
}

struct TrackedDoc {
    native_key: NativeDocKey,
    document: Doc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NativeDocKey {
    pub document_token: usize,
    pub database_token: usize,
}

#[derive(Clone)]
pub struct DocTarget {
    pub native_key: NativeDocKey,
    pub document: Doc,
}

impl DocRegistry {
    pub const fn new() -> Self {
        Self {
            documents: Vec::new(),
        }
    }

    pub fn replace_snapshot(&mut self, native_documents: Vec<NativeDocSnapshot>) {
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
            self.documents.push(TrackedDoc {
                native_key: NativeDocKey {
                    document_token: native.document_token,
                    database_token: native.database_token,
                },
                document: public_document(id, native),
            });
        }
    }

    pub fn list(&self) -> Vec<Doc> {
        self.documents
            .iter()
            .map(|tracked| tracked.document.clone())
            .collect()
    }

    pub fn find_by_id(&self, id: DocId) -> Option<DocTarget> {
        self.documents
            .iter()
            .find(|tracked| tracked.document.id == id)
            .map(document_target)
    }

    pub fn find_by_path(&self, path: &DrawingPath) -> Option<DocTarget> {
        self.documents
            .iter()
            .find(|tracked| {
                tracked
                    .document
                    .file_path()
                    .is_some_and(|file_path| path.matches(file_path.as_str()))
            })
            .map(document_target)
    }
}

impl Doc {
    pub fn display_name(&self) -> &str {
        match &self.name {
            DocName::Named(path) => path.file_name().unwrap_or(path.as_str()),
            DocName::Unnamed(name) => name,
        }
    }

    pub fn file_path(&self) -> Option<&Utf8Path> {
        match &self.name {
            DocName::Named(path) => Some(path),
            DocName::Unnamed(_) => None,
        }
    }
}

fn public_document(id: DocId, native: NativeDocSnapshot) -> Doc {
    Doc {
        id,
        name: document_name(native.name, native.named),
        modified: native.modified,
        read_only: native.read_only,
    }
}

fn document_target(tracked: &TrackedDoc) -> DocTarget {
    DocTarget {
        native_key: tracked.native_key,
        document: tracked.document.clone(),
    }
}

fn take_document_id(documents: &mut Vec<TrackedDoc>, document_token: usize) -> Option<DocId> {
    let position = documents
        .iter()
        .position(|document| document.native_key.document_token == document_token)?;
    Some(documents.swap_remove(position).document.id)
}

fn new_document_id(reserved_ids: &mut HashSet<DocId>) -> DocId {
    loop {
        let Ok(id) = nanoid::nanoid!(DOCUMENT_ID_LENGTH, &DOCUMENT_ID_ALPHABET).parse() else {
            continue;
        };

        if reserved_ids.insert(id) {
            return id;
        }
    }
}

fn document_name(mut name: String, named: bool) -> DocName {
    if named {
        return DocName::Named(name.into());
    }

    if name
        .get(name.len().saturating_sub(4)..)
        .is_some_and(|suffix| suffix.eq_ignore_ascii_case(".dwg"))
    {
        name.truncate(name.len() - 4);
    }

    DocName::Unnamed(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_identity_while_replacing_native_state() {
        let mut documents = DocRegistry::new();
        documents.replace_snapshot(vec![named_document(1, "/tmp/house.dwg")]);
        let original_id = documents.list()[0].id;

        documents.replace_snapshot(vec![NativeDocSnapshot {
            database_token: 99,
            modified: true,
            read_only: true,
            ..named_document(1, "/tmp/site.dwg")
        }]);

        let listed = documents.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, original_id);
        assert_eq!(listed[0].display_name(), "site.dwg");
        assert_eq!(
            listed[0].file_path().map(Utf8Path::as_str),
            Some("/tmp/site.dwg")
        );
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
        let mut documents = DocRegistry::new();
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
        assert_eq!(listed[0].display_name(), "site.dwg");
        assert_eq!(
            listed[0].file_path().map(Utf8Path::as_str),
            Some("/tmp/site.dwg")
        );
        assert_ne!(listed[1].id, original[0].id);
        assert_ne!(listed[1].id, original[1].id);
        assert_eq!(listed[1].display_name(), "Drawing1");
        assert_eq!(listed[1].file_path(), None);
    }

    #[test]
    fn strips_drawing_suffix_only_from_unnamed_documents() {
        assert_eq!(
            document_name("Drawing1.DWG".into(), false),
            DocName::Unnamed("Drawing1".into())
        );
        assert_eq!(
            document_name("/tmp/house.DWG".into(), true),
            DocName::Named("/tmp/house.DWG".into())
        );
        assert_eq!(
            document_name("図面".into(), false),
            DocName::Unnamed("図面".into())
        );
    }

    #[test]
    fn finds_documents_by_id_and_named_path() {
        let house = drawing_path("house");
        let other = drawing_path("other");
        let mut documents = DocRegistry::new();
        documents.replace_snapshot(vec![
            named_document(1, house.as_str()),
            unnamed_document(2, "Drawing1.dwg"),
        ]);
        let listed = documents.list();

        let by_id = documents.find_by_id(listed[0].id).unwrap();
        assert_eq!(
            by_id.native_key,
            NativeDocKey {
                document_token: 1,
                database_token: 101,
            }
        );
        assert_eq!(
            by_id.document.display_name(),
            house.as_path().file_name().unwrap()
        );
        assert_eq!(
            by_id.document.file_path().map(Utf8Path::as_str),
            Some(house.as_str())
        );
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

    fn named_document(document_token: usize, name: &str) -> NativeDocSnapshot {
        NativeDocSnapshot {
            document_token,
            database_token: document_token + 100,
            name: name.into(),
            named: true,
            modified: false,
            read_only: false,
        }
    }

    fn unnamed_document(document_token: usize, name: &str) -> NativeDocSnapshot {
        NativeDocSnapshot {
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
