use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use camino::{Utf8Path, Utf8PathBuf};

use crate::MAX_DRAWING_PATH_BYTES;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DrawingPath(Utf8PathBuf);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavePath(Utf8PathBuf);

#[derive(Debug)]
pub enum DrawingPathError {
    NotDwg,
    NotFile(PathBuf),
    NotAbsolute,
    TooLong,
    Resolve { path: PathBuf, source: io::Error },
    InvalidUtf8(OsString),
    AlreadyExists(PathBuf),
}

impl DrawingPath {
    pub fn canonicalize(path: impl AsRef<Path>) -> Result<Self, DrawingPathError> {
        let path = path.as_ref();

        if !Self::has_dwg_extension(path) {
            return Err(DrawingPathError::NotDwg);
        }

        if !path.is_file() {
            return Err(DrawingPathError::NotFile(path.to_owned()));
        }

        let canonical =
            std::fs::canonicalize(path).map_err(|source| DrawingPathError::Resolve {
                path: path.to_owned(),
                source,
            })?;
        let canonical = Utf8PathBuf::from_path_buf(canonical)
            .map_err(|path| DrawingPathError::InvalidUtf8(path.into_os_string()))?;

        Self::from_canonical(canonical)
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn as_path(&self) -> &Utf8Path {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0.into_string()
    }

    pub fn has_dwg_extension(path: impl AsRef<Path>) -> bool {
        path.as_ref()
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("dwg"))
    }

    pub fn matches(&self, path: &str) -> bool {
        paths_equal(self.as_str(), path)
    }

    fn from_canonical(path: Utf8PathBuf) -> Result<Self, DrawingPathError> {
        if !path.is_absolute() {
            return Err(DrawingPathError::NotAbsolute);
        }

        if path.as_str().len() > MAX_DRAWING_PATH_BYTES {
            return Err(DrawingPathError::TooLong);
        }

        if !Self::has_dwg_extension(path.as_std_path()) {
            return Err(DrawingPathError::NotDwg);
        }

        Ok(Self(path))
    }
}

impl SavePath {
    pub fn prepare(path: impl AsRef<Path>) -> Result<Self, DrawingPathError> {
        let path = Self::normalize(path.as_ref())?;
        path.ensure_available()?;
        Ok(path)
    }

    fn normalize(path: &Path) -> Result<Self, DrawingPathError> {
        if !DrawingPath::has_dwg_extension(path) {
            return Err(DrawingPathError::NotDwg);
        }

        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent = std::fs::canonicalize(parent).map_err(|source| DrawingPathError::Resolve {
            path: path.to_owned(),
            source,
        })?;
        let file_name = path.file_name().ok_or(DrawingPathError::NotAbsolute)?;
        let path = Utf8PathBuf::from_path_buf(parent.join(file_name))
            .map_err(|path| DrawingPathError::InvalidUtf8(path.into_os_string()))?;

        if path.as_str().len() > MAX_DRAWING_PATH_BYTES {
            return Err(DrawingPathError::TooLong);
        }

        Ok(Self(path))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn ensure_available(&self) -> Result<(), DrawingPathError> {
        match std::fs::symlink_metadata(self.0.as_std_path()) {
            Ok(_) => Err(DrawingPathError::AlreadyExists(
                self.0.as_std_path().to_owned(),
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(DrawingPathError::Resolve {
                path: self.0.as_std_path().to_owned(),
                source,
            }),
        }
    }

    pub fn into_string(self) -> String {
        self.0.into_string()
    }
}

impl fmt::Display for DrawingPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for DrawingPath {
    type Err = DrawingPathError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() > MAX_DRAWING_PATH_BYTES {
            return Err(DrawingPathError::TooLong);
        }

        if !Utf8Path::new(value).is_absolute() {
            return Err(DrawingPathError::NotAbsolute);
        }

        Self::canonicalize(value)
    }
}

impl FromStr for SavePath {
    type Err = DrawingPathError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if !Utf8Path::new(value).is_absolute() {
            return Err(DrawingPathError::NotAbsolute);
        }

        Self::normalize(Path::new(value))
    }
}

impl fmt::Display for DrawingPathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotDwg => formatter.write_str("Only DWG files can be opened"),
            Self::NotFile(path) => {
                write!(formatter, "DWG file '{}' does not exist", path.display())
            }
            Self::NotAbsolute => formatter.write_str("The drawing path must be absolute"),
            Self::TooLong => formatter.write_str("The drawing path exceeds the 32 KiB limit"),
            Self::Resolve { path, source } => {
                write!(
                    formatter,
                    "Could not resolve '{}' ({})",
                    path.display(),
                    io_error_description(source)
                )
            }
            Self::InvalidUtf8(path) => write!(
                formatter,
                "Drawing path '{}' is not valid UTF-8",
                path.to_string_lossy()
            ),
            Self::AlreadyExists(path) => write!(
                formatter,
                "File '{}' already exists; use another path or omit --as",
                path.display()
            ),
        }
    }
}

fn io_error_description(error: &io::Error) -> &'static str {
    match error.kind() {
        io::ErrorKind::NotFound => "file not found",
        io::ErrorKind::PermissionDenied => "permission denied",
        _ => "I/O error",
    }
}

impl std::error::Error for DrawingPathError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Resolve { source, .. } => Some(source),
            Self::NotDwg
            | Self::NotFile(_)
            | Self::NotAbsolute
            | Self::TooLong
            | Self::InvalidUtf8(_)
            | Self::AlreadyExists(_) => None,
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_dwg_and_non_absolute_wire_paths() {
        assert!(matches!(
            "/tmp/drawing.dxf".parse::<DrawingPath>(),
            Err(DrawingPathError::NotDwg)
        ));
        assert!(matches!(
            "drawing.dwg".parse::<DrawingPath>(),
            Err(DrawingPathError::NotAbsolute)
        ));
    }

    #[test]
    fn canonicalizes_an_existing_drawing() {
        let path = std::env::temp_dir().join(format!(
            "acadctl-drawing-path-{}-{}.dwg",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::write(&path, []).unwrap();

        let drawing = DrawingPath::canonicalize(&path).unwrap();
        let expected = std::fs::canonicalize(&path).unwrap();
        assert_eq!(drawing.as_path().as_std_path(), expected);

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn parsing_a_wire_path_preserves_the_canonical_invariant() {
        let path = std::env::temp_dir().join(format!(
            "acadctl-wire-drawing-path-{}.dwg",
            std::process::id()
        ));
        std::fs::write(&path, []).unwrap();
        let drawing: DrawingPath = path.to_str().unwrap().parse().unwrap();

        assert_eq!(
            drawing.as_path().as_std_path(),
            std::fs::canonicalize(&path).unwrap()
        );

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn prepares_a_new_save_destination_from_a_relative_path() {
        let file_name = format!("acadctl-save-path-{}.dwg", std::process::id());
        let destination = SavePath::prepare(&file_name).unwrap();
        let expected = std::env::current_dir().unwrap().join(file_name);

        assert_eq!(destination.as_str(), expected.to_str().unwrap());
    }

    #[test]
    fn refuses_an_existing_save_destination() {
        let path = std::env::temp_dir().join(format!(
            "acadctl-existing-save-path-{}.dwg",
            std::process::id()
        ));
        std::fs::write(&path, []).unwrap();

        assert!(matches!(
            SavePath::prepare(&path),
            Err(DrawingPathError::AlreadyExists(_))
        ));

        std::fs::remove_file(path).unwrap();
    }
}
