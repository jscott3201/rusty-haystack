/// Result of a peek evaluation — check a small sample before full scan.
#[derive(Debug)]
pub struct PeekResult<T> {
    /// Items found in the peek sample.
    pub items: Vec<T>,
    /// Whether the limit was satisfied by the peek alone.
    pub satisfied: bool,
    /// Number of items sampled.
    pub sampled: usize,
}

/// Peek at the first N items from an iterator, checking if a limit is satisfied.
pub fn peek_eval<T, I>(iter: I, peek_size: usize, limit: Option<usize>) -> PeekResult<T>
where
    I: Iterator<Item = T>,
{
    let mut items = Vec::new();
    let mut count = 0;

    for item in iter {
        count += 1;
        items.push(item);

        if let Some(lim) = limit
            && items.len() >= lim
        {
            return PeekResult {
                items,
                satisfied: true,
                sampled: count,
            };
        }

        if count >= peek_size {
            break;
        }
    }

    PeekResult {
        satisfied: limit.is_some_and(|lim| items.len() >= lim),
        sampled: count,
        items,
    }
}

/// Lazy evaluation wrapper that avoids full collection when limit is known.
pub fn lazy_collect<T, I>(iter: I, limit: Option<usize>) -> Vec<T>
where
    I: Iterator<Item = T>,
{
    match limit {
        Some(lim) => iter.take(lim).collect(),
        None => iter.collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peek_limit_satisfied() {
        let data = vec![1, 2, 3, 4, 5];
        let result = peek_eval(data.into_iter(), 10, Some(3));
        assert!(result.satisfied);
        assert_eq!(result.items.len(), 3);
        assert_eq!(result.sampled, 3);
    }

    #[test]
    fn peek_limit_not_satisfied() {
        let data = vec![1, 2];
        let result = peek_eval(data.into_iter(), 10, Some(5));
        assert!(!result.satisfied);
        assert_eq!(result.items.len(), 2);
        assert_eq!(result.sampled, 2);
    }

    #[test]
    fn peek_no_limit_stops_at_peek_size() {
        let data = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let result = peek_eval(data.into_iter(), 3, None);
        assert!(!result.satisfied);
        assert_eq!(result.items.len(), 3);
        assert_eq!(result.sampled, 3);
    }

    #[test]
    fn peek_empty_iterator() {
        let data: Vec<i32> = vec![];
        let result = peek_eval(data.into_iter(), 10, Some(5));
        assert!(!result.satisfied);
        assert!(result.items.is_empty());
        assert_eq!(result.sampled, 0);
    }

    #[test]
    fn lazy_collect_with_limit() {
        let data = vec![1, 2, 3, 4, 5];
        let result = lazy_collect(data.into_iter(), Some(3));
        assert_eq!(result, vec![1, 2, 3]);
    }

    #[test]
    fn lazy_collect_without_limit() {
        let data = vec![1, 2, 3, 4, 5];
        let result = lazy_collect(data.into_iter(), None);
        assert_eq!(result, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn lazy_collect_limit_exceeds_items() {
        let data = vec![1, 2];
        let result = lazy_collect(data.into_iter(), Some(10));
        assert_eq!(result, vec![1, 2]);
    }
}
