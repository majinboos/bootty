use std::sync::OnceLock;

use ab_glyph::{FontArc, FontVec};
use eframe::egui::{FontData, FontDefinitions, FontFamily};

use crate::terminal_text::FontStyle;

#[cfg(target_os = "macos")]
use std::path::PathBuf;

/// Build egui's UI text families from the shared system font database.
pub fn ui_font_definitions(families: &[String]) -> FontDefinitions {
    let database = system_font_database();
    let mut fonts = FontDefinitions::default();
    let mut loaded = false;
    for family in families.iter().rev() {
        loaded |= add_font(&mut fonts, database, fontdb::Family::Name(family), true);
    }
    if !loaded {
        add_font(&mut fonts, database, fontdb::Family::Monospace, true);
    }
    for family in "Apple Symbols|Segoe UI Symbol|Noto Sans Symbols 2|Noto Sans Symbols|DejaVu Sans|Symbola|Arial Unicode MS".split('|') {
        if add_font(&mut fonts, database, fontdb::Family::Name(family), false) {
            break;
        }
    }
    for ch in "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏".chars() {
        let supports = |face: &fontdb::FaceInfo| {
            database
                .with_face_data(face.id, |data, index| {
                    rustybuzz::ttf_parser::Face::parse(data, index)
                        .is_ok_and(|font| font.glyph_index(ch).is_some())
                })
                .unwrap_or(false)
        };
        let face = database
            .faces()
            .find(|face| face.monospaced && supports(face))
            .or_else(|| database.faces().find(|face| supports(face)));
        if let Some(face) = face {
            add_face(&mut fonts, database, face.id, false);
        }
    }
    fonts
}

fn add_font(
    fonts: &mut FontDefinitions,
    database: &fontdb::Database,
    family: fontdb::Family<'_>,
    first: bool,
) -> bool {
    database
        .query(&fontdb::Query {
            families: &[family],
            ..fontdb::Query::default()
        })
        .is_some_and(|id| add_face(fonts, database, id, first))
}

fn add_face(
    fonts: &mut FontDefinitions,
    database: &fontdb::Database,
    id: fontdb::ID,
    first: bool,
) -> bool {
    let Some((name, (bytes, index))) = database.face(id).and_then(|face| {
        database.with_face_data(id, |data, index| {
            (
                format!("bootty-ui-face-{}", face.post_script_name),
                (data.to_vec(), index),
            )
        })
    }) else {
        return false;
    };
    if fonts.font_data.contains_key(&name) {
        return false;
    }
    let mut data = FontData::from_owned(bytes);
    data.index = index;
    fonts
        .font_data
        .insert(name.clone(), std::sync::Arc::new(data));
    for family in [FontFamily::Monospace, FontFamily::Proportional] {
        let entries = fonts.families.entry(family).or_default();
        match first {
            true => entries.insert(0, name.clone()),
            false => entries.push(name.clone()),
        };
    }
    true
}

pub(super) fn query_font_id(
    database: &fontdb::Database,
    families: &[fontdb::Family<'_>],
    style: FontStyle,
) -> Option<fontdb::ID> {
    database.query(&fontdb::Query {
        families,
        weight: font_weight(style),
        style: font_style(style),
        ..fontdb::Query::default()
    })
}

pub(super) fn load_matching_font(
    database: &fontdb::Database,
    families: &[fontdb::Family<'_>],
    style: FontStyle,
) -> Option<FontArc> {
    query_font_id(database, families, style).and_then(|id| load_font_id(database, id))
}

pub(super) fn load_font_id(database: &fontdb::Database, id: fontdb::ID) -> Option<FontArc> {
    database
        .with_face_data(id, |data, index| {
            FontVec::try_from_vec_and_index(data.to_vec(), index)
                .ok()
                .map(FontArc::new)
        })
        .flatten()
}

pub(super) fn font_weight(style: FontStyle) -> fontdb::Weight {
    match style {
        FontStyle::Bold | FontStyle::BoldItalic => fontdb::Weight::BOLD,
        FontStyle::Regular | FontStyle::Italic => fontdb::Weight::NORMAL,
    }
}

pub(super) fn font_style(style: FontStyle) -> fontdb::Style {
    match style {
        FontStyle::Italic | FontStyle::BoldItalic => fontdb::Style::Italic,
        FontStyle::Regular | FontStyle::Bold => fontdb::Style::Normal,
    }
}

/// Every family name the system database exposes, sorted and de-duplicated. Scanning the database
/// is expensive, so callers read this once and keep the list.
#[must_use]
pub fn installed_family_names() -> Vec<String> {
    let mut names: Vec<String> = system_font_database()
        .faces()
        .filter_map(|face| face.families.first().map(|(name, _)| name.clone()))
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

pub fn system_font_database() -> &'static fontdb::Database {
    static SYSTEM_FONT_DATABASE: OnceLock<fontdb::Database> = OnceLock::new();
    SYSTEM_FONT_DATABASE.get_or_init(load_system_font_database)
}

#[doc(hidden)]
pub fn load_system_font_database() -> fontdb::Database {
    let mut database = fontdb::Database::new();
    database.load_system_fonts();
    load_macos_fonts(&mut database);
    set_generic_monospace_family(&mut database);
    database
}

// Keep fontdb's generic family on a real fixed-pitch system font.
fn set_generic_monospace_family(database: &mut fontdb::Database) {
    if let Some(family) = MONOSPACE_FAMILY_CANDIDATES.iter().find(|family| {
        database
            .query(&fontdb::Query {
                families: &[fontdb::Family::Name(family)],
                ..fontdb::Query::default()
            })
            .is_some()
    }) {
        database.set_monospace_family(*family);
    }
}

#[cfg(target_os = "macos")]
const MONOSPACE_FAMILY_CANDIDATES: &[&str] = &["SF Mono", "Menlo", "Monaco"];

#[cfg(windows)]
const MONOSPACE_FAMILY_CANDIDATES: &[&str] = &["Cascadia Mono", "Consolas", "Courier New"];

#[cfg(not(any(target_os = "macos", windows)))]
const MONOSPACE_FAMILY_CANDIDATES: &[&str] = &[
    "DejaVu Sans Mono",
    "Liberation Mono",
    "Noto Sans Mono",
    "Ubuntu Mono",
    "JetBrains Mono",
    "Source Code Pro",
];

#[cfg(target_os = "macos")]
fn load_macos_fonts(database: &mut fontdb::Database) {
    for dir in [
        PathBuf::from("/opt/zerobrew/share/fonts"),
        PathBuf::from("/opt/homebrew/share/fonts"),
        PathBuf::from("/usr/local/share/fonts"),
    ] {
        database.load_fonts_dir(dir);
    }
}

#[cfg(not(target_os = "macos"))]
fn load_macos_fonts(_database: &mut fontdb::Database) {}
