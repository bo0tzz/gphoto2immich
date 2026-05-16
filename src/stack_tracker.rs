//! Pair-up tracker for JPEG + RAF stacks.
//!
//! Whenever we observe (either via fresh upload or via the dedup pre-check)
//! that an asset belongs to a basename's JPEG or RAF slot, we record it here.
//! Once both slots are present we hand off the pair so a stack can be created
//! in Immich. Standalone JPEGs / RAFs (e.g. the camera was set to JPEG-only)
//! linger forever — that's fine, they just never trigger a stack.

use std::collections::HashMap;

use crate::camera::AssetKind;

#[derive(Debug, Default)]
struct StackEntry {
    jpeg: Option<String>,
    raf: Option<String>,
}

impl StackEntry {
    fn place(&mut self, kind: AssetKind, asset_id: String) {
        match kind {
            AssetKind::Jpeg => self.jpeg = Some(asset_id),
            AssetKind::Raf => self.raf = Some(asset_id),
            _ => {}
        }
    }

    fn take_pair(&mut self) -> Option<(String, String)> {
        match (self.jpeg.as_ref(), self.raf.as_ref()) {
            (Some(_), Some(_)) => Some((self.jpeg.take().unwrap(), self.raf.take().unwrap())),
            _ => None,
        }
    }
}

#[derive(Debug, Default)]
pub struct StackTracker {
    entries: HashMap<String, StackEntry>,
}

/// What the tracker decided after recording an observation.
#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    /// Nothing actionable yet — counterpart still missing.
    Wait,
    /// Counterpart already known too — caller should create a stack with
    /// `(jpeg_id, raf_id)` (in that order, JPEG primary).
    Stack { jpeg_id: String, raf_id: String },
    /// Not a stackable kind (e.g. MOV); caller should drop the observation.
    Ignore,
}

impl StackTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that asset `asset_id` (kind = `kind`) was observed for basename
    /// `basename`. Returns a Decision the caller acts on.
    pub fn observe(&mut self, basename: &str, kind: AssetKind, asset_id: String) -> Decision {
        match kind {
            AssetKind::Jpeg | AssetKind::Raf => {}
            _ => return Decision::Ignore,
        }
        let entry = self.entries.entry(basename.to_owned()).or_default();
        entry.place(kind, asset_id);
        match entry.take_pair() {
            Some((jpeg, raf)) => {
                // No further state to track — drop the entry so the map stays
                // bounded over a long-running session.
                self.entries.remove(basename);
                Decision::Stack {
                    jpeg_id: jpeg,
                    raf_id: raf,
                }
            }
            None => Decision::Wait,
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jpeg_alone_waits() {
        let mut t = StackTracker::new();
        assert_eq!(
            t.observe("DSCF0001", AssetKind::Jpeg, "jpeg-1".into()),
            Decision::Wait
        );
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn raf_then_jpeg_yields_stack_in_jpeg_first_order() {
        let mut t = StackTracker::new();
        assert_eq!(
            t.observe("DSCF0001", AssetKind::Raf, "raf-1".into()),
            Decision::Wait
        );
        assert_eq!(
            t.observe("DSCF0001", AssetKind::Jpeg, "jpeg-1".into()),
            Decision::Stack {
                jpeg_id: "jpeg-1".into(),
                raf_id: "raf-1".into()
            }
        );
        assert_eq!(t.len(), 0, "entry should be cleared after pairing");
    }

    #[test]
    fn jpeg_then_raf_also_works() {
        let mut t = StackTracker::new();
        t.observe("X", AssetKind::Jpeg, "j".into());
        let d = t.observe("X", AssetKind::Raf, "r".into());
        assert!(matches!(d, Decision::Stack { .. }));
    }

    #[test]
    fn mov_is_ignored() {
        let mut t = StackTracker::new();
        assert_eq!(
            t.observe("CLIP0001", AssetKind::Mov, "mov-1".into()),
            Decision::Ignore
        );
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn other_kind_is_ignored() {
        let mut t = StackTracker::new();
        assert_eq!(
            t.observe("misc", AssetKind::Other, "x".into()),
            Decision::Ignore
        );
    }

    #[test]
    fn different_basenames_dont_cross() {
        let mut t = StackTracker::new();
        t.observe("A", AssetKind::Jpeg, "ja".into());
        t.observe("B", AssetKind::Raf, "rb".into());
        // Neither pair is complete.
        assert_eq!(t.len(), 2);
    }

    #[test]
    fn re_pair_after_completion() {
        // If somehow we see the same basename twice (shouldn't happen in
        // practice), the tracker should pair up again rather than stalling.
        let mut t = StackTracker::new();
        t.observe("X", AssetKind::Jpeg, "j1".into());
        t.observe("X", AssetKind::Raf, "r1".into());
        // Now both slots cleared. Another round should also pair.
        t.observe("X", AssetKind::Jpeg, "j2".into());
        let d = t.observe("X", AssetKind::Raf, "r2".into());
        assert_eq!(
            d,
            Decision::Stack {
                jpeg_id: "j2".into(),
                raf_id: "r2".into()
            }
        );
    }
}
