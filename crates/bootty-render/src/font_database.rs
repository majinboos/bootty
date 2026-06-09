use std::sync::OnceLock;

pub fn system_font_database() -> &'static fontdb::Database {
    static SYSTEM_FONT_DATABASE: OnceLock<fontdb::Database> = OnceLock::new();
    SYSTEM_FONT_DATABASE.get_or_init(|| {
        let mut database = fontdb::Database::new();
        database.load_system_fonts();
        database
    })
}
