use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub(super) const MANAGED_SCREENSHOT_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);

const MANAGED_PARENT_DIRECTORY: &str = "acadctl";
const MANAGED_SCREENSHOT_DIRECTORY: &str = "screenshots";
const FILE_PREFIX: &str = "acadctl-screenshot-";
const FILE_SUFFIX: &str = ".png";

#[derive(Debug)]
pub(super) enum OutputError {
    InvalidTimestamp,
    InvalidOutput {
        path: PathBuf,
        reason: &'static str,
    },
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
}

impl fmt::Display for OutputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTimestamp => write!(
                formatter,
                "Invalid screenshot timestamp: expected YYYYMMDDTHHMMSS.mmmZ."
            ),
            Self::InvalidOutput { path, reason } => {
                write!(
                    formatter,
                    "Invalid screenshot output '{}': {reason}.",
                    path.display()
                )
            }
            Self::Io {
                operation,
                path,
                source,
            } => write!(
                formatter,
                "Screenshot output failed while {operation} '{}': {source}.",
                path.display()
            ),
        }
    }
}

impl Error for OutputError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

pub(super) fn publish_png(
    png: &[u8],
    output: Option<&Path>,
    timestamp: &str,
) -> Result<PathBuf, OutputError> {
    publish_png_in(
        png,
        output,
        timestamp,
        &std::env::temp_dir(),
        SystemTime::now(),
    )
}

fn publish_png_in(
    png: &[u8],
    output: Option<&Path>,
    timestamp: &str,
    system_temp_directory: &Path,
    now: SystemTime,
) -> Result<PathBuf, OutputError> {
    if !is_valid_timestamp(timestamp) {
        return Err(OutputError::InvalidTimestamp);
    }

    let destination = match output {
        None => {
            let system_temp_directory = absolute_path(system_temp_directory)?;
            let directory = prepare_managed_directory(&system_temp_directory)?;
            cleanup_managed_directory(&directory, now)?;
            Destination::Generated(directory)
        }
        Some(path) => match fs::metadata(path) {
            Ok(metadata) if metadata.is_dir() => {
                Destination::Generated(canonicalize_directory(path)?)
            }
            Ok(_) => {
                return Err(OutputError::InvalidOutput {
                    path: path.to_owned(),
                    reason: "destination already exists",
                });
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if fs::symlink_metadata(path).is_ok() {
                    return Err(OutputError::InvalidOutput {
                        path: path.to_owned(),
                        reason: "destination already exists",
                    });
                }

                let filename = path.file_name().ok_or_else(|| OutputError::InvalidOutput {
                    path: path.to_owned(),
                    reason: "file name is missing",
                })?;
                let parent = existing_parent(path);
                require_directory(parent)?;
                Destination::Exact(canonicalize_directory(parent)?.join(filename))
            }
            Err(source) => return Err(io_error("inspecting destination", path, source)),
        },
    };

    let resolved_path = match &destination {
        Destination::Exact(path) => path,
        Destination::Generated(directory) => directory,
    };
    if resolved_path.to_str().is_none() {
        return Err(OutputError::InvalidOutput {
            path: resolved_path.clone(),
            reason: "path is not valid UTF-8",
        });
    }

    let directory = destination.directory();
    let mut staged = StagedFile::create(directory, timestamp)?;
    staged
        .file_mut()
        .write_all(png)
        .map_err(|source| io_error("writing temporary file", &staged.path, source))?;
    staged
        .file_mut()
        .sync_all()
        .map_err(|source| io_error("syncing temporary file", &staged.path, source))?;
    staged.close();

    match destination {
        Destination::Exact(path) => {
            publish_link(&staged.path, &path).map_err(|source| {
                if source.kind() == io::ErrorKind::AlreadyExists {
                    OutputError::InvalidOutput {
                        path: path.clone(),
                        reason: "destination already exists",
                    }
                } else {
                    io_error("publishing file", &path, source)
                }
            })?;
            staged.remove();
            Ok(path)
        }
        Destination::Generated(directory) => {
            let mut ordinal = 1_u64;
            loop {
                let path = directory.join(generated_filename(timestamp, ordinal));
                match publish_link(&staged.path, &path) {
                    Ok(()) => {
                        staged.remove();
                        return Ok(path);
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        ordinal =
                            ordinal
                                .checked_add(1)
                                .ok_or_else(|| OutputError::InvalidOutput {
                                    path: directory.clone(),
                                    reason: "screenshot filename ordinals are exhausted",
                                })?;
                    }
                    Err(source) => return Err(io_error("publishing file", &path, source)),
                }
            }
        }
    }
}

