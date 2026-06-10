use libghostty_vt::style::RgbColor;
use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub fn from_hex(input: &str) -> Result<Self, String> {
        let hex = input.trim().strip_prefix('#').unwrap_or(input.trim());
        if hex.len() != 6 {
            return Err(format!("expected #RRGGBB color, got {input:?}"));
        }
        let value = u32::from_str_radix(hex, 16)
            .map_err(|_| format!("expected #RRGGBB color, got {input:?}"))?;
        Ok(Self {
            r: ((value >> 16) & 0xff) as u8,
            g: ((value >> 8) & 0xff) as u8,
            b: (value & 0xff) as u8,
        })
    }
}

impl From<Color> for RgbColor {
    fn from(color: Color) -> Self {
        RgbColor {
            r: color.r,
            g: color.g,
            b: color.b,
        }
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_hex(&value).map_err(serde::de::Error::custom)
    }
}
