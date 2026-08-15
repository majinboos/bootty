use bootty_render::font_database::{system_font_database, ui_font_definitions};
use eframe::egui::FontFamily;

#[test]
fn ui_fonts_preserve_configured_family_order() {
    let database = system_font_database();
    let families = database
        .faces()
        .flat_map(|face| face.families.iter().map(|(family, _)| family.clone()))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .take(2)
        .collect::<Vec<_>>();
    if families.len() < 2 {
        return;
    }
    let fonts = ui_font_definitions(&families);
    let entries = &fonts.families[&FontFamily::Proportional];
    let expected = families
        .iter()
        .map(|family| {
            let id = database
                .query(&fontdb::Query {
                    families: &[fontdb::Family::Name(family)],
                    ..fontdb::Query::default()
                })
                .expect("selected system family");
            format!(
                "bootty-ui-face-{}",
                database.face(id).unwrap().post_script_name
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(&entries[..expected.len()], expected);
}

#[test]
fn ui_fonts_fall_back_to_system_monospace_for_unknown_families() {
    let fonts = ui_font_definitions(&["Bootty Missing UI Family".to_owned()]);
    assert!(!fonts.families[&FontFamily::Monospace].is_empty());
    assert!(!fonts.families[&FontFamily::Proportional].is_empty());
}

#[test]
fn braille_spinner_fallback_prefers_monospaced_support() {
    let database = system_font_database();
    let glyph = '⠋';
    let supports = |face: &fontdb::FaceInfo| {
        database
            .with_face_data(face.id, |data, index| {
                rustybuzz::ttf_parser::Face::parse(data, index)
                    .is_ok_and(|font| font.glyph_index(glyph).is_some())
            })
            .unwrap_or(false)
    };
    let first_supporting = database.faces().find(|face| supports(face));
    let first_monospaced = database
        .faces()
        .find(|face| face.monospaced && supports(face));
    let (Some(first_supporting), Some(first_monospaced)) = (first_supporting, first_monospaced)
    else {
        return;
    };
    if first_supporting.monospaced || first_supporting.id == first_monospaced.id {
        return;
    }

    let Some(primary_family) = database
        .faces()
        .find(|face| !face.monospaced && !supports(face))
        .and_then(|face| face.families.first().map(|(family, _)| family.clone()))
    else {
        return;
    };
    let fonts = ui_font_definitions(&[primary_family]);
    let monospaced_name = format!("bootty-ui-face-{}", first_monospaced.post_script_name);
    assert!(fonts.font_data.contains_key(&monospaced_name));
}
