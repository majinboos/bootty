pub use bootty_mux::project::WorktreePickerEntry;

pub fn discover_worktree_picker_entries(project_path: &str) -> Vec<WorktreePickerEntry> {
    bootty_mux::project::discover_worktree_picker_entries(project_path)
}
