use std::borrow::Cow;

use memchr::memchr3_iter;

pub(crate) fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    let first = *needle.first()?;
    if needle.len() == 1 {
        return haystack.iter().position(|byte| *byte == first);
    }

    let mut offset = 0;
    while let Some(relative_start) = haystack[offset..].iter().position(|byte| *byte == first) {
        let start = offset + relative_start;
        if haystack[start..].starts_with(needle) {
            return Some(start);
        }
        offset = start + 1;
    }
    None
}

pub(crate) fn find_osc_terminator(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            0x07 => return Some((index, 1)),
            0x1b if bytes.get(index + 1) == Some(&b'\\') => return Some((index, 2)),
            _ => index += 1,
        }
    }
    None
}

pub(crate) fn split_osc_payload(payload: &[u8]) -> Option<(&[u8], &[u8])> {
    let separator = payload.iter().position(|byte| *byte == b';')?;
    Some((&payload[..separator], &payload[separator + 1..]))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct TerminalWriteFeatures {
    pub(super) tmux_passthrough: bool,
    pub(super) kitty_graphics: bool,
    pub(super) osc_side_effect: bool,
    pub(super) osc_color: bool,
}

impl TerminalWriteFeatures {
    pub(super) fn needs_sanitizing(self) -> bool {
        self.tmux_passthrough || self.kitty_graphics || self.osc_side_effect || self.osc_color
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct SgrOptimizer {
    bold: bool,
    italic: bool,
    underline: bool,
    scratch: Vec<u8>,
}

impl SgrOptimizer {
    pub(super) fn reset(&mut self) {
        self.bold = false;
        self.italic = false;
        self.underline = false;
        self.scratch.clear();
    }

    pub(super) fn optimize<'a>(&'a mut self, data: &'a [u8]) -> &'a [u8] {
        let mut cursor = 0;
        let mut changed = false;
        self.scratch.clear();

        while let Some(relative_start) = data[cursor..].iter().position(|byte| *byte == 0x1b) {
            let start = cursor + relative_start;
            if data.get(start + 1) != Some(&b'[') {
                cursor = start + 1;
                continue;
            }
            let params_start = start + 2;
            let Some(relative_end) = data[params_start..].iter().position(|byte| *byte == b'm')
            else {
                break;
            };
            let end = params_start + relative_end;
            let Some(optimized) = self.optimize_sgr_params(&data[params_start..end]) else {
                cursor = end + 1;
                continue;
            };
            if changed {
                self.scratch.extend_from_slice(&data[cursor..start]);
            } else {
                self.scratch.extend_from_slice(&data[..start]);
                changed = true;
            }
            if !optimized.is_empty() {
                self.scratch.extend_from_slice(b"\x1b[");
                self.scratch.extend_from_slice(optimized);
                self.scratch.push(b'm');
            }
            cursor = end + 1;
        }

        if changed {
            self.scratch.extend_from_slice(&data[cursor..]);
            &self.scratch
        } else {
            data
        }
    }

    fn optimize_sgr_params<'a>(&mut self, params: &'a [u8]) -> Option<&'a [u8]> {
        let active = self.bold && self.italic && self.underline;
        let optimized = active
            .then(|| redundant_style_suffix_prefix(params))
            .flatten();
        self.update_state(params);
        optimized
    }

    fn update_state(&mut self, params: &[u8]) {
        if params.is_empty() || params == b"0" {
            self.reset();
            return;
        }
        for param in params.split(|byte| *byte == b';') {
            match param {
                b"0" => self.reset(),
                b"1" => self.bold = true,
                b"3" => self.italic = true,
                b"4" => self.underline = true,
                b"22" => self.bold = false,
                b"23" => self.italic = false,
                b"24" => self.underline = false,
                _ => {}
            }
        }
    }
}

fn redundant_style_suffix_prefix(params: &[u8]) -> Option<&[u8]> {
    if params == b"1;3;4" {
        return Some(&[]);
    }
    let prefix_len = params.strip_suffix(b";1;3;4")?.len();
    let prefix = &params[..prefix_len];
    color_only_sgr_params(prefix).then_some(prefix)
}

fn color_only_sgr_params(params: &[u8]) -> bool {
    if params.is_empty() {
        return false;
    }
    let mut parts = params.split(|byte| *byte == b';').peekable();
    while let Some(part) = parts.next() {
        match part {
            b"30" | b"31" | b"32" | b"33" | b"34" | b"35" | b"36" | b"37" | b"39" | b"40"
            | b"41" | b"42" | b"43" | b"44" | b"45" | b"46" | b"47" | b"49" | b"90" | b"91"
            | b"92" | b"93" | b"94" | b"95" | b"96" | b"97" | b"100" | b"101" | b"102" | b"103"
            | b"104" | b"105" | b"106" | b"107" => {}
            b"38" | b"48" => match parts.next() {
                Some(b"5") => {
                    if !parts.next().is_some_and(decimal_param) {
                        return false;
                    }
                }
                Some(b"2") => {
                    for _ in 0..3 {
                        if !parts.next().is_some_and(decimal_param) {
                            return false;
                        }
                    }
                }
                _ => return false,
            },
            _ => return false,
        }
    }
    true
}

fn decimal_param(param: &[u8]) -> bool {
    !param.is_empty() && param.iter().all(u8::is_ascii_digit)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StreamingControlState {
    Complete(usize),
    Incomplete,
    Unrecognized,
}

const STREAMING_CONTROL_PREFIXES: &[&[u8]] = &[
    b"\x1bPtmux;",
    b"\x1b_G",
    b"\x1b]0;",
    b"\x1b]1;",
    b"\x1b]2;",
    b"\x1b]7;",
    b"\x1b]4;",
    b"\x1b]10;",
    b"\x1b]11;",
    b"\x1b]9;",
    b"\x1b]22;",
    b"\x1b]52;",
    b"\x1b]66;",
    b"\x1b]133;",
    b"\x1b]777;",
    b"\x1b]1337;",
];

const SIDE_EFFECT_OSC_PREFIXES: &[&[u8]] = &[
    b"1;", b"9;", b"22;", b"52;", b"66;", b"133;", b"777;", b"1337;",
];

const COLOR_OSC_PREFIXES: &[&[u8]] = &[
    b"4;", b"10;", b"11;", b"12;", b"13;", b"14;", b"15;", b"16;", b"17;", b"18;", b"19;", b"110",
    b"111", b"112", b"113", b"114", b"115", b"116", b"117", b"118", b"119",
];

pub(super) fn complete_streaming_control_prefix_len(data: &[u8]) -> usize {
    let mut index = 0;
    while let Some(relative_start) = data[index..].iter().position(|byte| *byte == 0x1b) {
        let start = index + relative_start;
        match streaming_control_state(&data[start..]) {
            StreamingControlState::Complete(len) => index = start + len,
            StreamingControlState::Incomplete => return start,
            StreamingControlState::Unrecognized => index = start + 1,
        }
    }
    data.len()
}

pub(super) fn contains_tracked_streaming_control(data: &[u8]) -> bool {
    if data.last() == Some(&0x1b) {
        return true;
    }

    for marker in memchr3_iter(b']', b'_', b'P', data) {
        if marker == 0 || data[marker - 1] != 0x1b {
            continue;
        }

        match data[marker] {
            b']' => return true,
            b'_' if data.get(marker + 1).is_none_or(|byte| *byte == b'G') => return true,
            b'P' => {
                let start = marker - 1;
                if b"\x1bPtmux;".starts_with(&data[start..data.len().min(start + 7)]) {
                    return true;
                }
            }
            _ => {}
        }
    }

    false
}

pub(super) const CURSOR_HOME: &[u8; 3] = b"\x1b[H";

pub(super) fn repeated_cursor_home_prefix_len(
    data: &[u8],
    pending_len: usize,
) -> Option<(usize, usize)> {
    let mut state = pending_len;
    let mut complete = 0;
    for byte in data {
        if *byte != CURSOR_HOME[state] {
            return None;
        }
        state += 1;
        if state == CURSOR_HOME.len() {
            complete += 1;
            state = 0;
        }
    }
    Some((complete, state))
}

fn streaming_control_state(data: &[u8]) -> StreamingControlState {
    if STREAMING_CONTROL_PREFIXES
        .iter()
        .any(|prefix| data.len() < prefix.len() && prefix.starts_with(data))
    {
        return StreamingControlState::Incomplete;
    }

    if data.starts_with(b"\x1bPtmux;") {
        return find_tmux_passthrough_end(data)
            .map(StreamingControlState::Complete)
            .unwrap_or(StreamingControlState::Incomplete);
    }
    if data.starts_with(b"\x1b_G") {
        return find_osc_terminator(&data[3..])
            .map(|(payload_len, terminator_len)| {
                StreamingControlState::Complete(3 + payload_len + terminator_len)
            })
            .unwrap_or(StreamingControlState::Incomplete);
    }
    if data.starts_with(b"\x1b]") {
        return match osc_streaming_prefix_state(&data[2..]) {
            StreamingControlState::Complete(_) => find_osc_terminator(&data[2..])
                .map(|(payload_len, terminator_len)| {
                    StreamingControlState::Complete(2 + payload_len + terminator_len)
                })
                .unwrap_or(StreamingControlState::Incomplete),
            state => state,
        };
    }

    StreamingControlState::Unrecognized
}

fn osc_streaming_prefix_state(data: &[u8]) -> StreamingControlState {
    if data.starts_with(b"7;")
        || SIDE_EFFECT_OSC_PREFIXES
            .iter()
            .any(|prefix| data.starts_with(prefix))
        || COLOR_OSC_PREFIXES
            .iter()
            .any(|prefix| data.starts_with(prefix))
    {
        return StreamingControlState::Complete(0);
    }
    if SIDE_EFFECT_OSC_PREFIXES
        .iter()
        .copied()
        .chain(COLOR_OSC_PREFIXES.iter().copied())
        .chain(std::iter::once(b"7;".as_slice()))
        .any(|prefix| data.len() < prefix.len() && prefix.starts_with(data))
    {
        return StreamingControlState::Incomplete;
    }
    StreamingControlState::Unrecognized
}

fn find_tmux_passthrough_end(data: &[u8]) -> Option<usize> {
    let mut cursor = 7;
    while cursor < data.len() {
        if data[cursor] == 0x1b && data.get(cursor + 1) == Some(&0x1b) {
            cursor += 2;
            continue;
        }
        if data[cursor] == 0x1b && data.get(cursor + 1) == Some(&b'\\') {
            return Some(cursor + 2);
        }
        cursor += 1;
    }
    None
}

pub(super) fn terminal_write_features(data: &[u8]) -> TerminalWriteFeatures {
    let mut features = TerminalWriteFeatures::default();
    let mut index = 0;
    while let Some(relative_start) = data[index..].iter().position(|byte| *byte == 0x1b) {
        let start = index + relative_start;
        match data.get(start + 1).copied() {
            Some(b'P') if data.get(start + 2..start + 7) == Some(b"tmux;") => {
                features.tmux_passthrough = true;
            }
            Some(b'_') if data.get(start + 2) == Some(&b'G') => {
                features.kitty_graphics = true;
            }
            Some(b']') if is_color_osc_prefix(data.get(start + 2..).unwrap_or_default()) => {
                features.osc_color = true;
            }
            Some(b']') if is_side_effect_osc_prefix(data.get(start + 2..).unwrap_or_default()) => {
                features.osc_side_effect = true;
            }
            _ => {}
        }
        if features.tmux_passthrough
            && features.kitty_graphics
            && features.osc_side_effect
            && features.osc_color
        {
            break;
        }
        index = start + 1;
    }
    features
}

pub(super) fn unwrap_tmux_passthrough_commands(data: &[u8]) -> Cow<'_, [u8]> {
    let mut out: Option<Vec<u8>> = None;
    let mut read_start = 0;
    while let Some(relative_start) = find_subslice(&data[read_start..], b"\x1bPtmux;") {
        let start = read_start + relative_start;
        let payload_start = start + 7;
        let mut cursor = payload_start;
        let mut payload_end = None;
        let mut has_escaped_escape = false;

        while cursor < data.len() {
            if data[cursor] == 0x1b && data.get(cursor + 1) == Some(&0x1b) {
                has_escaped_escape = true;
                cursor += 2;
                continue;
            }
            if data[cursor] == 0x1b && data.get(cursor + 1) == Some(&b'\\') {
                payload_end = Some(cursor);
                break;
            }
            cursor += 1;
        }

        let Some(payload_end) = payload_end else {
            read_start = payload_start;
            continue;
        };

        let out = out.get_or_insert_with(|| Vec::with_capacity(data.len()));
        out.extend_from_slice(&data[read_start..start]);
        if has_escaped_escape {
            let mut cursor = payload_start;
            while cursor < payload_end {
                if data[cursor] == 0x1b && data.get(cursor + 1) == Some(&0x1b) {
                    out.push(0x1b);
                    cursor += 2;
                } else {
                    out.push(data[cursor]);
                    cursor += 1;
                }
            }
        } else {
            out.extend_from_slice(&data[payload_start..payload_end]);
        }
        read_start = payload_end + 2;
    }

    match out {
        Some(mut out) => {
            out.extend_from_slice(&data[read_start..]);
            Cow::Owned(out)
        }
        None => Cow::Borrowed(data),
    }
}

pub(super) struct SanitizedKittyGraphics<'a> {
    pub(super) bytes: Cow<'a, [u8]>,
    pub(super) touched: bool,
}

