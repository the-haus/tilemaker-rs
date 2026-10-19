//! Tag filters in osmium `tags-filter` syntax.
//!
//! ```text
//! [TYPES/]KEY                 the key is present
//! [TYPES/]KEY=V1[,V2…]        the key has one of the values
//! [TYPES/]KEY!=V1[,V2…]       the key is present with none of the values
//! [TYPES/]!KEY                the key is absent
//! ```
//!
//! `TYPES` is any combination of `n` (nodes), `w` (ways), `r` (relations)
//! and `a` (areas: closed ways and multipolygon/boundary relations);
//! the default is `nwr`. Keys and values may contain `*` wildcards
//! (`name:*`, `*bahn*`). An element is selected when any filter matches.

use tilemaker_pbf::{ElementType, Tags};

/// Which element types a filter applies to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TypeMask {
    /// Nodes.
    pub nodes: bool,
    /// Ways.
    pub ways: bool,
    /// Relations.
    pub relations: bool,
    /// Areas only: closed ways and multipolygon/boundary relations.
    pub areas: bool,
}

impl TypeMask {
    /// All of nodes, ways and relations.
    pub const ALL: Self = Self {
        nodes: true,
        ways: true,
        relations: true,
        areas: false,
    };

    /// Whether an element of `kind` (closed / area-like or not) is covered.
    pub fn covers(&self, kind: ElementType, is_area: bool) -> bool {
        match kind {
            ElementType::Node => self.nodes,
            ElementType::Way => self.ways || (self.areas && is_area),
            ElementType::Relation => self.relations || (self.areas && is_area),
        }
    }
}

/// A `*` glob.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pattern {
    /// Exact text.
    Exact(String),
    /// Text with `*` wildcards, split at them.
    Glob(Vec<String>),
}

impl Pattern {
    /// Parse a pattern (`*` is a wildcard).
    pub fn new(s: &str) -> Self {
        if s.contains('*') {
            Pattern::Glob(s.split('*').map(str::to_string).collect())
        } else {
            Pattern::Exact(s.to_string())
        }
    }

    /// Whether `s` matches.
    pub fn matches(&self, s: &str) -> bool {
        match self {
            Pattern::Exact(e) => e == s,
            Pattern::Glob(parts) => {
                let (first, rest) = parts.split_first().unwrap();
                let Some(mut tail) = s.strip_prefix(first.as_str()) else {
                    return false;
                };
                let (last, middle) = rest.split_last().unwrap();
                for p in middle {
                    match tail.find(p.as_str()) {
                        Some(i) => tail = &tail[i + p.len()..],
                        None => return false,
                    }
                }
                tail.len() >= last.len() && tail.ends_with(last.as_str())
            }
        }
    }
}

/// What a filter tests about the key's value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Condition {
    /// The key is present.
    Present,
    /// The key is absent.
    Absent,
    /// The key has one of these values.
    OneOf(Vec<Pattern>),
    /// The key is present with none of these values.
    NoneOf(Vec<Pattern>),
}

/// One tag filter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagFilter {
    /// Element types it applies to.
    pub types: TypeMask,
    /// Key pattern.
    pub key: Pattern,
    /// Condition on the value.
    pub condition: Condition,
}

/// A filter expression that failed to parse.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid tag filter {expr:?}: {reason}")]
pub struct FilterParseError {
    /// The expression.
    pub expr: String,
    /// What is wrong.
    pub reason: &'static str,
}

