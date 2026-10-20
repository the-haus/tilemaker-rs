//! Which relations each element is a member of (for `member_of`).

use std::sync::Arc;

use tilemaker_pbf::{ElementType, PrimitiveBlock};

use crate::enrich::Parent;
use crate::filter::FilterSet;

/// A relation's id and tags.
type Relation = (i64, Arc<[(String, String)]>);

/// One membership: the member, and the relation and role (indices).
#[derive(Clone, Copy, Debug)]
struct Entry {
    id: i64,
    kind: ElementType,
    relation: u32,
    role: u32,
}

/// Memberships found in one block (indices local to it).
#[derive(Default)]
pub(crate) struct BlockParents {
    entries: Vec<Entry>,
    relations: Vec<Relation>,
    roles: Vec<String>,
}

impl BlockParents {
    /// The memberships of the block's relations that `filter` selects.
    pub(crate) fn scan(block: &PrimitiveBlock, filter: &FilterSet) -> Self {
        let mut out = BlockParents::default();
        let bf = filter.for_block(block);
        let mut roles = std::collections::HashMap::new();
        for r in block.relations() {
            let tags = r.tags();
            let area = crate::engine::is_area_relation(&tags);
            if !bf.matches(ElementType::Relation, area, &tags) {
                continue;
            }
            let rel = out.relations.len() as u32;
            out.relations.push((
                r.id(),
                tags.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            ));
            for m in r.members() {
                let role = *roles.entry(m.role).or_insert_with(|| {
                    out.roles.push(m.role.to_string());
                    out.roles.len() as u32 - 1
                });
                out.entries.push(Entry {
                    id: m.id,
                    kind: m.kind,
                    relation: rel,
                    role,
                });
            }
        }
        out
    }
}

/// Every membership, sorted by member.
#[derive(Default)]
pub(crate) struct Parents {
    entries: Vec<Entry>,
    relations: Vec<Relation>,
    roles: Vec<String>,
}

impl Parents {
    /// From the blocks' memberships, in file order.
    pub(crate) fn merge(blocks: Vec<BlockParents>) -> Self {
        let mut p = Parents::default();
        let mut role_ids = std::collections::HashMap::<String, u32>::new();
        for b in blocks {
            let rel_base = p.relations.len() as u32;
            let roles: Vec<u32> = b
                .roles
                .into_iter()
                .map(|r| {
                    let n = p.roles.len() as u32;
                    *role_ids.entry(r.clone()).or_insert_with(|| {
                        p.roles.push(r);
                        n
                    })
                })
                .collect();
            p.relations.extend(b.relations);
            p.entries.extend(b.entries.into_iter().map(|e| Entry {
                relation: rel_base + e.relation,
                role: roles[e.role as usize],
                ..e
            }));
        }
        // Stable: a member's relations stay in file order.
        p.entries.sort_by_key(|e| (e.kind, e.id));
        p.entries.shrink_to_fit();
        p
    }

    /// The relations element `kind`/`id` is a member of (once per role).
    pub(crate) fn of(&self, kind: ElementType, id: i64) -> Vec<Parent> {
        let start = self.entries.partition_point(|e| (e.kind, e.id) < (kind, id));
        self.entries[start..]
            .iter()
            .take_while(|e| e.kind == kind && e.id == id)
            .map(|e| {
                let (rid, tags) = &self.relations[e.relation as usize];
                Parent {
                    id: *rid,
                    role: self.roles[e.role as usize].clone(),
                    tags: tags.clone(),
                }
            })
            .collect()
    }

    /// Approximate heap bytes.
    pub(crate) fn heap_bytes(&self) -> usize {
        self.entries.capacity() * std::mem::size_of::<Entry>()
            + self
                .relations
                .iter()
                .map(|(_, t)| 24 + t.iter().map(|(k, v)| 48 + k.len() + v.len()).sum::<usize>())
                .sum::<usize>()
    }
}
