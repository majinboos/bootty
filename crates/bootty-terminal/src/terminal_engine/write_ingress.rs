use std::borrow::Cow;

use memchr::{memchr, memchr_iter, memchr2_iter, memchr3_iter, memmem::find};

pub(crate) fn find_osc_terminator(bytes: &[u8]) -> Option<(usize, usize)> {
    for index in memchr2_iter(0x07, 0x1b, bytes) {
        match bytes[index] {
            0x07 => return Some((index, 1)),
            0x1b if bytes.get(index + 1) == Some(&b'\\') => return Some((index, 2)),
            _ => {}
        }
    }
    None
}

pub(crate) fn split_osc_payload(payload: &[u8]) -> Option<(&[u8], &[u8])> {
    let separator = memchr(b';', payload)?;
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
    styles: u8,
    scratch: Vec<u8>,
}

impl SgrOptimizer {
    pub(super) fn reset(&mut self) {
        self.styles = 0;
        self.scratch.clear();
    }

    pub(super) fn optimize<'a>(&'a mut self, data: &'a [u8]) -> &'a [u8] {
        let mut cursor = 0;
        let mut changed = false;
        self.scratch.clear();

        while let Some(relative_start) = find(&data[cursor..], b"\x1b[") {
            let start = cursor + relative_start;
            let params_start = start + 2;
            let Some(relative_end) = memchr(b'm', &data[params_start..]) else {
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
        let optimized = (self.styles == 0b111)
            .then_some(params)
            .and_then(redundant_style_suffix_prefix);
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
                b"1" => self.styles |= 0b001,
                b"3" => self.styles |= 0b010,
                b"4" => self.styles |= 0b100,
                b"22" => self.styles &= !0b001,
                b"23" => self.styles &= !0b010,
                b"24" => self.styles &= !0b100,
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
            part if basic_color_sgr_param(part) => {}
            _ => return false,
        }
    }
    true
}

fn basic_color_sgr_param(param: &[u8]) -> bool {
    matches!(
        param,
        [b'3' | b'4' | b'9', b'0'..=b'7'] | [b'3' | b'4', b'9'] | [b'1', b'0', b'0'..=b'7']
    )
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
        return find_tmux_passthrough(data)
            .map(|(len, _)| StreamingControlState::Complete(len))
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
    let mut incomplete = false;
    for prefix in SIDE_EFFECT_OSC_PREFIXES
        .iter()
        .copied()
        .chain(COLOR_OSC_PREFIXES.iter().copied())
        .chain(std::iter::once(b"7;".as_slice()))
    {
        if data.starts_with(prefix) {
            return StreamingControlState::Complete(0);
        }
        incomplete |= data.len() < prefix.len() && prefix.starts_with(data);
    }
    if incomplete {
        StreamingControlState::Incomplete
    } else {
        StreamingControlState::Unrecognized
    }
}

fn find_tmux_passthrough(data: &[u8]) -> Option<(usize, bool)> {
    let mut cursor = 7;
    let mut has_escaped_escape = false;
    while let Some(relative_escape) = memchr(0x1b, &data[cursor..]) {
        cursor += relative_escape;
        match data.get(cursor + 1) {
            Some(&0x1b) => {
                has_escaped_escape = true;
                cursor += 2;
            }
            Some(&b'\\') => return Some((cursor + 2, has_escaped_escape)),
            _ => cursor += 1,
        }
    }
    None
}

pub(super) fn terminal_write_features(data: &[u8]) -> TerminalWriteFeatures {
    let mut features = TerminalWriteFeatures::default();
    for start in memchr_iter(0x1b, data) {
        match data.get(start + 1).copied() {
            Some(b'P') if data.get(start + 2..start + 7) == Some(b"tmux;") => {
                features.tmux_passthrough = true;
            }
            Some(b'_') if data.get(start + 2) == Some(&b'G') => {
                features.kitty_graphics = true;
            }
            Some(b']') => {
                let osc = data.get(start + 2..).unwrap_or_default();
                if has_osc_prefix(osc, COLOR_OSC_PREFIXES) {
                    features.osc_color = true;
                } else if has_osc_prefix(osc, SIDE_EFFECT_OSC_PREFIXES) {
                    features.osc_side_effect = true;
                }
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
    }
    features
}

pub(super) fn unwrap_tmux_passthrough_commands(data: &[u8]) -> Cow<'_, [u8]> {
    let mut out: Option<Vec<u8>> = None;
    let mut read_start = 0;
    while let Some(relative_start) = find(&data[read_start..], b"\x1bPtmux;") {
        let start = read_start + relative_start;
        let payload_start = start + 7;
        let Some((control_len, has_escaped_escape)) = find_tmux_passthrough(&data[start..]) else {
            read_start = payload_start;
            continue;
        };
        let payload_end = start + control_len - 2;

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
    while let Some(relative_start) = find(&data[read_start..], b"\x1b_G") {
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
        let control_end = memchr(b';', payload).unwrap_or(payload.len());
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
    if control
        .split(|byte| *byte == b',')
        .all(valid_kitty_graphics_field)
    {
        return None;
    }

    let mut sanitized = Vec::with_capacity(control.len());
    for (index, field) in control
        .split(|byte| *byte == b',')
        .filter(|field| valid_kitty_graphics_field(field))
        .enumerate()
    {
        if index > 0 {
            sanitized.push(b',');
        }
        sanitized.extend_from_slice(field);
    }
    Some(sanitized)
}

fn valid_kitty_graphics_field(field: &[u8]) -> bool {
    memchr(b'=', field).is_none_or(|separator| separator == 1 && field.len() - separator - 1 <= 11)
}

fn has_osc_prefix(data: &[u8], prefixes: &[&[u8]]) -> bool {
    prefixes.iter().any(|prefix| data.starts_with(prefix))
}
