//! Helpers for digesting camera file metadata into the form the pipeline
//! needs. Filename extension is the only classification signal we get from
//! libgphoto2, so that's the primary path.

use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetKind {
    Jpeg,
    Raf,
    Mov,
    Other,
}

impl AssetKind {
    pub fn from_filename(filename: &str) -> Self {
        let ext: String = filename
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_uppercase())
            .unwrap_or_default();
        match ext.as_str() {
            "JPG" | "JPEG" => AssetKind::Jpeg,
            "RAF" => AssetKind::Raf,
            "MOV" | "MP4" => AssetKind::Mov,
            _ => AssetKind::Other,
        }
    }
}

/// A digested view of a file on the camera we can carry into the pipeline.
#[derive(Debug, Clone)]
pub struct ObjectInfo {
    pub filename: String,
    pub size: u64,
    pub date_created_utc: DateTime<Utc>,
    pub kind: AssetKind,
}

impl ObjectInfo {
    pub fn basename(&self) -> &str {
        self.filename
            .rsplit_once('.')
            .map(|(b, _)| b)
            .unwrap_or(&self.filename)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn classifies_jpeg() {
        assert_eq!(AssetKind::from_filename("DSCF1234.JPG"), AssetKind::Jpeg);
        assert_eq!(AssetKind::from_filename("a.jpeg"), AssetKind::Jpeg);
    }

    #[test]
    fn classifies_raf() {
        assert_eq!(AssetKind::from_filename("DSCF1234.RAF"), AssetKind::Raf);
    }

    #[test]
    fn classifies_mov() {
        assert_eq!(AssetKind::from_filename("DSCF0001.MOV"), AssetKind::Mov);
        assert_eq!(AssetKind::from_filename("clip.mp4"), AssetKind::Mov);
    }

    #[test]
    fn classifies_unknown_as_other() {
        assert_eq!(AssetKind::from_filename("weird.bin"), AssetKind::Other);
    }

    #[test]
    fn basename_strips_extension() {
        let info = ObjectInfo {
            filename: "DSCF1234.RAF".into(),
            size: 0,
            date_created_utc: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            kind: AssetKind::Raf,
        };
        assert_eq!(info.basename(), "DSCF1234");
    }

    #[test]
    fn basename_without_extension() {
        let info = ObjectInfo {
            filename: "weird".into(),
            size: 0,
            date_created_utc: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            kind: AssetKind::Other,
        };
        assert_eq!(info.basename(), "weird");
    }
}
