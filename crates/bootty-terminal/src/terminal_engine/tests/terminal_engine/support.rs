use super::super::super::*;

pub(super) fn row_text(frame: &RenderFrame, row: u16) -> String {
    frame
        .cells
        .iter()
        .filter(|cell| cell.y == row)
        .flat_map(|cell| frame.cell_text(cell).iter().copied())
        .collect()
}

pub(crate) fn test_terminal_engine() -> Result<TerminalEngine> {
    TerminalEngine::new(TerminalGeometry {
        cols: 80,
        rows: 24,
        cell_width: 10,
        cell_height: 20,
    })
}

pub(super) fn captured_pty_engine() -> Result<(TerminalEngine, Arc<Mutex<Vec<u8>>>)> {
    let mut engine = test_terminal_engine()?;
    let output = Arc::new(Mutex::new(Vec::new()));
    let capture = output.clone();
    engine.on_pty_write(move |_terminal, bytes| {
        capture
            .lock()
            .expect("pty output lock")
            .extend_from_slice(bytes);
    })?;
    Ok((engine, output))
}

pub(super) fn take_pty_output(output: &Arc<Mutex<Vec<u8>>>) -> Vec<u8> {
    std::mem::take(&mut *output.lock().expect("pty output lock"))
}
