use serde_json::Value;

use crate::mux::command::{MuxDirection, MuxPaneResize};

use super::{
    ArgumentSchema, CommandDescriptor, CommandOutcome, CompactSchema, PaneResizeArgument,
    ResourceKind, ValueType,
};

pub(super) fn invalid_pane_select_argument() -> CommandOutcome {
    CommandOutcome::Failed {
        code: "invalid_arguments".to_owned(),
        message: "pane.select requires direction left, right, up, or down".to_owned(),
    }
}

pub(super) fn parse_pane_select_direction(value: &str) -> Result<MuxDirection, CommandOutcome> {
    match value {
        "left" => Ok(MuxDirection::Left),
        "right" => Ok(MuxDirection::Right),
        "up" => Ok(MuxDirection::Up),
        "down" => Ok(MuxDirection::Down),
        _ => Err(invalid_pane_select_argument()),
    }
}

pub(super) fn invalid_pane_resize_argument() -> CommandOutcome {
    CommandOutcome::Failed {
        code: "invalid_arguments".to_owned(),
        message: "pane.resize requires a positive directional or absolute adjustment".to_owned(),
    }
}

pub(super) fn parse_pane_resize_argument(value: &str) -> Result<MuxPaneResize, CommandOutcome> {
    let argument = serde_json::from_str::<PaneResizeArgument>(value)
        .map_err(|_| invalid_pane_resize_argument())?;
    let adjustment = match argument {
        PaneResizeArgument::Directional { direction, cells } => {
            let direction = match direction.as_str() {
                "left" => MuxDirection::Left,
                "down" => MuxDirection::Down,
                "up" => MuxDirection::Up,
                "right" => MuxDirection::Right,
                _ => return Err(invalid_pane_resize_argument()),
            };
            MuxPaneResize::Directional { direction, cells }
        }
        PaneResizeArgument::Absolute { columns, rows } => MuxPaneResize::Absolute { columns, rows },
    };
    adjustment
        .is_valid()
        .then_some(adjustment)
        .ok_or_else(invalid_pane_resize_argument)
}

pub(super) fn action_with_arguments(action: &str, arguments: &[String]) -> String {
    match arguments {
        [] => action.to_owned(),
        arguments => format!("{action}:{}", arguments.join(":")),
    }
}

pub(super) fn normalize_arguments(
    descriptor: &CommandDescriptor,
    arguments: &mut Vec<String>,
) -> Result<(), CommandOutcome> {
    let schemas = &descriptor.arguments.arguments;
    if schemas
        .iter()
        .enumerate()
        .any(|(index, schema)| schema.repeated && index + 1 != schemas.len())
    {
        return Err(invalid_arguments(
            descriptor,
            "only the final argument may be repeated",
        ));
    }
    if !schemas.last().is_some_and(|schema| schema.repeated) && arguments.len() > schemas.len() {
        return Err(invalid_arguments(
            descriptor,
            &format!(
                "expects at most {} argument(s), got {}",
                schemas.len(),
                arguments.len()
            ),
        ));
    }
    for schema in schemas.iter().skip(arguments.len()) {
        if let Some(default) = &schema.default {
            arguments.push(default.clone());
        } else if schema.required {
            return Err(invalid_arguments(
                descriptor,
                &format!("is missing required {} argument", schema.name),
            ));
        }
    }
    Ok(())
}

pub(super) fn validate_arguments(
    descriptor: &CommandDescriptor,
    arguments: &[String],
) -> Result<(), CommandOutcome> {
    let schemas = &descriptor.arguments.arguments;
    let repeated = schemas.last().is_some_and(|schema| schema.repeated);
    let required = schemas.iter().filter(|argument| argument.required).count();
    if arguments.len() < required || (!repeated && arguments.len() > schemas.len()) {
        return Err(invalid_arguments(
            descriptor,
            &format!(
                "expects {}{} argument(s), got {}",
                required,
                if repeated { " or more" } else { "" },
                arguments.len()
            ),
        ));
    }
    for (index, value) in arguments.iter().enumerate() {
        let schema = schemas
            .get(index)
            .or_else(|| repeated.then(|| schemas.last()).flatten())
            .expect("argument count was validated");
        let json = || serde_json::from_str::<Value>(value).ok();
        let valid_type = match schema.value_type {
            ValueType::Null => matches!(json(), Some(Value::Null)),
            ValueType::Boolean => value.parse::<bool>().is_ok(),
            ValueType::Integer => value.parse::<i64>().is_ok(),
            ValueType::Number => value.parse::<f32>().is_ok_and(f32::is_finite),
            ValueType::String => true,
            ValueType::Enum | ValueType::ResourceRef => !value.is_empty(),
            ValueType::Array => matches!(json(), Some(Value::Array(_))),
            ValueType::Object => matches!(json(), Some(Value::Object(_))),
            ValueType::Json => json().is_some(),
        };
        let parsed_integer = || value.parse::<i64>().ok();
        let valid_minimum = schema
            .minimum
            .is_none_or(|minimum| parsed_integer().is_some_and(|value| value >= minimum));
        let valid_maximum = schema
            .maximum
            .is_none_or(|maximum| parsed_integer().is_some_and(|value| value <= maximum));
        let valid = valid_type
            && valid_minimum
            && valid_maximum
            && (schema.choices.is_empty() || schema.choices.contains(value));
        if !valid {
            return Err(invalid_arguments(
                descriptor,
                &format!("has an invalid {} argument", schema.name),
            ));
        }
    }
    Ok(())
}

