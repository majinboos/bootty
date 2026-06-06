use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
};

use crate::terminal_text::FontStyle;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FontDiscoveryDescriptor {
    pub family: Option<String>,
    pub codepoint: Option<char>,
    pub size_points: f32,
    pub style: FontStyle,
}

impl FontDiscoveryDescriptor {
    pub fn hashcode(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.family.hash(&mut hasher);
        self.codepoint.hash(&mut hasher);
        self.size_points.to_bits().hash(&mut hasher);
        self.style.hash(&mut hasher);
        hasher.finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeferredFontFace {
    family: String,
    name: String,
    supported_ranges: Vec<(char, char)>,
    style: FontStyle,
}

impl DeferredFontFace {
    pub fn new(
        family: impl Into<String>,
        name: impl Into<String>,
        supported_ranges: impl IntoIterator<Item = (char, char)>,
        style: FontStyle,
    ) -> Self {
        Self {
            family: family.into(),
            name: name.into(),
            supported_ranges: supported_ranges.into_iter().collect(),
            style,
        }
    }

    pub fn family_name(&self) -> &str {
        &self.family
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn has_codepoint(&self, codepoint: char) -> bool {
        self.supported_ranges
            .iter()
            .any(|(start, end)| *start <= codepoint && codepoint <= *end)
    }

    pub fn load(&self, size_points: f32) -> LoadedFontFace {
        LoadedFontFace {
            family: self.family.clone(),
            name: self.name.clone(),
            size_points,
            style: self.style,
        }
    }

    fn match_score(&self, descriptor: &FontDiscoveryDescriptor) -> Option<u8> {
        let mut score = 0;
        if let Some(family) = &descriptor.family {
            if ascii_eq(&self.family, family) || ascii_eq(&self.name, family) {
                score += 2;
            } else {
                return None;
            }
        }
        if let Some(codepoint) = descriptor.codepoint {
            if self.has_codepoint(codepoint) {
                score += 1;
            } else {
                return None;
            }
        }
        Some(score)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LoadedFontFace {
    pub family: String,
    pub name: String,
    pub size_points: f32,
    pub style: FontStyle,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InMemoryFontDiscovery {
    faces: Vec<DeferredFontFace>,
}

impl InMemoryFontDiscovery {
    pub fn new(faces: impl IntoIterator<Item = DeferredFontFace>) -> Self {
        Self {
            faces: faces.into_iter().collect(),
        }
    }

    pub fn discover(&self, descriptor: &FontDiscoveryDescriptor) -> Vec<DeferredFontFace> {
        let mut matches = self
            .faces
            .iter()
            .filter_map(|face| face.match_score(descriptor).map(|score| (score, face)))
            .collect::<Vec<_>>();
        matches.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .cmp(left_score)
                .then_with(|| left.family.cmp(&right.family))
                .then_with(|| left.name.cmp(&right.name))
        });
        matches.into_iter().map(|(_, face)| face.clone()).collect()
    }

    pub fn discover_fallback(&self, codepoint: char) -> Vec<DeferredFontFace> {
        self.discover(&FontDiscoveryDescriptor {
            codepoint: Some(codepoint),
            ..FontDiscoveryDescriptor::default()
        })
    }
}

fn ascii_eq(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn discovery() -> InMemoryFontDiscovery {
        InMemoryFontDiscovery::new([
            DeferredFontFace::new("Mono", "Mono Regular", [(' ', '~')], FontStyle::Regular),
            DeferredFontFace::new("Emoji", "Emoji Color", [('😀', '🙏')], FontStyle::Regular),
            DeferredFontFace::new(
                "Symbols",
                "Symbols Regular",
                [('─', '╿')],
                FontStyle::Regular,
            ),
        ])
    }

    #[test]
    fn font_discovery_descriptor_ports_hash_cases() {
        let empty = FontDiscoveryDescriptor::default();
        let family_a = FontDiscoveryDescriptor {
            family: Some("A".to_owned()),
            ..FontDiscoveryDescriptor::default()
        };
        let family_b = FontDiscoveryDescriptor {
            family: Some("B".to_owned()),
            ..FontDiscoveryDescriptor::default()
        };

        assert_ne!(empty.hashcode(), 0);
        assert_ne!(family_a.hashcode(), family_b.hashcode());
    }

    #[test]
    fn font_discovery_ports_family_codepoint_and_sorting_cases() {
        let discovery = discovery();

        let mono = discovery.discover(&FontDiscoveryDescriptor {
            family: Some("mono".to_owned()),
            size_points: 12.0,
            ..FontDiscoveryDescriptor::default()
        });
        assert_eq!(mono.len(), 1);
        assert_eq!(mono[0].family_name(), "Mono");

        let ascii = discovery.discover_fallback('A');
        assert_eq!(ascii[0].family_name(), "Mono");
        assert!(ascii[0].has_codepoint('B'));

        let box_drawing = discovery.discover_fallback('─');
        assert_eq!(box_drawing[0].family_name(), "Symbols");

        let emoji = discovery.discover_fallback('😀');
        assert_eq!(emoji[0].family_name(), "Emoji");
    }

    #[test]
    fn deferred_font_face_ports_name_load_and_codepoint_cases() {
        let face = discovery()
            .discover(&FontDiscoveryDescriptor {
                family: Some("Mono".to_owned()),
                codepoint: Some(' '),
                size_points: 13.0,
                ..FontDiscoveryDescriptor::default()
            })
            .into_iter()
            .next()
            .expect("deferred face");

        assert_eq!(face.family_name(), "Mono");
        assert_eq!(face.name(), "Mono Regular");
        assert!(face.has_codepoint(' '));
        assert!(!face.has_codepoint('😀'));

        let loaded = face.load(13.0);
        assert_eq!(loaded.family, "Mono");
        assert_eq!(loaded.name, "Mono Regular");
        assert_eq!(loaded.size_points, 13.0);
        assert_eq!(loaded.style, FontStyle::Regular);
    }
}
