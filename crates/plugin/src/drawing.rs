use std::collections::HashSet;

use acadctl_rpc::{DrawingId, DrawingPath};
use camino::{Utf8Path, Utf8PathBuf};

use crate::ffi::NativeDocumentSnapshot;

const DRAWING_ID_ALPHABET: [char; 16] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'A', 'B', 'C', 'D', 'E', 'F',
];
const DRAWING_ID_LENGTH: usize = DrawingId::HEX_WIDTH;

pub struct DrawingRegistry {
    drawings: Vec<TrackedDrawing>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drawing {
    pub id: DrawingId,
    name: DrawingName,
    pub modified: bool,
    pub read_only: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum DrawingName {
    Named(Utf8PathBuf),
    Unnamed(String),
}

struct TrackedDrawing {
    native_key: NativeDocumentKey,
    drawing: Drawing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NativeDocumentKey {
    pub document_token: usize,
    pub database_token: usize,
}

#[derive(Clone)]
pub struct DrawingTarget {
    pub native_key: NativeDocumentKey,
    pub drawing: Drawing,
}

impl DrawingRegistry {
    pub const fn new() -> Self {
        Self {
            drawings: Vec::new(),
        }
    }

    pub fn replace_snapshot(&mut self, native_documents: Vec<NativeDocumentSnapshot>) {
        let mut reserved_ids = self
            .drawings
            .iter()
            .map(|tracked| tracked.drawing.id)
            .collect::<HashSet<_>>();
        let mut previous = std::mem::take(&mut self.drawings);

        let mut seen_tokens = HashSet::with_capacity(native_documents.len());

        self.drawings.reserve(native_documents.len());

        for native in native_documents {
            if !seen_tokens.insert(native.document_token) {
                continue;
            }

            let id = take_drawing_id(&mut previous, native.document_token)
                .unwrap_or_else(|| new_drawing_id(&mut reserved_ids));
            self.drawings.push(TrackedDrawing::from_native(id, native));
        }
    }

    pub fn list(&self) -> Vec<Drawing> {
        self.drawings
            .iter()
            .map(|tracked| tracked.drawing.clone())
            .collect()
    }

    pub fn find_by_id(&self, id: DrawingId) -> Option<DrawingTarget> {
        self.drawings
            .iter()
            .find(|tracked| tracked.drawing.id == id)
            .map(TrackedDrawing::target)
    }

    pub fn find_by_path(&self, path: &DrawingPath) -> Option<DrawingTarget> {
        self.drawings
            .iter()
            .find(|tracked| {
                tracked
                    .drawing
                    .file_path()
                    .is_some_and(|file_path| path.matches(file_path.as_str()))
            })
            .map(TrackedDrawing::target)
    }
}

impl TrackedDrawing {
    fn from_native(id: DrawingId, native: NativeDocumentSnapshot) -> Self {
        Self {
            native_key: NativeDocumentKey {
                document_token: native.document_token,
                database_token: native.database_token,
            },
            drawing: Drawing {
                id,
                name: DrawingName::from_native(native.name, native.named),
                modified: native.modified,
                read_only: native.read_only,
            },
        }
    }

    fn target(&self) -> DrawingTarget {
        DrawingTarget {
            native_key: self.native_key,
            drawing: self.drawing.clone(),
        }
    }
}

impl Drawing {
    pub fn display_name(&self) -> &str {
        match &self.name {
            DrawingName::Named(path) => path.file_name().unwrap_or(path.as_str()),
            DrawingName::Unnamed(name) => name,
        }
    }

    pub fn file_path(&self) -> Option<&Utf8Path> {
        match &self.name {
            DrawingName::Named(path) => Some(path),
            DrawingName::Unnamed(_) => None,
        }
    }
}

fn take_drawing_id(drawings: &mut Vec<TrackedDrawing>, document_token: usize) -> Option<DrawingId> {
    let position = drawings
        .iter()
        .position(|drawing| drawing.native_key.document_token == document_token)?;
    Some(drawings.swap_remove(position).drawing.id)
}

fn new_drawing_id(reserved_ids: &mut HashSet<DrawingId>) -> DrawingId {
    loop {
        let Ok(id) = nanoid::nanoid!(DRAWING_ID_LENGTH, &DRAWING_ID_ALPHABET).parse() else {
            continue;
        };

        if reserved_ids.insert(id) {
            return id;
        }
    }
}

impl DrawingName {
    fn from_native(mut name: String, named: bool) -> Self {
        if named {
            return Self::Named(name.into());
        }

        if name
            .get(name.len().saturating_sub(4)..)
            .is_some_and(|suffix| suffix.eq_ignore_ascii_case(".dwg"))
        {
            name.truncate(name.len() - 4);
        }

        Self::Unnamed(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_identity_while_replacing_native_state() {
        let mut drawings = DrawingRegistry::new();
        drawings.replace_snapshot(vec![named_drawing(1, "/tmp/house.dwg")]);
        let original_id = drawings.list()[0].id;

        drawings.replace_snapshot(vec![NativeDocumentSnapshot {
            database_token: 99,
            modified: true,
            read_only: true,
            ..named_drawing(1, "/tmp/site.dwg")
        }]);

        let listed = drawings.list();
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
            drawings
                .find_by_id(original_id)
                .unwrap()
                .native_key
                .database_token,
            99
        );
    }

    #[test]
    fn follows_native_order_and_removes_closed_drawings() {
        let mut drawings = DrawingRegistry::new();
        drawings.replace_snapshot(vec![
            named_drawing(1, "/tmp/house.dwg"),
            named_drawing(2, "/tmp/site.dwg"),
        ]);
        let original = drawings.list();

        drawings.replace_snapshot(vec![
            named_drawing(2, "/tmp/site.dwg"),
            unnamed_drawing(3, "Drawing1.dwg"),
        ]);

        let listed = drawings.list();
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
    fn strips_drawing_suffix_only_from_unnamed_drawings() {
        assert_eq!(
            DrawingName::from_native("Drawing1.DWG".into(), false),
            DrawingName::Unnamed("Drawing1".into())
        );
        assert_eq!(
            DrawingName::from_native("/tmp/house.DWG".into(), true),
            DrawingName::Named("/tmp/house.DWG".into())
        );
        assert_eq!(
            DrawingName::from_native("図面".into(), false),
            DrawingName::Unnamed("図面".into())
        );
    }

    #[test]
    fn finds_drawings_by_id_and_named_path() {
        let house = drawing_path("house");
        let other = drawing_path("other");
        let mut drawings = DrawingRegistry::new();
        drawings.replace_snapshot(vec![
            named_drawing(1, house.as_str()),
            unnamed_drawing(2, "Drawing1.dwg"),
        ]);
        let listed = drawings.list();

        let by_id = drawings.find_by_id(listed[0].id).unwrap();
        assert_eq!(
            by_id.native_key,
            NativeDocumentKey {
                document_token: 1,
                database_token: 101,
            }
        );
        assert_eq!(
            by_id.drawing.display_name(),
            house.as_path().file_name().unwrap()
        );
        assert_eq!(
            by_id.drawing.file_path().map(Utf8Path::as_str),
            Some(house.as_str())
        );
        assert_eq!(
            drawings
                .find_by_path(&house)
                .unwrap()
                .native_key
                .document_token,
            1
        );
        assert!(drawings.find_by_path(&other).is_none());
    }

    fn named_drawing(document_token: usize, name: &str) -> NativeDocumentSnapshot {
        NativeDocumentSnapshot {
            document_token,
            database_token: document_token + 100,
            name: name.into(),
            named: true,
            modified: false,
            read_only: false,
        }
    }

    fn unnamed_drawing(document_token: usize, name: &str) -> NativeDocumentSnapshot {
        NativeDocumentSnapshot {
            named: false,
            ..named_drawing(document_token, name)
        }
    }

    fn drawing_path(name: &str) -> DrawingPath {
        let path = std::env::temp_dir().join(format!(
            "acadctl-drawings-{}-{name}.dwg",
            std::process::id()
        ));
        std::fs::write(&path, []).unwrap();
        let drawing = DrawingPath::canonicalize(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        drawing
    }
}
