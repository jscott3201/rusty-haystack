// Haystack List — an ordered list of Kind values.

use crate::kinds::Kind;
use std::fmt;

/// Haystack List — a thin wrapper around `Vec<Kind>`.
///
/// Used for tag values that are lists of Haystack values.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HList(pub Vec<Kind>);

impl HList {
    /// Create an empty list.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a list from a pre-built Vec.
    pub fn from_vec(v: Vec<Kind>) -> Self {
        Self(v)
    }

    /// Returns the number of elements.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns `true` if the list is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns a reference to the element at `index`, if in bounds.
    pub fn get(&self, index: usize) -> Option<&Kind> {
        self.0.get(index)
    }

    /// Append a value to the end of the list.
    pub fn push(&mut self, val: Kind) {
        self.0.push(val);
    }

    /// Iterate over the values.
    pub fn iter(&self) -> impl Iterator<Item = &Kind> {
        self.0.iter()
    }
}

impl fmt::Display for HList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[")?;
        for (i, item) in self.0.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{item}")?;
        }
        write!(f, "]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kinds::Number;

    #[test]
    fn empty_list() {
        let l = HList::new();
        assert!(l.is_empty());
        assert_eq!(l.len(), 0);
        assert_eq!(l.get(0), None);
    }

    #[test]
    fn from_vec() {
        let l = HList::from_vec(vec![Kind::Marker, Kind::Str("hello".into())]);
        assert_eq!(l.len(), 2);
        assert!(!l.is_empty());
    }

    #[test]
    fn push_and_get() {
        let mut l = HList::new();
        l.push(Kind::Number(Number::unitless(1.0)));
        l.push(Kind::Str("two".into()));
        l.push(Kind::Marker);

        assert_eq!(l.len(), 3);
        assert_eq!(l.get(0), Some(&Kind::Number(Number::unitless(1.0))));
        assert_eq!(l.get(1), Some(&Kind::Str("two".into())));
        assert_eq!(l.get(2), Some(&Kind::Marker));
        assert_eq!(l.get(3), None);
    }

    #[test]
    fn iteration() {
        let l = HList::from_vec(vec![
            Kind::Number(Number::unitless(1.0)),
            Kind::Number(Number::unitless(2.0)),
            Kind::Number(Number::unitless(3.0)),
        ]);

        let collected: Vec<&Kind> = l.iter().collect();
        assert_eq!(collected.len(), 3);
    }

    #[test]
    fn equality() {
        let a = HList::from_vec(vec![Kind::Marker, Kind::Str("x".into())]);
        let b = HList::from_vec(vec![Kind::Marker, Kind::Str("x".into())]);
        assert_eq!(a, b);
    }

    #[test]
    fn inequality() {
        let a = HList::from_vec(vec![Kind::Marker]);
        let b = HList::from_vec(vec![Kind::NA]);
        assert_ne!(a, b);
    }

    #[test]
    fn display_empty() {
        let l = HList::new();
        assert_eq!(l.to_string(), "[]");
    }

    #[test]
    fn display_with_items() {
        let l = HList::from_vec(vec![
            Kind::Number(Number::unitless(1.0)),
            Kind::Str("hello".into()),
        ]);
        assert_eq!(l.to_string(), "[1, hello]");
    }

    #[test]
    fn default_is_empty() {
        let l = HList::default();
        assert!(l.is_empty());
    }
}
