//! Minimal in-memory equivalent of Go's `mime/multipart.FileHeader`,
//! covering the parts the Caspar node relies on.

use std::collections::HashMap;
use std::io::Cursor;

/// Describes a file uploaded as part of a multipart request.
#[derive(Debug, Clone, Default)]
pub struct FileHeader {
    pub filename: String,
    pub header: HashMap<String, Vec<String>>,
    pub size: i64,
    /// Full file contents held in memory (Go keeps either a temp file or an
    /// in-memory buffer; the node always reads the whole file, so we buffer it).
    pub content: Vec<u8>,
}

impl FileHeader {
    /// Equivalent of Go's `(*FileHeader).Open()`.
    pub fn open(&self) -> std::io::Result<Cursor<Vec<u8>>> {
        Ok(Cursor::new(self.content.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn open_returns_a_cursor_that_yields_the_full_content() {
        let fh = FileHeader {
            filename: "f".to_string(),
            content: b"hello world".to_vec(),
            size: 11,
            header: HashMap::new(),
        };
        let mut cur = fh.open().expect("open");
        let mut buf = Vec::new();
        cur.read_to_end(&mut buf).unwrap();
        assert_eq!(buf, b"hello world");
    }

    #[test]
    fn open_clones_content_so_repeated_reads_are_independent() {
        let fh = FileHeader {
            filename: "f".to_string(),
            content: b"abc".to_vec(),
            ..FileHeader::default()
        };
        let mut a = fh.open().unwrap();
        let mut b = fh.open().unwrap();
        let mut buf_a = Vec::new();
        let mut buf_b = Vec::new();
        a.read_to_end(&mut buf_a).unwrap();
        b.read_to_end(&mut buf_b).unwrap();
        assert_eq!(buf_a, buf_b);
        assert_eq!(buf_a, b"abc");
    }

    #[test]
    fn default_is_empty() {
        let fh = FileHeader::default();
        assert!(fh.filename.is_empty());
        assert!(fh.content.is_empty());
        assert_eq!(fh.size, 0);
        assert!(fh.header.is_empty());
    }
}
