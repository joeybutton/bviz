//! Reading the files a command line names, wherever bviz runs.
//!
//! On a desktop these are the filesystem's own calls. In a browser there is
//! no filesystem: the page that starts bviz hands it each file's bytes under
//! the name the command line gives it ([`provide`]), and the same calls read
//! them from memory. Every reader of a named file goes through here, so a
//! viewer reads in the browser exactly as it does on a desktop.

use std::io;
use std::path::Path;

/// A file opened for reading, which reports its size in bytes.
pub trait Opened: io::Read + io::Seek + Send {
    fn size(&self) -> io::Result<u64>;
}

/// Whether `path` names a regular file: something a reader can take its
/// bytes from more than once, unlike a pipe.
pub fn is_file(path: &Path) -> bool {
    imp::is_file(path)
}

/// Opens `path` for reading.
pub fn open(path: &Path) -> io::Result<Box<dyn Opened>> {
    imp::open(path)
}

/// The whole of `path`.
pub fn read(path: &Path) -> io::Result<Vec<u8>> {
    imp::read(path)
}

#[cfg(not(target_family = "wasm"))]
mod imp {
    use super::Opened;
    use std::io;
    use std::path::Path;

    impl Opened for std::fs::File {
        fn size(&self) -> io::Result<u64> {
            Ok(self.metadata()?.len())
        }
    }

    pub(super) fn is_file(path: &Path) -> bool {
        std::fs::metadata(path).is_ok_and(|m| m.is_file())
    }

    pub(super) fn open(path: &Path) -> io::Result<Box<dyn Opened>> {
        Ok(Box::new(std::fs::File::open(path)?))
    }

    pub(super) fn read(path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }
}

#[cfg(target_family = "wasm")]
pub use imp::provide;

#[cfg(target_family = "wasm")]
mod imp {
    use super::Opened;
    use std::collections::HashMap;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    /// The files the page provided, by the name the command line gives each.
    static FILES: Mutex<Option<HashMap<PathBuf, Arc<[u8]>>>> = Mutex::new(None);

    /// Makes `bytes` the contents of `path`, for the command line to name.
    pub fn provide(path: impl Into<PathBuf>, bytes: Vec<u8>) {
        let mut files = FILES.lock().unwrap_or_else(|e| e.into_inner());
        files
            .get_or_insert_with(HashMap::new)
            .insert(path.into(), bytes.into());
    }

    fn get(path: &Path) -> io::Result<Arc<[u8]>> {
        let files = FILES.lock().unwrap_or_else(|e| e.into_inner());
        files
            .as_ref()
            .and_then(|f| f.get(path).cloned())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "no such file was opened in the page",
                )
            })
    }

    struct Memory(io::Cursor<Arc<[u8]>>);

    impl io::Read for Memory {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.0.read(buf)
        }
    }

    impl io::Seek for Memory {
        fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
            self.0.seek(pos)
        }
    }

    impl Opened for Memory {
        fn size(&self) -> io::Result<u64> {
            Ok(self.0.get_ref().len() as u64)
        }
    }

    pub(super) fn is_file(path: &Path) -> bool {
        get(path).is_ok()
    }

    pub(super) fn open(path: &Path) -> io::Result<Box<dyn Opened>> {
        Ok(Box::new(Memory(io::Cursor::new(get(path)?))))
    }

    pub(super) fn read(path: &Path) -> io::Result<Vec<u8>> {
        Ok(get(path)?.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn reads_a_file_whole_and_opened() {
        let dir = std::env::temp_dir().join(format!("bviz_files_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        std::fs::write(&path, b"hello").unwrap();
        assert!(is_file(&path));
        assert!(!is_file(&dir));
        assert_eq!(read(&path).unwrap(), b"hello");
        let mut opened = open(&path).unwrap();
        assert_eq!(opened.size().unwrap(), 5);
        let mut text = String::new();
        opened.read_to_string(&mut text).unwrap();
        assert_eq!(text, "hello");
        assert!(read(&dir.join("absent")).is_err());
    }
}
