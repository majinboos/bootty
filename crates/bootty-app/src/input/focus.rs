#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputFocus {
    #[default]
    Terminal,
    Sidebar,
    Picker,
    Dialog,
}

impl InputFocus {
    pub fn terminal_owns_input(self) -> bool {
        matches!(self, Self::Terminal)
    }
}