fn prepare_managed_directory(system_temp_directory: &Path) -> Result<PathBuf, OutputError> {
    let parent = system_temp_directory.join(MANAGED_PARENT_DIRECTORY);
    create_real_directory(&parent)?;
    let directory = parent.join(MANAGED_SCREENSHOT_DIRECTORY);
    create_real_directory(&directory)?;
    Ok(directory)
}

fn create_real_directory(path: &Path) -> Result<(), OutputError> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            match fs::symlink_metadata(path) {
                Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
                Ok(_) => Err(OutputError::InvalidOutput {
                    path: path.to_owned(),
                    reason: "managed path is not a real directory",
                }),
                Err(source) => Err(io_error("inspecting managed directory", path, source)),
            }
        }
        Err(source) => Err(io_error("creating managed directory", path, source)),
    }
}

enum Destination {
    Exact(PathBuf),
    Generated(PathBuf),
}

impl Destination {
    fn directory(&self) -> &Path {
        match self {
            Self::Exact(path) => path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
            Self::Generated(directory) => directory,
        }
    }
}

struct StagedFile {
    file: Option<File>,
    path: PathBuf,
    remove_on_drop: bool,
}

impl StagedFile {
    fn create(directory: &Path, timestamp: &str) -> Result<Self, OutputError> {
        let mut ordinal = 1_u64;
        loop {
            let filename = if ordinal == 1 {
                format!(".{FILE_PREFIX}{timestamp}{FILE_SUFFIX}.publishing")
            } else {
                format!(".{FILE_PREFIX}{timestamp}{FILE_SUFFIX}.publishing--{ordinal}")
            };
            let path = directory.join(filename);
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        file: Some(file),
                        path,
                        remove_on_drop: true,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    ordinal = ordinal
                        .checked_add(1)
                        .ok_or_else(|| OutputError::InvalidOutput {
                            path: directory.to_owned(),
                            reason: "temporary filename ordinals are exhausted",
                        })?;
                }
                Err(source) => return Err(io_error("creating temporary file", &path, source)),
            }
        }
    }

    fn file_mut(&mut self) -> &mut File {
        self.file
            .as_mut()
            .expect("a staged file is open until writing and syncing finish")
    }

    fn close(&mut self) {
        self.file = None;
    }

    fn remove(&mut self) {
        if fs::remove_file(&self.path).is_ok() {
            self.remove_on_drop = false;
        }
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        if self.remove_on_drop {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn publish_link(staged: &Path, destination: &Path) -> io::Result<()> {
    fs::hard_link(staged, destination)
}

fn existing_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

fn canonicalize_directory(path: &Path) -> Result<PathBuf, OutputError> {
    fs::canonicalize(path).map_err(|source| io_error("resolving directory", path, source))
}

fn absolute_path(path: &Path) -> Result<PathBuf, OutputError> {
    if path.is_absolute() {
        return Ok(path.to_owned());
    }

    std::env::current_dir()
        .map(|directory| directory.join(path))
        .map_err(|source| io_error("resolving current directory", path, source))
}

fn require_directory(path: &Path) -> Result<(), OutputError> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(OutputError::InvalidOutput {
            path: path.to_owned(),
            reason: "parent is not a directory",
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Err(OutputError::InvalidOutput {
            path: path.to_owned(),
            reason: "parent directory does not exist",
        }),
        Err(source) => Err(io_error("inspecting directory", path, source)),
    }
}

fn cleanup_managed_directory(directory: &Path, now: SystemTime) -> Result<(), OutputError> {
    let entries = fs::read_dir(directory)
        .map_err(|source| io_error("reading managed directory", directory, source))?;

    for entry in entries {
        let entry =
            entry.map_err(|source| io_error("reading managed directory", directory, source))?;
        let path = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !is_managed_filename(&name) {
            continue;
        }

        let metadata = fs::symlink_metadata(&path)
            .map_err(|source| io_error("inspecting managed file", &path, source))?;
        if !metadata.file_type().is_file() {
            continue;
        }

        let modified = metadata
            .modified()
            .map_err(|source| io_error("reading managed file time", &path, source))?;
        let is_expired = now
            .duration_since(modified)
            .is_ok_and(|age| age > MANAGED_SCREENSHOT_RETENTION);
        if is_expired {
            fs::remove_file(&path)
                .map_err(|source| io_error("removing expired screenshot", &path, source))?;
        }
    }

    Ok(())
}

fn generated_filename(timestamp: &str, ordinal: u64) -> String {
    if ordinal == 1 {
        format!("{FILE_PREFIX}{timestamp}{FILE_SUFFIX}")
    } else {
        format!("{FILE_PREFIX}{timestamp}--{ordinal}{FILE_SUFFIX}")
    }
}

fn is_managed_filename(filename: &str) -> bool {
    let Some(body) = filename
        .strip_prefix(FILE_PREFIX)
        .and_then(|name| name.strip_suffix(FILE_SUFFIX))
    else {
        return false;
    };
    let Some(timestamp) = body
        .as_bytes()
        .get(..20)
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
    else {
        return false;
    };
    if !is_valid_timestamp(timestamp) {
        return false;
    }

    let ordinal = &body[timestamp.len()..];
    if ordinal.is_empty() {
        return true;
    }
    let Some(digits) = ordinal.strip_prefix("--") else {
        return false;
    };
    if digits.is_empty()
        || digits.starts_with('0')
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return false;
    }
    digits.parse::<u64>().is_ok_and(|value| value >= 2)
}

fn is_valid_timestamp(timestamp: &str) -> bool {
    if timestamp.len() != 20 {
        return false;
    }
    let bytes = timestamp.as_bytes();
    if bytes[8] != b'T' || bytes[15] != b'.' || bytes[19] != b'Z' {
        return false;
    }
    for range in [0..8, 9..15, 16..19] {
        if !bytes[range].iter().all(u8::is_ascii_digit) {
            return false;
        }
    }

    let number = |range: std::ops::Range<usize>| -> u32 {
        bytes[range]
            .iter()
            .fold(0, |value, digit| value * 10 + u32::from(digit - b'0'))
    };
    let year = number(0..4);
    let month = number(4..6);
    let day = number(6..8);
    let hour = number(9..11);
    let minute = number(11..13);
    let second = number(13..15);

    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => return false,
    };
    year != 0 && day != 0 && day <= days_in_month && hour < 24 && minute < 60 && second < 60
}

