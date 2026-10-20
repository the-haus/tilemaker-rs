//! An object's tags in one reusable buffer, and the `Find`/`Holds` Lua
//! functions that read them without allocating.

use std::cell::Cell;
use std::os::raw::c_int;

use mlua::ffi;

/// Tags back to back in one string, reused from object to object.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TagBuf {
    data: String,
    /// Per tag: where its key ends (and its value starts), and where its
    /// value ends; keys start where the previous value ends.
    ends: Vec<(u32, u32)>,
}

impl TagBuf {
    /// Empty.
    pub fn new() -> Self {
        Self::default()
    }

    /// Remove every tag (keeping the buffers).
    pub fn clear(&mut self) {
        self.data.clear();
        self.ends.clear();
    }

    /// Add a tag.
    pub fn push(&mut self, key: &str, value: &str) {
        self.data.push_str(key);
        let k = self.data.len() as u32;
        self.data.push_str(value);
        self.ends.push((k, self.data.len() as u32));
    }

    /// Number of tags.
    pub fn len(&self) -> usize {
        self.ends.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    /// Tag `i`.
    #[inline]
    fn at(&self, i: usize) -> (&str, &str) {
        let start = if i == 0 { 0 } else { self.ends[i - 1].1 as usize };
        let (k, v) = self.ends[i];
        (&self.data[start..k as usize], &self.data[k as usize..v as usize])
    }

    /// The tags in order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&str, &str)> + '_ {
        (0..self.ends.len()).map(move |i| self.at(i))
    }

    /// The value of `key`.
    #[inline]
    pub fn get(&self, key: &[u8]) -> Option<&str> {
        let bytes = self.data.as_bytes();
        let mut start = 0usize;
        for &(k, v) in &self.ends {
            if &bytes[start..k as usize] == key {
                return Some(&self.data[k as usize..v as usize]);
            }
            start = v as usize;
        }
        None
    }

    /// As owned pairs.
    pub fn to_vec(&self) -> Vec<(String, String)> {
        self.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }
}

impl<'a> Extend<(&'a str, &'a str)> for TagBuf {
    fn extend<I: IntoIterator<Item = (&'a str, &'a str)>>(&mut self, iter: I) {
        for (k, v) in iter {
            self.push(k, v);
        }
    }
}

impl<'a> FromIterator<(&'a str, &'a str)> for TagBuf {
    fn from_iter<I: IntoIterator<Item = (&'a str, &'a str)>>(iter: I) -> Self {
        let mut t = TagBuf::new();
        t.extend(iter);
        t
    }
}

thread_local! {
    /// The tags of the object this thread's Lua state is processing (null
    /// outside of a call). One Lua state runs on a thread at a time.
    static CURRENT: Cell<*const TagBuf> = const { Cell::new(std::ptr::null()) };
}

/// Make `tags` the ones `Find`/`Holds` read until [`clear_current`].
pub(crate) fn set_current(tags: &TagBuf) {
    CURRENT.with(|c| c.set(tags));
}

/// `Find`/`Holds` see no tags.
pub(crate) fn clear_current() {
    CURRENT.with(|c| c.set(std::ptr::null()));
}

/// The value of the key at stack index 1, if any.
///
/// # Safety
/// `l` must be a valid Lua state, and the current tags (if set) alive.
unsafe fn lookup<'a>(l: *mut ffi::lua_State) -> Option<&'a str> {
    let mut len = 0usize;
    let p = ffi::lua_tolstring(l, 1, &mut len);
    if p.is_null() {
        return None;
    }
    let key = std::slice::from_raw_parts(p as *const u8, len);
    let tags = CURRENT.with(|c| c.get());
    if tags.is_null() {
        return None;
    }
    (*tags).get(key)
}

/// `Find(key)`: the value, or `""`.
pub(crate) unsafe extern "C-unwind" fn find(l: *mut ffi::lua_State) -> c_int {
    let v = lookup(l).unwrap_or("");
    ffi::lua_pushlstring(l, v.as_ptr() as *const _, v.len());
    1
}

/// `Holds(key)`: whether the key is present.
pub(crate) unsafe extern "C-unwind" fn holds(l: *mut ffi::lua_State) -> c_int {
    ffi::lua_pushboolean(l, lookup(l).is_some() as c_int);
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer() {
        let mut t: TagBuf = [("highway", "primary"), ("name", ""), ("", "x")].into_iter().collect();
        assert_eq!(t.len(), 3);
        assert_eq!(t.get(b"highway"), Some("primary"));
        assert_eq!(t.get(b"name"), Some(""));
        assert_eq!(t.get(b""), Some("x"));
        assert_eq!(t.get(b"nam"), None);
        assert_eq!(
            t.iter().collect::<Vec<_>>(),
            vec![("highway", "primary"), ("name", ""), ("", "x")]
        );
        t.clear();
        assert!(t.is_empty());
        t.push("a", "b");
        assert_eq!(t.to_vec(), vec![("a".to_string(), "b".to_string())]);
    }
}
