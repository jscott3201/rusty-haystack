// Query planner — bitmap-accelerated filter evaluation.
//
// Two-phase approach:
// 1. **Bitmap phase**: For Has/Missing nodes, combine tag bitmaps with
//    set operations (AND/OR/NOT) to produce a candidate bitset.
// 2. **Scan phase**: Iterate set bits from candidates, evaluate the
//    full filter AST on each candidate entity.
//
// Returns `None` when bitmap optimization is not applicable (e.g.
// comparison nodes), causing the caller to fall back to a full scan.

use crate::filter::{CmpOp, FilterNode};
use crate::kinds::Kind;

use super::adjacency::RefAdjacency;
use super::bitmap::TagBitmapIndex;
use super::value_index::ValueIndex;

/// Helper: convert a list of entity IDs into a bitmap.
fn ids_to_bitmap(entity_ids: &[usize], max_id: usize) -> Vec<u64> {
    if max_id == 0 {
        return Vec::new();
    }
    let mut bitmap = vec![0u64; max_id.div_ceil(64)];
    for &eid in entity_ids {
        if eid < max_id {
            bitmap[eid / 64] |= 1u64 << (eid % 64);
        }
    }
    bitmap
}

/// Compute a bitmap of candidate entity ids that *may* match the given
/// filter node, using tag presence bitmaps only.
///
/// Returns `None` if the filter contains terms that cannot be resolved
/// via bitmap (e.g. comparison operators), meaning a full scan is needed.
pub fn bitmap_candidates(
    node: &FilterNode,
    tag_index: &TagBitmapIndex,
    max_id: usize,
) -> Option<Vec<u64>> {
    match node {
        FilterNode::Has(path) => {
            if !path.is_single() {
                return None; // multi-segment paths need entity resolution
            }
            let bm = tag_index.has_tag(path.first())?;
            Some(bm.clone())
        }

        FilterNode::Missing(path) => {
            if !path.is_single() {
                return None;
            }
            match tag_index.has_tag(path.first()) {
                Some(bm) => Some(TagBitmapIndex::negate(bm, max_id)),
                None => {
                    // Tag never seen: all entities match "missing".
                    if max_id == 0 {
                        Some(Vec::new())
                    } else {
                        Some(TagBitmapIndex::negate(&[], max_id))
                    }
                }
            }
        }

        FilterNode::And(left, right) => {
            let l = bitmap_candidates(left, tag_index, max_id);
            let r = bitmap_candidates(right, tag_index, max_id);
            match (l, r) {
                (Some(lb), Some(rb)) => {
                    // Selectivity optimization: order by popcount.
                    let lc = TagBitmapIndex::count_ones(&lb);
                    let rc = TagBitmapIndex::count_ones(&rb);
                    if lc == 0 || rc == 0 {
                        // Short-circuit: one side is empty.
                        return Some(Vec::new());
                    }
                    if lc <= rc {
                        Some(TagBitmapIndex::intersect(&[&lb, &rb]))
                    } else {
                        Some(TagBitmapIndex::intersect(&[&rb, &lb]))
                    }
                }
                // If only one side is optimizable, use it as the candidate
                // set; the other side will be checked during the scan phase.
                (Some(bm), None) | (None, Some(bm)) => Some(bm),
                (None, None) => None,
            }
        }

        FilterNode::Or(left, right) => {
            let l = bitmap_candidates(left, tag_index, max_id);
            let r = bitmap_candidates(right, tag_index, max_id);
            match (l, r) {
                (Some(lb), Some(rb)) => Some(TagBitmapIndex::union(&[&lb, &rb])),
                // If either side is not optimizable, we cannot narrow.
                _ => None,
            }
        }

        FilterNode::Cmp { path, .. } => {
            // Comparison nodes cannot be resolved via bitmap, but we can
            // still use the tag existence bitmap to prune candidates.
            if path.is_single() {
                tag_index.has_tag(path.first()).cloned()
            } else {
                None
            }
        }

        FilterNode::SpecMatch(_) => None,
    }
}

