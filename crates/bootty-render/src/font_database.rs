use std::sync::OnceLock;

#[cfg(target_os = "macos")]
use std::path::PathBuf;

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

// fontdb's default generic "monospace" family resolves to a proportional face (e.g. Helvetica on
// macOS, the platform default elsewhere), so generic monospace fallbacks — the primary text path on
// every OS, plus the CoreText symbol path on macOS — drift off monospace. Point the generic at a
// real fixed-pitch system font, picking the first installed candidate for the platform.
fn set_generic_monospace_family(database: &mut fontdb::Database) {
    for family in MONOSPACE_FAMILY_CANDIDATES {
        if database
            .query(&fontdb::Query {
                families: &[fontdb::Family::Name(family)],
                ..fontdb::Query::default()
            })
            .is_some()
        {
            database.set_monospace_family(*family);
            break;
        }
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
    for dir in macos_additional_font_dirs() {
        database.load_fonts_dir(dir);
    }
}
#[cfg(target_os = "macos")]
fn macos_additional_font_dirs() -> [PathBuf; 3] {
    [
        PathBuf::from("/opt/zerobrew/share/fonts"),
        PathBuf::from("/opt/homebrew/share/fonts"),
        PathBuf::from("/usr/local/share/fonts"),
    ]
}

#[cfg(not(target_os = "macos"))]
fn load_macos_fonts(_database: &mut fontdb::Database) {}