fn invalid_arguments(descriptor: &CommandDescriptor, detail: &str) -> CommandOutcome {
    CommandOutcome::Failed {
        code: "invalid_arguments".to_owned(),
        message: format!("command {} {detail}", descriptor.id),
    }
}

pub(super) fn legacy_schema_for(id: &str) -> CompactSchema {
    let argument = match id {
        "select_tab" | "select_session" | "select_space" => Some(ArgumentSchema {
            name: "index".to_owned(),
            value_type: ValueType::Integer,
            required: true,
            choices: Vec::new(),
            minimum: Some(1),
            maximum: Some(i64::from(u32::MAX)),
            default: None,
            repeated: false,
        }),
        "move_tab" | "move_session" => Some(ArgumentSchema {
            name: "delta".to_owned(),
            value_type: ValueType::Integer,
            required: true,
            choices: Vec::new(),
            minimum: Some(i64::from(i32::MIN)),
            maximum: Some(i64::from(i32::MAX)),
            default: None,
            repeated: false,
        }),
        "scroll_page_lines" => Some(ArgumentSchema {
            name: "delta".to_owned(),
            value_type: ValueType::Integer,
            required: true,
            choices: Vec::new(),
            minimum: Some(i64::from(i16::MIN)),
            maximum: Some(i64::from(i16::MAX)),
            default: None,
            repeated: false,
        }),
        "increase_font_size" | "decrease_font_size" | "set_font_size" => {
            Some(argument("size", ValueType::Number))
        }
        "select_pane" => Some(ArgumentSchema {
            name: "direction".to_owned(),
            value_type: ValueType::Enum,
            required: true,
            choices: ["left", "right", "up", "down"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            minimum: None,
            maximum: None,
            default: None,
            repeated: false,
        }),
        "change_appearance" => Some(ArgumentSchema {
            name: "appearance".to_owned(),
            value_type: ValueType::Enum,
            required: true,
            choices: ["system", "light", "dark"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            minimum: None,
            maximum: None,
            default: None,
            repeated: false,
        }),
        "navigate_search" => Some(ArgumentSchema {
            name: "direction".to_owned(),
            value_type: ValueType::Enum,
            required: true,
            choices: ["next", "previous"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            minimum: None,
            maximum: None,
            default: None,
            repeated: false,
        }),
        "copy_to_clipboard" => Some(ArgumentSchema {
            name: "format".to_owned(),
            value_type: ValueType::Enum,
            required: false,
            choices: ["plain", "vt", "html", "mixed"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            minimum: None,
            maximum: None,
            default: None,
            repeated: false,
        }),
        "csi" | "esc" | "text" | "search" => Some(argument("value", ValueType::String)),
        _ => None,
    };
    CompactSchema {
        arguments: argument.into_iter().collect(),
    }
}

pub(super) fn argument(name: &str, value_type: ValueType) -> ArgumentSchema {
    ArgumentSchema {
        name: name.to_owned(),
        value_type,
        required: true,
        choices: Vec::new(),
        minimum: None,
        maximum: None,
        default: None,
        repeated: false,
    }
}
pub(super) fn legacy_target_for(id: &str) -> Option<ResourceKind> {
    match id {
        "quit" => Some(ResourceKind::Instance),
        "close_window"
        | "toggle_fullscreen"
        | "toggle_sidebar_focus"
        | "toggle_sidebar_visibility"
        | "open_settings"
        | "change_appearance"
        | "switch_theme"
        | "reload_config"
        | "create_space"
        | "next_space"
        | "previous_space"
        | "select_space"
        | "show_keybinds"
        | "command_palette"
        | "increase_font_size"
        | "decrease_font_size"
        | "reset_font_size"
        | "set_font_size" => Some(ResourceKind::ApplicationWindow),
        "new_mux_session" | "session_picker" | "next_session" | "previous_session"
        | "last_session" | "select_session" | "close_space" | "edit_space" => {
            Some(ResourceKind::Binding)
        }
        "new_tab" | "rename_session" | "ditch_session" | "move_session" | "next_tab"
        | "previous_tab" | "last_tab" | "select_tab" => Some(ResourceKind::Session),
        "move_tab" | "rename_tab" => Some(ResourceKind::MuxWindow),
        "select_pane" | "next_pane" | "previous_pane" => Some(ResourceKind::Pane),
        "split_right" | "split_down" | "kill_pane" | "close_surface" | "toggle_pane_zoom" => {
            Some(ResourceKind::Pane)
        }
        "scroll_to_top"
        | "scroll_to_bottom"
        | "scroll_page_up"
        | "scroll_page_down"
        | "scroll_page_lines"
        | "start_search"
        | "search"
        | "search_selection"
        | "navigate_search"
        | "end_search"
        | "copy_to_clipboard"
        | "copy_mode"
        | "paste_from_clipboard"
        | "csi"
        | "esc"
        | "text" => Some(ResourceKind::Terminal),
        _ => None,
    }
}
