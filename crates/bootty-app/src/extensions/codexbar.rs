pub(super) fn validate_provider(provider: &str) -> std::io::Result<()> {
    let valid = !provider.is_empty()
        && provider
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if valid {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid codexbar provider",
        ))
    }
}

pub(super) fn reject_reserved_shell_command(cmd: &str) -> std::io::Result<()> {
    if command_invokes_usage(cmd) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "bootty.run cannot invoke codexbar usage; use bootty.codexbar_usage(provider)",
        ));
    }
    Ok(())
}

fn command_invokes_usage(cmd: &str) -> bool {
    let tokens = shellish_tokens(cmd);
    let mut command_start = true;
    let mut previous_command_is_codexbar = false;
    for (index, token) in tokens.iter().enumerate() {
        if index > 0 && contains_shell_command_separator(&cmd[tokens[index - 1].1..token.0]) {
            command_start = true;
            previous_command_is_codexbar = false;
        }
        let token = token.2;
        if previous_command_is_codexbar && token == "usage" {
            return true;
        }
        previous_command_is_codexbar = command_start
            && token
                .rsplit('/')
                .next()
                .is_some_and(|name| name == "codexbar");
        if command_start && is_shell_assignment(token) {
            continue;
        }
        command_start = false;
    }
    false
}

fn shellish_tokens(cmd: &str) -> Vec<(usize, usize, &str)> {
    let mut tokens = Vec::new();
    let mut start = None;
    for (index, ch) in cmd.char_indices() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '/' | '.' | '=') {
            start.get_or_insert(index);
        } else if let Some(token_start) = start.take() {
            tokens.push((token_start, index, &cmd[token_start..index]));
        }
    }
    if let Some(token_start) = start {
        tokens.push((token_start, cmd.len(), &cmd[token_start..]));
    }
    tokens
}

fn contains_shell_command_separator(value: &str) -> bool {
    value
        .chars()
        .any(|ch| matches!(ch, ';' | '&' | '|' | '\n' | '\r' | '(' | '`'))
}

fn is_shell_assignment(token: &str) -> bool {
    token
        .split_once('=')
        .is_some_and(|(name, _)| !name.is_empty() && !name.contains('/'))
}

#[cfg(target_os = "macos")]
pub(super) fn resolve_program() -> std::io::Result<String> {
    bootty_mux::process::resolve_program("codexbar").map_err(std::io::Error::other)
}

#[cfg(not(target_os = "macos"))]
pub(super) fn resolve_program() -> std::io::Result<String> {
    Ok("codexbar".to_owned())
}
