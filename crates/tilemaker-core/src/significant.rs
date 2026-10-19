//! `node_keys` / `way_keys` (port of `SignificantTags`): `key`,
//! `key=value`, or all `~`-prefixed to list keys that do *not* count.

use tilemaker_pbf::Tags;

#[derive(Clone, Debug)]
struct Filter {
    key: String,
    value: Option<String>,
}

/// Which objects the profile wants to see.
#[derive(Clone, Debug, Default)]
pub struct SignificantTags {
    enabled: bool,
    accept: bool,
    filters: Vec<Filter>,
}

impl SignificantTags {
    /// Everything passes.
    pub fn all() -> Self {
        Self::default()
    }

    /// From the profile's list.
    pub fn new(raw: &[String]) -> Result<Self, String> {
        let mut filters = Vec::new();
        let mut accept = None;
        for r in raw {
            let (acc, rest) = match r.strip_prefix('~') {
                Some(rest) => (false, rest),
                None => (true, r.as_str()),
            };
            if *accept.get_or_insert(acc) != acc {
                return Err(format!("cannot mix reject and accept filters: {}, {r}", raw[0]));
            }
            let (key, value) = match rest.split_once('=') {
                Some((k, v)) => (k.to_string(), Some(v.to_string())),
                None => (rest.to_string(), None),
            };
            filters.push(Filter { key, value });
        }
        Ok(Self {
            enabled: true,
            accept: accept.unwrap_or(true),
            filters,
        })
    }

    /// Whether filtering is on.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    fn matches(&self, k: &str, v: &str) -> bool {
        self.filters
            .iter()
            .any(|f| f.key == k && f.value.as_deref().is_none_or(|fv| fv == v))
    }

    /// Whether an object with these tags is significant.
    pub fn filter(&self, tags: &Tags<'_>) -> bool {
        self.filter_iter(tags.iter())
    }

    /// Like [`filter`](Self::filter) for owned pairs.
    pub fn filter_pairs(&self, tags: &[(&str, &str)]) -> bool {
        self.filter_iter(tags.iter().copied())
    }

    fn filter_iter<'a>(&self, mut tags: impl Iterator<Item = (&'a str, &'a str)>) -> bool {
        if !self.enabled {
            return true;
        }
        if self.filters.is_empty() {
            return false;
        }
        if self.accept {
            tags.any(|(k, v)| self.matches(k, v))
        } else {
            tags.any(|(k, v)| !self.matches(k, v))
        }
    }
}