/// Enhanced candidate computation that uses B-Tree value indexes and
/// ref adjacency for Cmp nodes in addition to tag bitmap indexes.
///
/// When a `Cmp` node references a single-segment path with an available value
/// index, this produces a precise candidate set (only entities whose value
/// satisfies the comparison) rather than just tag-existence candidates.
///
/// For Ref equality (`siteRef == @site-0`), uses the adjacency reverse map
/// to produce an exact bitmap without scanning entities.
///
/// Falls back to `bitmap_candidates` behavior for non-indexed fields.
pub fn bitmap_candidates_with_values(
    node: &FilterNode,
    tag_index: &TagBitmapIndex,
    value_index: &ValueIndex,
    adjacency: &RefAdjacency,
    max_id: usize,
) -> Option<Vec<u64>> {
    match node {
        // Ref equality: use adjacency reverse map for O(1) lookup.
        FilterNode::Cmp {
            path,
            op: CmpOp::Eq,
            val: Kind::Ref(r),
        } if path.is_single() => {
            let field = path.first();
            let entity_ids = adjacency.sources_for(field, &r.val);
            Some(ids_to_bitmap(&entity_ids, max_id))
        }

        FilterNode::Cmp { path, op, val }
            if path.is_single() && value_index.has_index(path.first()) =>
        {
            let field = path.first();
            let entity_ids = match op {
                CmpOp::Eq => value_index.eq_lookup(field, val),
                CmpOp::Ne => value_index.ne_lookup(field, val),
                CmpOp::Lt => value_index.lt_lookup(field, val),
                CmpOp::Le => value_index.le_lookup(field, val),
                CmpOp::Gt => value_index.gt_lookup(field, val),
                CmpOp::Ge => value_index.ge_lookup(field, val),
            };

            Some(ids_to_bitmap(&entity_ids, max_id))
        }

        FilterNode::And(left, right) => {
            let l = bitmap_candidates_with_values(left, tag_index, value_index, adjacency, max_id);
            let r = bitmap_candidates_with_values(right, tag_index, value_index, adjacency, max_id);
            match (l, r) {
                (Some(lb), Some(rb)) => {
                    let lc = TagBitmapIndex::count_ones(&lb);
                    let rc = TagBitmapIndex::count_ones(&rb);
                    if lc == 0 || rc == 0 {
                        return Some(Vec::new());
                    }
                    if lc <= rc {
                        Some(TagBitmapIndex::intersect(&[&lb, &rb]))
                    } else {
                        Some(TagBitmapIndex::intersect(&[&rb, &lb]))
                    }
                }
                (Some(bm), None) | (None, Some(bm)) => Some(bm),
                (None, None) => None,
            }
        }

        FilterNode::Or(left, right) => {
            let l = bitmap_candidates_with_values(left, tag_index, value_index, adjacency, max_id);
            let r = bitmap_candidates_with_values(right, tag_index, value_index, adjacency, max_id);
            match (l, r) {
                (Some(lb), Some(rb)) => Some(TagBitmapIndex::union(&[&lb, &rb])),
                _ => None,
            }
        }

        // Delegate to base bitmap_candidates for all other node types.
        _ => bitmap_candidates(node, tag_index, max_id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::parse_filter;

    fn build_test_index() -> (TagBitmapIndex, usize) {
        let mut idx = TagBitmapIndex::new();
        // entity 0: site
        idx.add(0, &["site".into(), "dis".into()]);
        // entity 1: equip
        idx.add(1, &["equip".into(), "dis".into()]);
        // entity 2: point, sensor
        idx.add(2, &["point".into(), "sensor".into(), "dis".into()]);
        // entity 3: site, geoCity
        idx.add(3, &["site".into(), "geoCity".into(), "dis".into()]);
        (idx, 4) // max_id = 4
    }

    #[test]
    fn bitmap_candidates_for_has() {
        let (idx, max_id) = build_test_index();
        let ast = parse_filter("site").unwrap();
        let bm = bitmap_candidates(&ast, &idx, max_id).unwrap();
        let bits: Vec<usize> = TagBitmapIndex::iter_set_bits(&bm).collect();
        assert_eq!(bits, vec![0, 3]);
    }

    #[test]
    fn bitmap_candidates_for_missing() {
        let (idx, max_id) = build_test_index();
        let ast = parse_filter("not site").unwrap();
        let bm = bitmap_candidates(&ast, &idx, max_id).unwrap();
        let bits: Vec<usize> = TagBitmapIndex::iter_set_bits(&bm).collect();
        assert_eq!(bits, vec![1, 2]);
    }

    #[test]
    fn bitmap_candidates_for_and() {
        let (idx, max_id) = build_test_index();
        let ast = parse_filter("site and geoCity").unwrap();
        let bm = bitmap_candidates(&ast, &idx, max_id).unwrap();
        let bits: Vec<usize> = TagBitmapIndex::iter_set_bits(&bm).collect();
        assert_eq!(bits, vec![3]);
    }

    #[test]
    fn bitmap_candidates_for_or() {
        let (idx, max_id) = build_test_index();
        let ast = parse_filter("site or equip").unwrap();
        let bm = bitmap_candidates(&ast, &idx, max_id).unwrap();
        let bits: Vec<usize> = TagBitmapIndex::iter_set_bits(&bm).collect();
        assert_eq!(bits, vec![0, 1, 3]);
    }

    #[test]
    fn bitmap_candidates_for_comparison_uses_tag_existence() {
        let (idx, max_id) = build_test_index();
        let ast = parse_filter("geoCity == \"Richmond\"").unwrap();
        let bm = bitmap_candidates(&ast, &idx, max_id);
        // Comparison on a single-segment path uses tag existence bitmap.
        assert!(bm.is_some());
        let bits: Vec<usize> = TagBitmapIndex::iter_set_bits(&bm.unwrap()).collect();
        assert_eq!(bits, vec![3]);
    }

    #[test]
    fn bitmap_candidates_returns_none_for_multi_segment_path() {
        let (idx, max_id) = build_test_index();
        let ast = parse_filter("siteRef->area").unwrap();
        let bm = bitmap_candidates(&ast, &idx, max_id);
        assert!(bm.is_none());
    }

    #[test]
    fn bitmap_candidates_returns_none_for_spec_match() {
        let (idx, max_id) = build_test_index();
        let ast = parse_filter("ph::Site").unwrap();
        let bm = bitmap_candidates(&ast, &idx, max_id);
        assert!(bm.is_none());
    }

    #[test]
    fn and_with_one_side_optimizable() {
        let (idx, max_id) = build_test_index();
        // "site" is optimizable, "dis == \"hello\"" is a comparison.
        let ast = parse_filter("site and dis == \"hello\"").unwrap();
        let bm = bitmap_candidates(&ast, &idx, max_id);
        // Should still produce a bitmap (intersection of site and dis existence).
        assert!(bm.is_some());
    }

    #[test]
    fn or_with_one_side_not_optimizable() {
        let (idx, max_id) = build_test_index();
        // "site" optimizable, "siteRef->area" is not.
        let ast = parse_filter("site or siteRef->area").unwrap();
        let bm = bitmap_candidates(&ast, &idx, max_id);
        // Cannot optimize OR if one side is unknown.
        assert!(bm.is_none());
    }

    #[test]
    fn empty_index() {
        let idx = TagBitmapIndex::new();
        let ast = parse_filter("site").unwrap();
        let bm = bitmap_candidates(&ast, &idx, 0);
        // No bitmap for unknown tag.
        assert!(bm.is_none());
    }

    #[test]
    fn missing_unknown_tag_returns_all() {
        let (idx, max_id) = build_test_index();
        let ast = parse_filter("not unknownTag").unwrap();
        let bm = bitmap_candidates(&ast, &idx, max_id).unwrap();
        let bits: Vec<usize> = TagBitmapIndex::iter_set_bits(&bm).collect();
        // All 4 entities should match.
        assert_eq!(bits, vec![0, 1, 2, 3]);
    }
}
