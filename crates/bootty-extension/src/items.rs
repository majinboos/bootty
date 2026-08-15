use crate::{ModuleColor, ModuleCoord, ModuleCornerRadius, ModuleItem, ModulePrimitive};
use mlua::{Table, Value};

const ERROR_COLOR: ModuleColor = ModuleColor::rgb(0xf3, 0x8b, 0xa8);

pub fn error_item(message: &str) -> ModuleItem {
    ModuleItem {
        text: first_line(message),
        fg: Some(ERROR_COLOR),
        ..ModuleItem::default()
    }
}

pub fn items_from_value(value: Value) -> Vec<ModuleItem> {
    match value {
        Value::String(text) => vec![ModuleItem {
            text: text.to_string_lossy(),
            ..ModuleItem::default()
        }],
        Value::Table(table) => {
            // Item text is optional; icon/gauge/action-only tables are single items too.
            if table_looks_like_item(&table) {
                vec![item_from_table(&table)]
            } else {
                table
                    .sequence_values::<Table>()
                    .filter_map(Result::ok)
                    .map(|item| item_from_table(&item))
                    .collect()
            }
        }
        _ => Vec::new(),
    }
}

fn table_looks_like_item(table: &Table) -> bool {
    [
        "text",
        "fg",
        "bg",
        "stroke",
        "icon",
        "gauge",
        "primitives",
        "pad_left",
        "pad_right",
        "join",
        "gap",
        "action",
        "key",
        "kind",
        "number",
        "indent",
        "tree",
        "selectable",
        "session_id",
        "reorder_anchor",
        "current",
        "active",
        "dim_fg",
    ]
    .into_iter()
    .any(|key| table.contains_key(key).unwrap_or(false))
}

fn item_from_table(table: &Table) -> ModuleItem {
    ModuleItem {
        text: table.get::<String>("text").unwrap_or_default(),
        fg: color_field(table, "fg"),
        bg: color_field(table, "bg"),
        stroke: color_field(table, "stroke"),
        icon: string_field(table, "icon"),
        gauge: table
            .get::<f64>("gauge")
            .ok()
            .filter(|value| value.is_finite())
            .map(|value| value.clamp(0.0, 1.0) as f32),
        primitives: table
            .get::<Table>("primitives")
            .ok()
            .map(|primitives| primitives_from_table(&primitives))
            .unwrap_or_default(),
        pad_left: table
            .get::<f64>("pad_left")
            .ok()
            .filter(|value| value.is_finite())
            .unwrap_or(0.0)
            .max(0.0) as f32,
        pad_right: table
            .get::<f64>("pad_right")
            .ok()
            .filter(|value| value.is_finite())
            .unwrap_or(0.0)
            .max(0.0) as f32,
        join: table.get::<bool>("join").ok(),
        gap: table.get::<bool>("gap").ok(),
        action: string_field(table, "action"),
        key: string_field(table, "key"),
        kind: string_field(table, "kind"),
        number: table.get::<u32>("number").ok().map(|value| value as usize),
        indent: table.get::<u16>("indent").ok(),
        tree: string_field(table, "tree"),
        selectable: table.get::<bool>("selectable").ok(),
        session_id: string_field(table, "session_id"),
        reorder_anchor: string_field(table, "reorder_anchor"),
        current: table.get::<bool>("current").ok(),
        active: table.get::<bool>("active").ok(),
        dim_fg: color_field(table, "dim_fg"),
    }
}

fn string_field(table: &Table, key: &str) -> Option<String> {
    table
        .get::<String>(key)
        .ok()
        .filter(|value| !value.is_empty())
}

fn color_field(table: &Table, key: &str) -> Option<ModuleColor> {
    table
        .get::<String>(key)
        .ok()
        .and_then(|hex| parse_hex_color(&hex))
}

fn primitives_from_table(table: &Table) -> Vec<ModulePrimitive> {
    table
        .sequence_values::<Table>()
        .filter_map(Result::ok)
        .filter_map(|primitive| primitive_from_table(&primitive))
        .collect()
}

