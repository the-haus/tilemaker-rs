//! Relations accepted in `relation_scan_function`: their tags and which
//! nodes, ways and relations belong to them (port of `RelationScanStore`).

use std::collections::HashMap;

/// Membership lists keyed by member id.
#[derive(Clone, Debug, Default)]
pub struct ScannedRelations {
    tags: HashMap<i64, Vec<(String, String)>>,
    roles: Vec<String>,
    role_index: HashMap<String, u16>,
    ways: HashMap<i64, Vec<(i64, u16)>>,
    nodes: HashMap<i64, Vec<(i64, u16)>>,
    relations: HashMap<i64, Vec<(i64, u16)>>,
}

/// Member type for [`ScannedRelations::add_member`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberKind {
    /// Node.
    Node,
    /// Way.
    Way,
    /// Relation.
    Relation,
}

impl ScannedRelations {
    /// Empty.
    pub fn new() -> Self {
        Self::default()
    }

    fn role(&mut self, role: &str) -> u16 {
        if let Some(&r) = self.role_index.get(role) {
            return r;
        }
        let r = self.roles.len() as u16;
        self.roles.push(role.to_string());
        self.role_index.insert(role.to_string(), r);
        r
    }

    /// Record an accepted relation's tags.
    pub fn store_tags(&mut self, relation: i64, tags: Vec<(String, String)>) {
        self.tags.insert(relation, tags);
    }

    /// Record a member of an accepted relation.
    pub fn add_member(&mut self, relation: i64, kind: MemberKind, member: i64, role: &str) {
        let r = self.role(role);
        let map = match kind {
            MemberKind::Node => &mut self.nodes,
            MemberKind::Way => &mut self.ways,
            MemberKind::Relation => &mut self.relations,
        };
        map.entry(member).or_default().push((relation, r));
    }

    /// Merge another store (from another thread).
    pub fn merge(&mut self, other: ScannedRelations) {
        for (id, t) in other.tags {
            self.tags.insert(id, t);
        }
        let remap: Vec<u16> = other.roles.iter().map(|r| self.role(r)).collect();
        for (src, dst) in [
            (other.ways, &mut self.ways),
            (other.nodes, &mut self.nodes),
            (other.relations, &mut self.relations),
        ] {
            for (member, list) in src {
                dst.entry(member)
                    .or_default()
                    .extend(list.into_iter().map(|(rel, role)| (rel, remap[role as usize])));
            }
        }
    }

    /// Whether nothing was accepted.
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty()
    }

    /// Relations containing a member, as `(relation id, role)`.
    pub fn relations_for(&self, kind: MemberKind, member: i64) -> Vec<(i64, String)> {
        let map = match kind {
            MemberKind::Node => &self.nodes,
            MemberKind::Way => &self.ways,
            MemberKind::Relation => &self.relations,
        };
        map.get(&member)
            .map(|l| {
                l.iter()
                    .map(|&(rel, r)| (rel, self.roles[r as usize].clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Relations that are members of other accepted relations.
    pub fn relations_in_relations(&self) -> Vec<i64> {
        let mut v: Vec<i64> = self
            .relations
            .keys()
            .copied()
            .filter(|r| self.tags.contains_key(r))
            .collect();
        v.sort_unstable();
        v
    }

    /// An accepted relation's tag.
    pub fn tag(&self, relation: i64, key: &str) -> Option<&str> {
        self.tags
            .get(&relation)?
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// An accepted relation's tags.
    pub fn tags(&self, relation: i64) -> Option<&[(String, String)]> {
        self.tags.get(&relation).map(|v| v.as_slice())
    }

    /// Whether a relation was accepted.
    pub fn has_tags(&self, relation: i64) -> bool {
        self.tags.contains_key(&relation)
    }

    /// Set a tag on an accepted relation (`SetTag` in post-scan).
    pub fn set_tag(&mut self, relation: i64, key: &str, value: &str) {
        let tags = self.tags.entry(relation).or_default();
        match tags.iter_mut().find(|(k, _)| k == key) {
            Some(t) => t.1 = value.to_string(),
            None => tags.push((key.to_string(), value.to_string())),
        }
    }
}
