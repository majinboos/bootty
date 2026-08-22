use bootty_winit::{
    modifier_remap::{ModifierRemapParseError, ModifierRemapSet},
    terminal::KeyMods,
};
use pretty_assertions::assert_eq;

fn left_ctrl() -> KeyMods {
    KeyMods {
        ctrl: true,
        ..KeyMods::default()
    }
}

fn right_ctrl() -> KeyMods {
    KeyMods {
        ctrl: true,
        right_ctrl: true,
        ..KeyMods::default()
    }
}

fn left_alt() -> KeyMods {
    KeyMods {
        alt: true,
        ..KeyMods::default()
    }
}

fn right_alt() -> KeyMods {
    KeyMods {
        alt: true,
        right_alt: true,
        ..KeyMods::default()
    }
}

fn left_command() -> KeyMods {
    KeyMods {
        command: true,
        ..KeyMods::default()
    }
}

#[test]
fn unsided_source_maps_both_sides_to_the_left_target() {
    let mut remaps = ModifierRemapSet::default();
    remaps.parse("ctrl=super").expect("remap parses");
    remaps.finalize();

    assert_eq!(remaps.apply(left_ctrl()), left_command());
    assert_eq!(remaps.apply(right_ctrl()), left_command());
    assert_eq!(
        remaps.formatted_entries(),
        vec![
            "right_ctrl=left_super".to_owned(),
            "left_ctrl=left_super".to_owned(),
        ]
    );
}

#[test]
fn sided_source_and_target_preserve_other_modifier_sides() {
    let mut remaps = ModifierRemapSet::default();
    remaps
        .parse("left_alt=right_ctrl")
        .expect("sided remap parses");
    remaps.finalize();

    assert_eq!(remaps.apply(left_alt()), right_ctrl());
    assert_eq!(remaps.apply(right_alt()), right_alt());
}

#[test]
fn aliases_and_errors_keep_the_public_grammar() {
    let mut remaps = ModifierRemapSet::default();
    remaps.parse("cmd=control").expect("aliases parse");
    remaps.parse("opt=shift").expect("aliases parse");

    assert_eq!(
        remaps.parse("ctrl"),
        Err(ModifierRemapParseError::MissingAssignment)
    );
    assert_eq!(
        remaps.parse("middle_ctrl=super"),
        Err(ModifierRemapParseError::InvalidModifier(
            "middle_ctrl".to_owned()
        ))
    );
}

#[test]
fn an_empty_set_formats_as_the_clear_entry() {
    assert_eq!(
        ModifierRemapSet::default().formatted_entries(),
        vec![String::new()]
    );
}
