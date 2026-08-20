use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FontFeature {
    tag: [u8; 4],
    value: u32,
}

impl FontFeature {
    pub const fn new(tag: [u8; 4], value: u32) -> Self {
        Self { tag, value }
    }

    pub fn parse(setting: &str) -> Option<Self> {
        let setting = setting.split_once(',').map_or(setting, |(head, _)| head);
        parse_font_feature_setting(setting)
    }

    pub const fn tag(self) -> [u8; 4] {
        self.tag
    }

    pub const fn value(self) -> u32 {
        self.value
    }

    fn tag_str(self) -> String {
        String::from_utf8_lossy(&self.tag).into_owned()
    }
}

impl fmt::Display for FontFeature {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.value <= 1 {
            formatter.write_str(if self.value == 0 { "-" } else { "+" })?;
            formatter.write_str(&self.tag_str())
        } else {
            write!(formatter, "{}={}", self.tag_str(), self.value)
        }
    }
}

pub fn parse_font_features(settings: &str) -> Vec<FontFeature> {
    settings
        .split(',')
        .filter_map(parse_font_feature_setting)
        .collect()
}

fn parse_font_feature_setting(setting: &str) -> Option<FontFeature> {
    let bytes = setting.as_bytes();
    let mut index = skip_space(bytes, 0);
    let mut prefixed_value = None;
    match bytes.get(index).copied() {
        Some(b'+') => {
            prefixed_value = Some(1);
            index += 1;
        }
        Some(b'-') => {
            prefixed_value = Some(0);
            index += 1;
        }
        _ => {}
    }

    let mut tag = [0_u8; 4];
    let mut len = 0_usize;
    while let Some(byte) = bytes.get(index).copied() {
        if byte == b'\'' || byte == b'"' {
            index += 1;
            continue;
        }
        if len == 4 || byte == b' ' || byte == b'\t' || byte == b'=' || byte == b',' {
            break;
        }
        tag[len] = byte;
        len += 1;
        index += 1;
    }
    if len != 4 {
        return None;
    }

    let mut rest = &setting[index..];
    loop {
        let trimmed = rest.trim_start_matches([' ', '\t', '\'', '"']);
        if trimmed.len() == rest.len() {
            break;
        }
        rest = trimmed;
    }

    let value = if let Some(value) = prefixed_value {
        if rest.trim_matches([' ', '\t']).is_empty() {
            value
        } else {
            return None;
        }
    } else if rest.trim_matches([' ', '\t']).is_empty() {
        1
    } else {
        let rest = rest.trim_start_matches([' ', '\t']);
        let rest = rest.strip_prefix('=').map_or(rest, |value| value);
        parse_font_feature_value(rest.trim_matches([' ', '\t']))?
    };

    Some(FontFeature { tag, value })
}

fn parse_font_feature_value(value: &str) -> Option<u32> {
    match value {
        "on" | "ON" | "On" => Some(1),
        "off" | "OFF" | "Off" => Some(0),
        _ if value.bytes().all(|byte| byte.is_ascii_digit()) => value.parse().ok(),
        _ => None,
    }
}

fn skip_space(bytes: &[u8], mut index: usize) -> usize {
    while matches!(bytes.get(index), Some(b' ' | b'\t')) {
        index += 1;
    }
    index
}
