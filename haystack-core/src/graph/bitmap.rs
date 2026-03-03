// Tag bitmap index — roaring bitmap-based tag-presence queries.

use roaring::RoaringBitmap;
use std::collections::HashMap;

/// Tag-presence bitmap index.
///
/// Maps tag names to compressed roaring bitmaps. Each entity is assigned
/// a numeric id; the bit at position `id` is set in every bitmap for every
/// tag the entity carries.
pub struct TagBitmapIndex {
    bitmaps: HashMap<String, RoaringBitmap>,
}

impl TagBitmapIndex {
    pub fn new() -> Self {
        Self {
            bitmaps: HashMap::new(),
        }
    }

    /// Add an entity's tags to the index.
    pub fn add(&mut self, entity_id: usize, tags: &[String]) {
        let eid = entity_id as u32;
        for tag in tags {
            self.bitmaps.entry(tag.clone()).or_default().insert(eid);
        }
    }

    /// Remove an entity from the given tag bitmaps.
    pub fn remove(&mut self, entity_id: usize, tags: &[String]) {
        let eid = entity_id as u32;
        for tag in tags {
            if let Some(bm) = self.bitmaps.get_mut(tag.as_str()) {
                bm.remove(eid);
            }
        }
    }

    /// Get the bitmap for a tag, if it exists.
    pub fn has_tag(&self, tag: &str) -> Option<&RoaringBitmap> {
        self.bitmaps.get(tag)
    }

    /// Bitwise AND of multiple bitmaps.
    pub fn intersect(bitmaps: &[&RoaringBitmap]) -> RoaringBitmap {
        if bitmaps.is_empty() {
            return RoaringBitmap::new();
        }
        let mut result = bitmaps[0].clone();
        for bm in &bitmaps[1..] {
            result &= *bm;
        }
        result
    }

    /// Bitwise OR of multiple bitmaps.
    pub fn union(bitmaps: &[&RoaringBitmap]) -> RoaringBitmap {
        let mut result = RoaringBitmap::new();
        for bm in bitmaps {
            result |= *bm;
        }
        result
    }

    /// Bitwise NOT of a bitmap, limited to max_id bits.
    pub fn negate(bitmap: &RoaringBitmap, max_id: usize) -> RoaringBitmap {
        if max_id == 0 {
            return RoaringBitmap::new();
        }
        let mut all = RoaringBitmap::from_iter(0..max_id as u32);
        all -= bitmap;
        all
    }

    /// Iterate over set bit positions.
    pub fn iter_set_bits(bitmap: &RoaringBitmap) -> impl Iterator<Item = usize> + '_ {
        bitmap.iter().map(|x| x as usize)
    }

    /// Count the number of set bits.
    pub fn count_ones(bitmap: &RoaringBitmap) -> usize {
        bitmap.len() as usize
    }
}

impl Default for TagBitmapIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_check_individual_bits() {
        let mut idx = TagBitmapIndex::new();
        idx.add(0, &["site".into()]);
        idx.add(5, &["site".into()]);
        idx.add(63, &["site".into()]);
        idx.add(64, &["site".into()]);

        let bm = idx.has_tag("site").unwrap();
        assert!(bm.contains(0));
        assert!(bm.contains(5));
        assert!(bm.contains(63));
        assert!(bm.contains(64));
    }

    #[test]
    fn add_remove_tag_tracking() {
        let mut idx = TagBitmapIndex::new();
        idx.add(3, &["equip".into(), "ahu".into()]);
        assert!(idx.has_tag("equip").is_some());
        assert!(idx.has_tag("ahu").is_some());

        idx.remove(3, &["equip".into(), "ahu".into()]);
        let bm = idx.has_tag("equip").unwrap();
        assert_eq!(TagBitmapIndex::count_ones(bm), 0);
    }

    #[test]
    fn intersect_multiple_bitmaps() {
        let mut idx = TagBitmapIndex::new();
        idx.add(1, &["site".into(), "equip".into()]);
        idx.add(2, &["site".into()]);
        idx.add(3, &["equip".into()]);

        let site_bm = idx.has_tag("site").unwrap();
        let equip_bm = idx.has_tag("equip").unwrap();
        let result = TagBitmapIndex::intersect(&[site_bm, equip_bm]);

        let bits: Vec<usize> = TagBitmapIndex::iter_set_bits(&result).collect();
        assert_eq!(bits, vec![1]);
    }

    #[test]
    fn union_multiple_bitmaps() {
        let mut idx = TagBitmapIndex::new();
        idx.add(1, &["site".into()]);
        idx.add(3, &["equip".into()]);

        let site_bm = idx.has_tag("site").unwrap();
        let equip_bm = idx.has_tag("equip").unwrap();
        let result = TagBitmapIndex::union(&[site_bm, equip_bm]);

        let bits: Vec<usize> = TagBitmapIndex::iter_set_bits(&result).collect();
        assert_eq!(bits, vec![1, 3]);
    }

    #[test]
    fn negate_bitmap() {
        let mut bm = RoaringBitmap::new();
        bm.insert(1);
        bm.insert(3);
        let negated = TagBitmapIndex::negate(&bm, 5);
        let bits: Vec<usize> = TagBitmapIndex::iter_set_bits(&negated).collect();
        assert_eq!(bits, vec![0, 2, 4]);
    }

    #[test]
    fn iterate_set_bits() {
        let mut bm = RoaringBitmap::new();
        bm.insert(0);
        bm.insert(2);
        bm.insert(4);
        bm.insert(128);
        let bits: Vec<usize> = TagBitmapIndex::iter_set_bits(&bm).collect();
        assert_eq!(bits, vec![0, 2, 4, 128]);
    }

    #[test]
    fn auto_grow_capacity() {
        let mut idx = TagBitmapIndex::new();
        idx.add(2000, &["far_away".into()]);
        let bm = idx.has_tag("far_away").unwrap();
        let bits: Vec<usize> = TagBitmapIndex::iter_set_bits(bm).collect();
        assert_eq!(bits, vec![2000]);
    }

    #[test]
    fn count_ones_popcount() {
        let mut bm = RoaringBitmap::new();
        for i in [0, 2, 3, 4, 64, 65] {
            bm.insert(i);
        }
        assert_eq!(TagBitmapIndex::count_ones(&bm), 6);
    }

    #[test]
    fn empty_bitmap_operations() {
        assert_eq!(TagBitmapIndex::intersect(&[]).len(), 0);
        assert_eq!(TagBitmapIndex::union(&[]).len(), 0);
        assert_eq!(TagBitmapIndex::negate(&RoaringBitmap::new(), 0).len(), 0);
        assert_eq!(TagBitmapIndex::count_ones(&RoaringBitmap::new()), 0);
    }

    #[test]
    fn unknown_tag_returns_none() {
        let idx = TagBitmapIndex::new();
        assert!(idx.has_tag("nonexistent").is_none());
    }

    #[test]
    fn remove_nonexistent_entity_is_noop() {
        let mut idx = TagBitmapIndex::new();
        idx.remove(999, &["site".into()]);
    }

    #[test]
    fn intersect_single_bitmap() {
        let mut bm = RoaringBitmap::new();
        bm.insert(0);
        bm.insert(1);
        bm.insert(2);
        let result = TagBitmapIndex::intersect(&[&bm]);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn negate_exact_word_boundary() {
        let bm = RoaringBitmap::from_iter(0..64u32);
        let negated = TagBitmapIndex::negate(&bm, 64);
        assert_eq!(TagBitmapIndex::count_ones(&negated), 0);
    }
}