fn primitive_from_table(table: &Table) -> Option<ModulePrimitive> {
    let kind = table
        .get::<String>("type")
        .or_else(|_| table.get::<String>("kind"))
        .ok()?;
    let fill = table
        .get::<String>("fill")
        .ok()
        .and_then(|hex| parse_hex_color(&hex));
    let stroke = table
        .get::<String>("stroke")
        .ok()
        .and_then(|hex| parse_hex_color(&hex));
    match kind.as_str() {
        "rect" => Some(ModulePrimitive::Rect {
            fill,
            stroke,
            x: coord_from_table(table, "x", "x_px", 0.0),
            y: coord_from_table(table, "y", "y_px", 0.0),
            w: coord_from_table(table, "w", "w_px", 1.0),
            h: coord_from_table(table, "h", "h_px", 1.0),
            radius: radius_from_table(table),
        }),
        "polygon" => {
            let points = table
                .get::<Table>("points")
                .ok()?
                .sequence_values::<Table>()
                .filter_map(Result::ok)
                .map(|point| {
                    (
                        coord_from_table(&point, "x", "dx", 0.0),
                        coord_from_table(&point, "y", "dy", 0.0),
                    )
                })
                .collect::<Vec<_>>();
            (points.len() >= 3).then_some(ModulePrimitive::Polygon {
                fill,
                stroke,
                points,
            })
        }
        "text" => {
            let text = string_field(table, "text")?;
            Some(ModulePrimitive::Text {
                text,
                color: color_field(table, "color").or(fill),
                x: coord_from_table(table, "x", "x_px", 0.0),
                y: coord_from_table(table, "y", "y_px", 0.5),
                size: positive_f32_field(table, "size").unwrap_or(11.0),
                align: string_field(table, "align").unwrap_or_else(|| "left_center".to_owned()),
                min_width: positive_f32_field(table, "min_width"),
            })
        }
        "icon" => {
            let icon = string_field(table, "icon").or_else(|| string_field(table, "slug"))?;
            Some(ModulePrimitive::Icon {
                icon,
                color: color_field(table, "color").or(fill),
                x: coord_from_table(table, "x", "x_px", 0.0),
                y: coord_from_table(table, "y", "y_px", 0.5),
                size: positive_f32_field(table, "size").unwrap_or(12.0),
                min_width: positive_f32_field(table, "min_width"),
            })
        }
        _ => None,
    }
}

fn coord_from_table(table: &Table, frac_key: &str, px_key: &str, default_frac: f32) -> ModuleCoord {
    let frac = table
        .get::<f64>(frac_key)
        .ok()
        .filter(|value| value.is_finite())
        .map_or(default_frac, |value| value as f32);
    let px = table
        .get::<f64>(px_key)
        .ok()
        .filter(|value| value.is_finite())
        .map_or(0.0, |value| value as f32);
    ModuleCoord { frac, px }
}

fn positive_f32_field(table: &Table, key: &str) -> Option<f32> {
    table
        .get::<f64>(key)
        .ok()
        .filter(|value| value.is_finite() && *value > 0.0)
        .map(|value| value as f32)
}

fn radius_from_table(table: &Table) -> ModuleCornerRadius {
    if let Ok(radius) = table.get::<f64>("radius") {
        let radius = radius.clamp(0.0, f64::from(u8::MAX)) as u8;
        return ModuleCornerRadius {
            nw: radius,
            ne: radius,
            sw: radius,
            se: radius,
        };
    }
    let Ok(radius) = table.get::<Table>("radius") else {
        return ModuleCornerRadius::default();
    };
    let corner = |key: &str| {
        radius
            .get::<f64>(key)
            .ok()
            .filter(|value| value.is_finite())
            .map_or(0, |value| value.clamp(0.0, f64::from(u8::MAX)) as u8)
    };
    ModuleCornerRadius {
        nw: corner("nw"),
        ne: corner("ne"),
        sw: corner("sw"),
        se: corner("se"),
    }
}

pub(super) fn parse_hex_color(value: &str) -> Option<ModuleColor> {
    let hex = value.trim().strip_prefix('#')?;
    match hex.len() {
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some(ModuleColor::rgb(r, g, b))
        }
        3 => {
            let expand = |slice: &str| u8::from_str_radix(slice, 16).map(|v| v * 17);
            let r = expand(&hex[0..1]).ok()?;
            let g = expand(&hex[1..2]).ok()?;
            let b = expand(&hex[2..3]).ok()?;
            Some(ModuleColor::rgb(r, g, b))
        }
        _ => None,
    }
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or(text).to_owned()
}
