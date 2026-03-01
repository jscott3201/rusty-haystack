// Tag bitmap index — Vec<u64> bitsets for fast tag-presence queries.

use std::collections::HashMap;

/// Initial capacity in u64 words (covers 1024 entities).
const INITIAL_WORDS: usize = 16;

/// Tag-presence bitmap index.
///
/// Maps tag names to bitsets stored as `Vec<u64>`. Each entity is assigned
/// a numeric id; the bit at position `id` is set in every bitmap for every
/// tag the entity carries.
pub struct TagBitmapIndex {
    /// tag_name -> bitmap (Vec<u64>)
    bitmaps: HashMap<String, Vec<u64>>,
    /// Number of u64 words currently allocated per bitmap.
    capacity: usize,
}

impl TagBitmapIndex {
    /// Create a new empty index.
    pub fn new() -> Self {
        Self {
            bitmaps: HashMap::new(),
            capacity: INITIAL_WORDS,
        }
    }

    /// Add an entity's tags to the index.
    ///
    /// Sets the bit at `entity_id` in each tag's bitmap.
    pub fn add(&mut self, entity_id: usize, tags: &[String]) {
        self.ensure_capacity(entity_id);
        let word_idx = entity_id / 64;
        let bit = 1u64 << (entity_id % 64);

        for tag in tags {
            let bm = self
                .bitmaps
                .entry(tag.clone())
                .or_insert_with(|| vec![0u64; self.capacity]);
            // Grow this bitmap if it was created before a capacity increase.
            if bm.len() < self.capacity {
                bm.resize(self.capacity, 0);
            }
            bm[word_idx] |= bit;
        }
    }

    /// Remove an entity from the given tag bitmaps.
    ///
    /// Clears the bit at `entity_id` in each tag's bitmap.
    pub fn remove(&mut self, entity_id: usize, tags: &[String]) {
        let word_idx = entity_id / 64;
        let bit = 1u64 << (entity_id % 64);

        for tag in tags {
            if let Some(bm) = self.bitmaps.get_mut(tag.as_str()) {
                if word_idx < bm.len() {
                    bm[word_idx] &= !bit;
                }
            }
        }
    }

    /// Get the bitmap for a tag, if it exists.
    pub fn has_tag(&self, tag: &str) -> Option<&Vec<u64>> {
        self.bitmaps.get(tag)
    }

    /// Bitwise AND of multiple bitmaps.
    ///
    /// Returns an empty bitmap if `bitmaps` is empty.
    pub fn intersect(bitmaps: &[&Vec<u64>]) -> Vec<u64> {
        if bitmaps.is_empty() {
            return Vec::new();
        }
        let len = bitmaps.iter().map(|b| b.len()).min().unwrap_or(0);
        let mut result = bitmaps[0][..len].to_vec();
        for bm in &bitmaps[1..] {
            for (i, word) in result.iter_mut().enumerate() {
                *word &= bm.get(i).copied().unwrap_or(0);
            }
        }
        result
    }

    /// Bitwise OR of multiple bitmaps.
    ///
    /// Returns an empty bitmap if `bitmaps` is empty.
    pub fn union(bitmaps: &[&Vec<u64>]) -> Vec<u64> {
        if bitmaps.is_empty() {
            return Vec::new();
        }
        let len = bitmaps.iter().map(|b| b.len()).max().unwrap_or(0);
        let mut result = vec![0u64; len];
        for bm in bitmaps {
            for (i, &word) in bm.iter().enumerate() {
                result[i] |= word;
            }
        }
        result
    }

    /// Bitwise NOT of a bitmap, limited to `max_id` bits.
    pub fn negate(bitmap: &[u64], max_id: usize) -> Vec<u64> {
        if max_id == 0 {
            return Vec::new();
        }
        let total_words = max_id.div_ceil(64);
        let mut result = vec![0u64; total_words];
        for (i, word) in result.iter_mut().enumerate() {
            let src = bitmap.get(i).copied().unwrap_or(0);
            *word = !src;
        }
        // Mask off bits beyond max_id in the last word.
        let tail_bits = max_id % 64;
        if tail_bits != 0 && !result.is_empty() {
            let last = result.len() - 1;
            result[last] &= (1u64 << tail_bits) - 1;
        }
        result
    }

