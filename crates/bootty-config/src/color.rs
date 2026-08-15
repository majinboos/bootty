use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub fn from_hex(input: &str) -> Result<Self, String> {
        let hex = input.trim().strip_prefix('#').unwrap_or(input.trim());
        if !matches!(hex.len(), 6 | 8) {
            return Err(format!(
                "expected #RRGGBB or #RRGGBBAA color, got {input:?}"
            ));
        }
        let value = u32::from_str_radix(hex, 16)
            .map_err(|_| format!("expected #RRGGBB or #RRGGBBAA color, got {input:?}"))?;
        let (rgb, a) = if hex.len() == 8 {
            (value >> 8, (value & 0xff) as u8)
        } else {
            (value, 0xff)
        };
        Ok(Self {
            r: ((rgb >> 16) & 0xff) as u8,
            g: ((rgb >> 8) & 0xff) as u8,
            b: (rgb & 0xff) as u8,
            a,
        })
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