impl TagFilter {
    /// Parse an osmium-style expression (see the module docs).
    pub fn parse(expr: &str) -> Result<Self, FilterParseError> {
        let err = |reason| FilterParseError {
            expr: expr.to_string(),
            reason,
        };
        let mut rest = expr.trim();
        let mut types = TypeMask::ALL;
        if let Some((prefix, after)) = rest.split_once('/') {
            if !prefix.is_empty() && prefix.chars().all(|c| matches!(c, 'n' | 'w' | 'r' | 'a')) {
                types = TypeMask {
                    nodes: prefix.contains('n'),
                    ways: prefix.contains('w'),
                    relations: prefix.contains('r'),
                    areas: prefix.contains('a'),
                };
                rest = after;
            }
        }
        if rest.is_empty() {
            return Err(err("missing key"));
        }
        let (key, condition) = if let Some(k) = rest.strip_prefix('!') {
            if k.contains('=') {
                return Err(err("'!key' cannot have a value"));
            }
            (k, Condition::Absent)
        } else if let Some((k, v)) = rest.split_once("!=") {
            (k, Condition::NoneOf(values(v)))
        } else if let Some((k, v)) = rest.split_once('=') {
            (k, Condition::OneOf(values(v)))
        } else {
            (rest, Condition::Present)
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(err("missing key"));
        }
        if let Condition::OneOf(v) | Condition::NoneOf(v) = &condition {
            if v.is_empty() {
                return Err(err("missing value after '='"));
            }
        }
        Ok(Self {
            types,
            key: Pattern::new(key),
            condition,
        })
    }

    /// Whether the tags satisfy the filter (ignoring element type).
    pub fn matches_tags<'a>(&self, tags: impl IntoIterator<Item = (&'a str, &'a str)>) -> bool {
        let mut found = false;
        for (k, v) in tags {
            if !self.key.matches(k) {
                continue;
            }
            found = true;
            match &self.condition {
                Condition::Present => return true,
                Condition::Absent => return false,
                Condition::OneOf(vals) => {
                    if vals.iter().any(|p| p.matches(v)) {
                        return true;
                    }
                }
                Condition::NoneOf(vals) => {
                    if !vals.iter().any(|p| p.matches(v)) {
                        return true;
                    }
                }
            }
        }
        matches!(self.condition, Condition::Absent) && !found
    }
}

fn values(v: &str) -> Vec<Pattern> {
    v.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(Pattern::new)
        .collect()
}

/// A set of filters; an element is selected when any applies and matches.
/// An empty set selects every element that has at least one tag.
#[derive(Clone, Debug, Default)]
pub struct FilterSet {
    filters: Vec<TagFilter>,
    /// Select untagged elements too (only when there are no filters).
    pub include_untagged: bool,
}

impl FilterSet {
    /// From parsed filters.
    pub fn new(filters: Vec<TagFilter>) -> Self {
        Self {
            filters,
            include_untagged: false,
        }
    }

    /// Parse several expressions.
    pub fn parse<S: AsRef<str>>(exprs: &[S]) -> Result<Self, FilterParseError> {
        Ok(Self::new(
            exprs
                .iter()
                .map(|e| TagFilter::parse(e.as_ref()))
                .collect::<Result<_, _>>()?,
        ))
    }

    /// The filters.
    pub fn filters(&self) -> &[TagFilter] {
        &self.filters
    }

    /// Whether no filters are set.
    pub fn is_empty(&self) -> bool {
        self.filters.is_empty()
    }

    /// Whether any filter could select elements of `kind` (used to skip
    /// whole passes).
    pub fn may_select(&self, kind: ElementType) -> bool {
        self.filters.is_empty()
            || self.filters.iter().any(|f| match kind {
                ElementType::Node => f.types.nodes,
                ElementType::Way => f.types.ways || f.types.areas,
                ElementType::Relation => f.types.relations || f.types.areas,
            })
    }

    /// Whether an element with these tags is selected.
    pub fn matches(&self, kind: ElementType, is_area: bool, tags: &Tags<'_>) -> bool {
        if self.filters.is_empty() {
            return self.include_untagged || !tags.is_empty();
        }
        self.filters
            .iter()
            .any(|f| f.types.covers(kind, is_area) && f.matches_tags(tags.iter()))
    }