    /// Iterate over positions of set bits in a bitmap.
    pub fn iter_set_bits(bitmap: &[u64]) -> impl Iterator<Item = usize> + '_ {
        bitmap
            .iter()
            .enumerate()
            .flat_map(|(word_idx, &word)| SetBitIter {
                word,
                base: word_idx * 64,
            })
    }

    /// Count the number of set bits (population count).
    pub fn count_ones(bitmap: &[u64]) -> usize {
        bitmap.iter().map(|w| w.count_ones() as usize).sum()
    }

    /// Ensure capacity covers `entity_id`.
    fn ensure_capacity(&mut self, entity_id: usize) {
        let needed = entity_id / 64 + 1;
        if needed > self.capacity {
            // Double until sufficient.
            let mut new_cap = self.capacity;
            while new_cap < needed {
                new_cap *= 2;
            }
            self.capacity = new_cap;
            // Grow all existing bitmaps.
            for bm in self.bitmaps.values_mut() {
                bm.resize(self.capacity, 0);
            }
        }
    }
}

impl Default for TagBitmapIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// Iterator that yields set bit positions from a single u64 word.
struct SetBitIter {
    word: u64,
    base: usize,
}

impl Iterator for SetBitIter {
    type Item = usize;

    #[inline]
    fn next(&mut self) -> Option<usize> {
        if self.word == 0 {
            return None;
        }
        let tz = self.word.trailing_zeros() as usize;
        // Clear the lowest set bit.
        self.word &= self.word - 1;
        Some(self.base + tz)
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
        assert_eq!(bm[0] & 1, 1); // bit 0
        assert_ne!(bm[0] & (1u64 << 5), 0); // bit 5
        assert_ne!(bm[0] & (1u64 << 63), 0); // bit 63
        assert_ne!(bm[1] & 1, 0); // bit 64
    }

    #[test]
    fn add_remove_tag_tracking() {
        let mut idx = TagBitmapIndex::new();
        idx.add(3, &["equip".into(), "ahu".into()]);
        assert!(idx.has_tag("equip").is_some());
        assert!(idx.has_tag("ahu").is_some());

        idx.remove(3, &["equip".into(), "ahu".into()]);
        // Bitmaps still exist but bit is cleared.
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
        assert_eq!(bits, vec![1]); // only entity 1 has both
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
        let bitmap = vec![0b1010u64]; // bits 1, 3 set
        let negated = TagBitmapIndex::negate(&bitmap, 5);
        // Bits 0, 2, 4 should be set (within max_id=5)
        let bits: Vec<usize> = TagBitmapIndex::iter_set_bits(&negated).collect();
        assert_eq!(bits, vec![0, 2, 4]);
    }

    #[test]
    fn iterate_set_bits() {
        let bitmap = vec![0b10101u64, 0u64, 1u64]; // bits 0,2,4 in word 0; bit 128 in word 2
        let bits: Vec<usize> = TagBitmapIndex::iter_set_bits(&bitmap).collect();
        assert_eq!(bits, vec![0, 2, 4, 128]);
    }

    #[test]
    fn auto_grow_capacity() {
        let mut idx = TagBitmapIndex::new();
        // Initial capacity is 16 words = 1024 bits.
        // Add entity at id 2000 should trigger growth.
        idx.add(2000, &["far_away".into()]);
        let bm = idx.has_tag("far_away").unwrap();
        let bits: Vec<usize> = TagBitmapIndex::iter_set_bits(bm).collect();
        assert_eq!(bits, vec![2000]);
    }

    #[test]
    fn count_ones_popcount() {
        let bitmap = vec![0b11101u64, 0b11u64]; // 4 + 2 = 6
        assert_eq!(TagBitmapIndex::count_ones(&bitmap), 6);
    }

    #[test]
    fn empty_bitmap_operations() {
        assert_eq!(TagBitmapIndex::intersect(&[]), Vec::<u64>::new());
        assert_eq!(TagBitmapIndex::union(&[]), Vec::<u64>::new());
        assert_eq!(TagBitmapIndex::negate(&[], 0), Vec::<u64>::new());
        assert_eq!(TagBitmapIndex::count_ones(&[]), 0);
    }

    #[test]
    fn unknown_tag_returns_none() {
        let idx = TagBitmapIndex::new();
        assert!(idx.has_tag("nonexistent").is_none());
    }

    #[test]
    fn remove_nonexistent_entity_is_noop() {
        let mut idx = TagBitmapIndex::new();
        // Should not panic.
        idx.remove(999, &["site".into()]);
    }

    #[test]
    fn intersect_single_bitmap() {
        let bm = vec![0b111u64];
        let result = TagBitmapIndex::intersect(&[&bm]);
        assert_eq!(result, vec![0b111u64]);
    }

    #[test]
    fn negate_exact_word_boundary() {
        // max_id = 64 means exactly 1 word, all bits valid.
        let bitmap = vec![u64::MAX];
        let negated = TagBitmapIndex::negate(&bitmap, 64);
        assert_eq!(TagBitmapIndex::count_ones(&negated), 0);
    }
}
