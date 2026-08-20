use bootty_mux::controller::SpaceId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceSummary {
    pub id: SpaceId,
    pub name: String,
    pub icon: String,
    pub color: [u8; 3],
    pub tint_sidebar: bool,
    pub active: bool,
    pub error: Option<String>,
}
