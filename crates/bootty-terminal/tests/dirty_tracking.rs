//! Guards how `extract_frame` reports per-frame dirtiness.
//!
//! libghostty's render-state dirty tracking is caller-managed: `update` does not
//! unset dirty state, so `extract_frame` must clear the per-row flags
//! (`row.set_dirty(false)`) and the global flag (`snapshot.set_dirty(Clean)`)
//! each frame. With that reset in place a localized edit reports only the
//! affected rows as dirty, which lets the incremental-extraction path skip
//! unchanged rows. If the reset regresses, every edit dirties the whole screen.

use bootty_terminal::geometry::TerminalGeometry;
use bootty_terminal::terminal::TerminalEngine;
use pretty_assertions::assert_eq;

fn engine(cols: u16, rows: u16) -> TerminalEngine {
    TerminalEngine::new(TerminalGeometry {
        cols,
        rows,
        cell_width: 9,
        cell_height: 22,
    })
    .expect("engine")
}

#[test]
fn localized_edit_reports_partial_dirtiness() {
    let mut engine = engine(120, 40);
    for row in 0..40 {
        engine.write_vt(format!("\x1b[{};1Hrow {row:03}", row + 1).as_bytes());
    }
    // Consume the initial full frame so the next extract reflects only the edit.
    engine.extract_frame().expect("frame");

    // Change one cell on one row (the cursor move also redraws its old/new row).
    engine.write_vt(b"\x1b[20;1Hx");
    let frame = engine.extract_frame().expect("frame");

    assert!(
        frame.stats.dirty_rows > 0,
        "the edit must dirty at least one row"
    );
    assert!(
        frame.stats.dirty_rows < 40,
        "a localized edit must not dirty the whole screen; got {}",
        frame.stats.dirty_rows
    );
    assert!(
        frame.row_dirty.iter().any(|dirty| !*dirty),
        "rows untouched by the edit must stay clean"
    );
}

#[test]
fn no_op_extract_reports_no_dirty_rows() {
    let mut engine = engine(120, 40);
    engine.write_vt(b"hello");
    engine.extract_frame().expect("frame");

    let frame = engine.extract_frame().expect("frame");
    assert_eq!(frame.stats.dirty_rows, 0);
    assert!(frame.row_dirty.iter().all(|dirty| !*dirty));
}