fn is_leap_year(year: u32) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> OutputError {
    OutputError::Io {
        operation,
        path: path.to_owned(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);
    const TIMESTAMP: &str = "20260818T123456.789Z";

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "acadctl-output-tests-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn omitted_output_uses_and_creates_the_managed_directory() {
        let root = TestDirectory::new();
        let path = publish_png_in(b"png", None, TIMESTAMP, &root.0, SystemTime::now()).unwrap();

        assert_eq!(
            path,
            root.0
                .join("acadctl/screenshots")
                .join("acadctl-screenshot-20260818T123456.789Z.png")
        );
        assert_eq!(fs::read(path).unwrap(), b"png");
    }

    #[test]
    fn existing_directory_generates_collision_ordinals_from_two() {
        let root = TestDirectory::new();
        let first = root.0.join(generated_filename(TIMESTAMP, 1));
        fs::write(&first, b"first").unwrap();

        let second = publish_png_in(
            b"second",
            Some(&root.0),
            TIMESTAMP,
            &root.0,
            SystemTime::now(),
        )
        .unwrap();

        assert_eq!(
            second,
            fs::canonicalize(&root.0)
                .unwrap()
                .join(generated_filename(TIMESTAMP, 2))
        );
        assert!(second.is_absolute());
        assert_eq!(fs::read(first).unwrap(), b"first");
        assert_eq!(fs::read(second).unwrap(), b"second");
    }

    #[test]
    fn nonexistent_output_is_used_as_the_exact_file_path() {
        let root = TestDirectory::new();
        let path = root.0.join("capture.with-any-extension");

        let written = publish_png_in(
            b"complete png",
            Some(&path),
            TIMESTAMP,
            &root.0,
            SystemTime::now(),
        )
        .unwrap();

        assert_eq!(
            written,
            fs::canonicalize(&root.0)
                .unwrap()
                .join("capture.with-any-extension")
        );
        assert!(written.is_absolute());
        assert_eq!(fs::read(written).unwrap(), b"complete png");
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_output_is_rejected_before_creating_a_file() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let root = TestDirectory::new();
        let path = root
            .0
            .join(OsString::from_vec(b"capture-\xff.png".to_vec()));

        let error =
            publish_png_in(b"png", Some(&path), TIMESTAMP, &root.0, SystemTime::now()).unwrap_err();

        assert!(matches!(error, OutputError::InvalidOutput { .. }));
        assert!(!path.exists());
    }

    #[test]
    fn nonexistent_parent_is_rejected_and_not_created() {
        let root = TestDirectory::new();
        let parent = root.0.join("missing");
        let path = parent.join("capture.png");

        let error =
            publish_png_in(b"png", Some(&path), TIMESTAMP, &root.0, SystemTime::now()).unwrap_err();

        assert!(matches!(error, OutputError::InvalidOutput { .. }));
        assert!(!parent.exists());
    }

    #[test]
    fn existing_file_is_never_overwritten() {
        let root = TestDirectory::new();
        let path = root.0.join("capture.png");
        fs::write(&path, b"original").unwrap();

        let error = publish_png_in(
            b"replacement",
            Some(&path),
            TIMESTAMP,
            &root.0,
            SystemTime::now(),
        )
        .unwrap_err();

        assert!(matches!(error, OutputError::InvalidOutput { .. }));
        assert_eq!(fs::read(path).unwrap(), b"original");
    }

    #[test]
    fn exact_output_is_race_safe_and_never_partially_visible() {
        let root = TestDirectory::new();
        let path = root.0.join("capture.png");
        let first_path = path.clone();
        let second_path = path.clone();

        let first = std::thread::spawn(move || {
            publish_png(b"first complete image", Some(&first_path), TIMESTAMP)
        });
        let second = std::thread::spawn(move || {
            publish_png(b"second complete image", Some(&second_path), TIMESTAMP)
        });
        let results = [first.join().unwrap(), second.join().unwrap()];

        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
        let contents = fs::read(path).unwrap();
        assert!(contents == b"first complete image" || contents == b"second complete image");
    }

    #[test]
    fn explicit_directory_is_never_cleaned() {
        let root = TestDirectory::new();
        let old = root.0.join(generated_filename("20200101T000000.000Z", 1));
        fs::write(&old, b"keep").unwrap();
        let future = SystemTime::now() + MANAGED_SCREENSHOT_RETENTION + Duration::from_secs(1);

        publish_png_in(b"new", Some(&root.0), TIMESTAMP, &root.0, future).unwrap();

        assert_eq!(fs::read(old).unwrap(), b"keep");
    }

    #[test]
    fn managed_cleanup_removes_only_expired_direct_regular_grammar_matches() {
        let root = TestDirectory::new();
        let managed = root
            .0
            .join(MANAGED_PARENT_DIRECTORY)
            .join(MANAGED_SCREENSHOT_DIRECTORY);
        fs::create_dir_all(&managed).unwrap();
        let expired = managed.join(generated_filename("20200101T000000.000Z", 2));
        let malformed = managed.join("acadctl-screenshot-20200101T000000.000Z--02.png");
        let unrelated = managed.join("other.png");
        let named_directory = managed.join(generated_filename("20200101T000000.000Z", 3));
        let nested = managed.join("nested");
        let nested_match = nested.join(generated_filename("20200101T000000.000Z", 4));
        fs::write(&expired, b"delete").unwrap();
        fs::write(&malformed, b"keep").unwrap();
        fs::write(&unrelated, b"keep").unwrap();
        fs::create_dir(&named_directory).unwrap();
        fs::create_dir(&nested).unwrap();
        fs::write(&nested_match, b"keep").unwrap();

        let future = SystemTime::now() + MANAGED_SCREENSHOT_RETENTION + Duration::from_secs(1);
        cleanup_managed_directory(&managed, future).unwrap();

        assert!(!expired.exists());
        assert!(malformed.exists());
        assert!(unrelated.exists());
        assert!(named_directory.is_dir());
        assert!(nested_match.exists());
    }

    #[cfg(unix)]
    #[test]
    fn managed_cleanup_does_not_follow_symlinks() {
        let root = TestDirectory::new();
        let managed = root
            .0
            .join(MANAGED_PARENT_DIRECTORY)
            .join(MANAGED_SCREENSHOT_DIRECTORY);
        fs::create_dir_all(&managed).unwrap();
        let target = root.0.join("target.png");
        let link = managed.join(generated_filename("20200101T000000.000Z", 1));
        fs::write(&target, b"keep").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let future = SystemTime::now() + MANAGED_SCREENSHOT_RETENTION + Duration::from_secs(1);
        cleanup_managed_directory(&managed, future).unwrap();

        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(fs::read(target).unwrap(), b"keep");
    }

    #[cfg(unix)]
    #[test]
    fn managed_cleanup_rejects_a_symlinked_managed_ancestor() {
        let root = TestDirectory::new();
        let outside = root.0.join("outside");
        let outside_screenshots = outside.join(MANAGED_SCREENSHOT_DIRECTORY);
        fs::create_dir_all(&outside_screenshots).unwrap();
        let protected = outside_screenshots.join(generated_filename("20200101T000000.000Z", 1));
        fs::write(&protected, b"keep").unwrap();
        std::os::unix::fs::symlink(&outside, root.0.join(MANAGED_PARENT_DIRECTORY)).unwrap();
        let future = SystemTime::now() + MANAGED_SCREENSHOT_RETENTION + Duration::from_secs(1);

        let error = publish_png_in(b"new", None, TIMESTAMP, &root.0, future).unwrap_err();

        assert!(matches!(error, OutputError::InvalidOutput { .. }));
        assert_eq!(fs::read(protected).unwrap(), b"keep");
    }

    #[test]
    fn validates_timestamp_and_complete_managed_filename_grammar() {
        for valid in ["20260818T123456.789Z", "20240229T235959.000Z"] {
            assert!(is_valid_timestamp(valid));
        }
        for invalid in [
            "20260818-123456.789Z",
            "20260818T123456Z",
            "20260229T123456.789Z",
            "20260818T246000.000Z",
            "00000818T123456.789Z",
        ] {
            assert!(!is_valid_timestamp(invalid), "{invalid}");
        }

        for valid in [
            "acadctl-screenshot-20260818T123456.789Z.png",
            "acadctl-screenshot-20260818T123456.789Z--2.png",
            "acadctl-screenshot-20260818T123456.789Z--18446744073709551615.png",
        ] {
            assert!(is_managed_filename(valid), "{valid}");
        }
        for invalid in [
            "acadctl-screenshot-20260818T123456.789Z--1.png",
            "acadctl-screenshot-20260818T123456.789Z--02.png",
            "acadctl-screenshot-20260818T123456.789Z--2.PNG",
            "prefix-acadctl-screenshot-20260818T123456.789Z.png",
            "acadctl-screenshot-20260818T123456.789Z.png.backup",
            "acadctl-screenshot-20260818T12345é.789Z.png",
        ] {
            assert!(!is_managed_filename(invalid), "{invalid}");
        }
    }

    #[test]
    fn invalid_timestamp_is_rejected_before_any_filesystem_change() {
        let root = TestDirectory::new();

        let error = publish_png_in(
            b"png",
            None,
            "2026-08-18T12:34:56Z",
            &root.0,
            SystemTime::now(),
        )
        .unwrap_err();

        assert!(matches!(error, OutputError::InvalidTimestamp));
        assert!(!root.0.join("acadctl").exists());
    }
}
