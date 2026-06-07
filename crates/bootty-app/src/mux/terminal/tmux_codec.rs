use std::{
    io::Write,
    process::ChildStdin,
    sync::{Arc, Mutex},
};

use anyhow::Result;

use super::pane::MuxPaneTarget;

pub(super) fn target_input_selector(target: &MuxPaneTarget) -> &str {
    target.input_selector()
}

pub(super) fn send_tmux_hex_input(
    stdin: &Arc<Mutex<ChildStdin>>,
    target: &str,
    bytes: &[u8],
) -> Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    let mut command = format!("send-keys -H -t {target}");
    for byte in bytes {
        command.push(' ');
        command.push_str(&format!("{byte:02x}"));
    }
    let mut stdin = stdin
        .lock()
        .map_err(|_| anyhow::anyhow!("native mux control stdin lock poisoned"))?;
    writeln!(stdin, "{command}")?;
    stdin.flush()?;
    Ok(())
}

pub(super) fn decode_tmux_control_output(input: &str) -> Vec<u8> {
    let mut output = Vec::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            let mut lookahead = chars.clone();
            let digits = [lookahead.next(), lookahead.next(), lookahead.next()];
            if let Some(value) = parse_octal_escape(digits) {
                output.push(value);
                for _ in 0..3 {
                    chars.next();
                }
                continue;
            }
        }
        append_tmux_control_char(&mut output, ch);
    }
    output
}

fn append_tmux_control_char(output: &mut Vec<u8>, ch: char) {
    let value = ch as u32;
    if let Ok(byte) = u8::try_from(value) {
        output.push(byte);
    } else {
        let mut bytes = [0; 4];
        output.extend_from_slice(ch.encode_utf8(&mut bytes).as_bytes());
    }
}

fn parse_octal_escape(digits: [Option<char>; 3]) -> Option<u8> {
    let mut value = 0_u8;
    for digit in digits {
        let digit = digit?;
        if !matches!(digit, '0'..='7') {
            return None;
        }
        value = value * 8 + digit as u8 - b'0';
    }
    Some(value)
}

#[derive(Default)]
pub(super) struct TmuxPassthroughDecoder {
    pending: Vec<u8>,
}

impl TmuxPassthroughDecoder {
    pub(super) fn push(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.pending.extend_from_slice(bytes);
        let mut out = Vec::with_capacity(self.pending.len());
        let mut read_start = 0;

        while let Some(relative_start) =
            find_subslice_bytes(&self.pending[read_start..], b"\x1bPtmux;")
        {
            let start = read_start + relative_start;
            out.extend_from_slice(&self.pending[read_start..start]);
            let payload_start = start + 7;
            let Some((payload, end)) =
                decode_complete_tmux_passthrough(&self.pending[payload_start..])
            else {
                self.pending.drain(..start);
                return out;
            };
            out.extend_from_slice(&payload);
            read_start = payload_start + end;
        }

        let keep_from = trailing_prefix_start(&self.pending[read_start..], b"\x1bPtmux;")
            .map_or(self.pending.len(), |relative| read_start + relative);
        out.extend_from_slice(&self.pending[read_start..keep_from]);
        self.pending.drain(..keep_from);
        out
    }
}

fn decode_complete_tmux_passthrough(bytes: &[u8]) -> Option<(Vec<u8>, usize)> {
    let mut payload = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] == 0x1b && bytes.get(cursor + 1) == Some(&0x1b) {
            payload.push(0x1b);
            cursor += 2;
            continue;
        }
        if bytes[cursor] == 0x1b && bytes.get(cursor + 1) == Some(&b'\\') {
            return Some((payload, cursor + 2));
        }
        payload.push(bytes[cursor]);
        cursor += 1;
    }
    None
}

fn find_subslice_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn trailing_prefix_start(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    let max_len = haystack.len().min(needle.len().saturating_sub(1));
    (1..=max_len)
        .rev()
        .find(|len| haystack[haystack.len() - len..] == needle[..*len])
        .map(|len| haystack.len() - len)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(session_id: &str) -> MuxPaneTarget {
        MuxPaneTarget::Session {
            session_id: session_id.to_owned(),
            cwd: None,
        }
    }

    #[test]
    fn tmux_control_output_decodes_octal_escapes() {
        assert_eq!(
            decode_tmux_control_output(r"A\033[31mB\015\012"),
            b"A\x1b[31mB\r\n"
        );
    }

    #[test]
    fn tmux_control_output_recovers_non_ascii_bytes_from_parser_chars() {
        let placeholder = "\u{00f4}\u{008e}\u{00bb}\u{00ae}";

        assert_eq!(
            decode_tmux_control_output(placeholder),
            "\u{10eeee}".as_bytes()
        );
    }

    #[test]
    fn tmux_passthrough_decoder_handles_split_wrapped_kitty_commands() {
        let mut decoder = TmuxPassthroughDecoder::default();

        assert_eq!(decoder.push(b"before\x1bPtmux;\x1b\x1b_Ga=T"), b"before");
        assert_eq!(decoder.push(b";payload\x1b\x1b\\"), b"");
        assert_eq!(
            decoder.push(b"\x1b\\after"),
            b"\x1b_Ga=T;payload\x1b\\after"
        );
    }

    #[test]
    fn tmux_hex_input_uses_pane_target_when_available() {
        let target = MuxPaneTarget::Pane {
            session_id: "agents".to_owned(),
            pane_id: "%3".to_owned(),
            cwd: None,
        };

        assert_eq!(target_input_selector(&target), "%3");
    }

    #[test]
    fn target_input_selector_uses_session_when_no_pane_target_exists() {
        assert_eq!(target_input_selector(&target("agents")), "agents");
    }
}
