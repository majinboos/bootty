use std::io;

pub use bootty_mux::project::ProjectPickerEntry;

pub fn discover_project_picker_entries() -> Vec<ProjectPickerEntry> {
    let home = bootty_mux::project::home_dir();
    bootty_mux::project::discover_project_picker_entries(home.as_deref())
}

pub fn toggle_favorite_project_path(project_path: &str) -> io::Result<bool> {
    let home = bootty_mux::project::home_dir();
    bootty_mux::project::toggle_favorite_project_path(home.as_deref(), project_path)
}
