use std::{
    collections::{HashMap, HashSet, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SharedGridFontSize {
    pub points: f32,
    pub xdpi: u16,
    pub ydpi: u16,
}

impl SharedGridFontSize {
    pub fn new(points: f32) -> Self {
        Self {
            points,
            xdpi: 0,
            ydpi: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedGridKey {
    font_points_bits: u32,
    xdpi: u16,
    ydpi: u16,
    descriptors: Vec<String>,
    metric_modifier_count: usize,
    freetype_load_flags: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SharedGridHandle {
    hashcode: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SharedGridEntry {
    id: u64,
    refs: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SharedGridSet {
    entries: HashMap<u64, SharedGridEntry>,
    next_id: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SharedGridIndex {
    pub collection_index: u32,
    pub style: SharedGridStyle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SharedGridStyle {
    Regular,
    Bold,
    Italic,
    BoldItalic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SharedGridGlyphKey {
    pub index: SharedGridIndex,
    pub glyph: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedGrid {
    supported_codepoints: HashSet<char>,
    codepoint_cache: HashMap<(char, SharedGridStyle), SharedGridIndex>,
    glyph_cache: HashMap<SharedGridGlyphKey, Vec<u8>>,
    resolver_enabled: bool,
}

impl SharedGridKey {
    pub fn new(size: SharedGridFontSize) -> Self {
        Self {
            font_points_bits: size.points.to_bits(),
            xdpi: size.xdpi,
            ydpi: size.ydpi,
            descriptors: vec!["monospace".to_string()],
            metric_modifier_count: 0,
            freetype_load_flags: 0,
        }
    }

    pub fn hashcode(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.hash(&mut hasher);
        hasher.finish()
    }
}

impl Hash for SharedGridKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.font_points_bits.hash(state);
        self.xdpi.hash(state);
        self.ydpi.hash(state);
        self.descriptors.hash(state);
        self.metric_modifier_count.hash(state);
        self.freetype_load_flags.hash(state);
    }
}

impl SharedGrid {
    pub fn new(supported_codepoints: impl IntoIterator<Item = char>) -> Self {
        Self {
            supported_codepoints: supported_codepoints.into_iter().collect(),
            codepoint_cache: HashMap::new(),
            glyph_cache: HashMap::new(),
            resolver_enabled: true,
        }
    }

    pub fn ascii_regular() -> Self {
        Self::new((32u8..127).map(char::from))
    }

    pub fn get_index(
        &mut self,
        codepoint: char,
        style: SharedGridStyle,
    ) -> Option<SharedGridIndex> {
        if let Some(index) = self.codepoint_cache.get(&(codepoint, style)) {
            return Some(*index);
        }
        if !self.resolver_enabled || !self.supported_codepoints.contains(&codepoint) {
            return None;
        }

        let index = SharedGridIndex {
            collection_index: 0,
            style,
        };
        self.codepoint_cache.insert((codepoint, style), index);
        Some(index)
    }

    pub fn has_codepoint(&self, index: SharedGridIndex, codepoint: char) -> bool {
        index.collection_index == 0 && self.supported_codepoints.contains(&codepoint)
    }

    pub fn disable_resolver_for_cache_only_test(&mut self) {
        self.resolver_enabled = false;
    }

    pub fn contains_glyph(&self, key: SharedGridGlyphKey) -> bool {
        self.glyph_cache.contains_key(&key)
    }

    pub fn render_glyph(
        &mut self,
        key: SharedGridGlyphKey,
        fail_after_insert: bool,
    ) -> Result<&[u8], SharedGridRenderError> {
        if self.glyph_cache.contains_key(&key) {
            return Ok(&self.glyph_cache[&key]);
        }

        self.glyph_cache.insert(key, vec![0xff]);
        if fail_after_insert {
            self.glyph_cache.remove(&key);
            return Err(SharedGridRenderError::OutOfMemory);
        }
        Ok(&self.glyph_cache[&key])
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SharedGridRenderError {
    OutOfMemory,
}

impl SharedGridSet {
    pub fn ref_grid(&mut self, key: &SharedGridKey) -> (SharedGridHandle, u64) {
        let hashcode = key.hashcode();
        let entry = self.entries.entry(hashcode).or_insert_with(|| {
            let id = self.next_id;
            self.next_id += 1;
            SharedGridEntry { id, refs: 0 }
        });
        entry.refs += 1;
        (SharedGridHandle { hashcode }, entry.id)
    }

    pub fn deref(&mut self, handle: SharedGridHandle) {
        let Some(entry) = self.entries.get_mut(&handle.hashcode) else {
            return;
        };
        entry.refs = entry.refs.saturating_sub(1);
        if entry.refs == 0 {
            self.entries.remove(&handle.hashcode);
        }
    }

    pub fn count(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_grid_key_ports_stable_hash_for_same_config() {
        let key = SharedGridKey::new(SharedGridFontSize::new(12.0));
        let key2 = SharedGridKey::new(SharedGridFontSize::new(12.0));

        assert_ne!(key.hashcode(), 0);
        assert_eq!(key.hashcode(), key2.hashcode());
    }

    #[test]
    fn shared_grid_key_ports_different_font_points() {
        let key = SharedGridKey::new(SharedGridFontSize::new(12.0));
        let key2 = SharedGridKey::new(SharedGridFontSize::new(16.0));

        assert_ne!(key.hashcode(), key2.hashcode());
    }

    #[test]
    fn shared_grid_key_ports_different_font_dpi() {
        let key = SharedGridKey::new(SharedGridFontSize {
            points: 12.0,
            xdpi: 1,
            ydpi: 0,
        });
        let key2 = SharedGridKey::new(SharedGridFontSize {
            points: 12.0,
            xdpi: 2,
            ydpi: 0,
        });

        assert_ne!(key.hashcode(), key2.hashcode());
    }

    #[test]
    fn shared_grid_set_ports_ref_and_deref_reuse() {
        let key = SharedGridKey::new(SharedGridFontSize::new(12.0));
        let mut set = SharedGridSet::default();

        let (key1, grid1) = set.ref_grid(&key);
        assert_eq!(set.count(), 1);

        let (key2, grid2) = set.ref_grid(&key);
        assert_eq!(set.count(), 1);
        assert_eq!(grid1, grid2);

        set.deref(key2);
        assert_eq!(set.count(), 1);

        set.deref(key1);
        assert_eq!(set.count(), 0);
    }

    #[test]
    fn shared_grid_ports_get_index_cache_behavior() {
        let mut grid = SharedGrid::ascii_regular();
        for byte in 32u8..127 {
            let codepoint = char::from(byte);
            let index = grid.get_index(codepoint, SharedGridStyle::Regular).unwrap();
            assert_eq!(index.style, SharedGridStyle::Regular);
            assert_eq!(index.collection_index, 0);
            assert!(grid.has_codepoint(index, codepoint));
        }

        grid.disable_resolver_for_cache_only_test();
        for byte in 32u8..127 {
            let index = grid
                .get_index(char::from(byte), SharedGridStyle::Regular)
                .unwrap();
            assert_eq!(index.collection_index, 0);
        }
        assert_eq!(grid.get_index('界', SharedGridStyle::Regular), None);
    }

    #[test]
    fn shared_grid_ports_render_glyph_error_cache_rollback() {
        let mut grid = SharedGrid::ascii_regular();
        let index = grid.get_index('A', SharedGridStyle::Regular).unwrap();
        let key = SharedGridGlyphKey { index, glyph: 42 };

        assert!(!grid.contains_glyph(key));
        assert_eq!(
            grid.render_glyph(key, true),
            Err(SharedGridRenderError::OutOfMemory)
        );
        assert!(!grid.contains_glyph(key));

        assert_eq!(grid.render_glyph(key, false).unwrap(), &[0xff]);
        assert!(grid.contains_glyph(key));
    }
}