pub(super) fn sanitize_kitty_graphics_commands(data: &[u8]) -> SanitizedKittyGraphics<'_> {
    let mut out: Option<Vec<u8>> = None;
    let mut read_start = 0;
    let mut touched = false;
    while let Some(relative_start) = find_subslice(&data[read_start..], b"\x1b_G") {
        touched = true;
        let start = read_start + relative_start;
        let payload_start = start + 3;
        let Some((payload_len, terminator_len)) = find_osc_terminator(&data[payload_start..])
        else {
            read_start = payload_start;
            continue;
        };
        let payload_end = payload_start + payload_len;
        let payload = &data[payload_start..payload_end];
        let control_end = payload
            .iter()
            .position(|byte| *byte == b';')
            .unwrap_or(payload.len());
        let control = &payload[..control_end];
        let Some(sanitized_control) = sanitize_kitty_graphics_control(control) else {
            read_start = payload_end + terminator_len;
            continue;
        };

        let out = out.get_or_insert_with(|| Vec::with_capacity(data.len()));
        out.extend_from_slice(&data[read_start..payload_start]);
        out.extend_from_slice(&sanitized_control);
        out.extend_from_slice(&payload[control_end..payload.len()]);
        out.extend_from_slice(&data[payload_end..payload_end + terminator_len]);
        read_start = payload_end + terminator_len;
    }

    match out {
        Some(mut out) => {
            out.extend_from_slice(&data[read_start..]);
            SanitizedKittyGraphics {
                bytes: Cow::Owned(out),
                touched,
            }
        }
        None => SanitizedKittyGraphics {
            bytes: Cow::Borrowed(data),
            touched,
        },
    }
}