    /// Like [`matches`](Self::matches) for owned tag pairs.
    pub fn matches_pairs(&self, kind: ElementType, is_area: bool, tags: &[(&str, &str)]) -> bool {
        if self.filters.is_empty() {
            return self.include_untagged || !tags.is_empty();
        }
        self.filters
            .iter()
            .any(|f| f.types.covers(kind, is_area) && f.matches_tags(tags.iter().copied()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(expr: &str, kind: ElementType, area: bool, tags: &[(&str, &str)]) -> bool {
        let f = TagFilter::parse(expr).unwrap();
        f.types.covers(kind, area) && f.matches_tags(tags.iter().copied())
    }

    #[test]
    fn parses_osmium_examples() {
        let f = TagFilter::parse("n/place=city,town").unwrap();
        assert_eq!(
            f.types,
            TypeMask {
                nodes: true,
                ways: false,
                relations: false,
                areas: false
            }
        );
        assert_eq!(f.key, Pattern::Exact("place".into()));
        assert_eq!(
            f.condition,
            Condition::OneOf(vec![Pattern::Exact("city".into()), Pattern::Exact("town".into())])
        );
        let f = TagFilter::parse("r/admin_level=2,4").unwrap();
        assert!(f.types.relations && !f.types.nodes);
        let f = TagFilter::parse("highway").unwrap();
        assert_eq!(f.types, TypeMask::ALL);
        assert_eq!(f.condition, Condition::Present);
        assert!(TagFilter::parse("w/").is_err());
        assert!(TagFilter::parse("a=").is_err());
        assert!(TagFilter::parse("!a=b").is_err());
        // A slash inside a key is not a type prefix.
        let f = TagFilter::parse("disused/amenity").unwrap();
        assert_eq!(f.key, Pattern::Exact("disused/amenity".into()));
    }

    #[test]
    fn matching() {
        use ElementType::*;
        assert!(m("n/place=city,town", Node, false, &[("place", "town")]));
        assert!(!m("n/place=city,town", Node, false, &[("place", "village")]));
        assert!(!m("n/place=city,town", Way, false, &[("place", "city")]));
        assert!(m("highway!=footway", Way, false, &[("highway", "primary")]));
        assert!(!m("highway!=footway", Way, false, &[("highway", "footway")]));
        assert!(!m("highway!=footway", Way, false, &[("name", "x")]));
        assert!(m("!building", Way, false, &[("name", "x")]));
        assert!(!m("!building", Way, false, &[("building", "yes")]));
        assert!(m("name:*", Node, false, &[("name:de", "x")]));
        assert!(!m("name:*", Node, false, &[("name", "x")]));
        assert!(m("name=*bahn*", Node, false, &[("name", "Autobahnkreuz")]));
        assert!(m("a/landuse", Way, true, &[("landuse", "forest")]));
        assert!(!m("a/landuse", Way, false, &[("landuse", "forest")]));
        assert!(m("a/natural=water", Relation, true, &[("natural", "water")]));
    }

    #[test]
    fn globs() {
        let p = Pattern::new("a*b*c");
        assert!(p.matches("abc"));
        assert!(p.matches("axxbyyc"));
        assert!(!p.matches("axxbyy"));
        assert!(!p.matches("ac"));
        assert!(Pattern::new("*").matches(""));
        assert!(Pattern::new("ab*").matches("ab"));
        assert!(!Pattern::new("*ab").matches("b"));
        assert!(!Pattern::new("a*a").matches("a"));
    }

    #[test]
    fn set_semantics() {
        let s = FilterSet::parse(&["n/amenity=cafe", "w/highway"]).unwrap();
        assert!(s.may_select(ElementType::Node));
        assert!(s.may_select(ElementType::Way));
        assert!(!s.may_select(ElementType::Relation));
        assert!(s.matches_pairs(ElementType::Way, false, &[("highway", "x")]));
        assert!(!s.matches_pairs(ElementType::Node, false, &[("highway", "x")]));
        let empty = FilterSet::default();
        assert!(empty.matches_pairs(ElementType::Node, false, &[("a", "b")]));
        assert!(!empty.matches_pairs(ElementType::Node, false, &[]));
    }
}