fn sanitize_kitty_graphics_control(control: &[u8]) -> Option<Vec<u8>> {
    let mut changed = false;
    for field in control.split(|byte| *byte == b',') {
        let Some(separator) = field.iter().position(|byte| *byte == b'=') else {
            continue;
        };
        let key = &field[..separator];
        let value = &field[separator + 1..];
        if key.len() != 1 || value.len() > 11 {
            changed = true;
            break;
        }
    }
    if !changed {
        return None;
    }

    let mut sanitized = Vec::with_capacity(control.len());
    for field in control.split(|byte| *byte == b',') {
        let Some(separator) = field.iter().position(|byte| *byte == b'=') else {
            append_kitty_graphics_field(&mut sanitized, field);
            continue;
        };
        let key = &field[..separator];
        let value = &field[separator + 1..];
        if key.len() == 1 && value.len() <= 11 {
            append_kitty_graphics_field(&mut sanitized, field);
        }
    }
    Some(sanitized)
}

fn append_kitty_graphics_field(out: &mut Vec<u8>, field: &[u8]) {
    if !out.is_empty() {
        out.push(b',');
    }
    out.extend_from_slice(field);
}

fn is_side_effect_osc_prefix(data: &[u8]) -> bool {
    data.starts_with(b"1;")
        || data.starts_with(b"9;")
        || data.starts_with(b"22;")
        || data.starts_with(b"52;")
        || data.starts_with(b"66;")
        || data.starts_with(b"133;")
        || data.starts_with(b"777;")
        || data.starts_with(b"1337;")
}

fn is_color_osc_prefix(data: &[u8]) -> bool {
    COLOR_OSC_PREFIXES
        .iter()
        .any(|prefix| data.starts_with(prefix))
}
