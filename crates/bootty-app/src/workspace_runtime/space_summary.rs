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
    /// Whether a session in the active Space can move here, which only holds within one
    /// multiplexer. The switcher greys the rest rather than hiding them.
    pub accepts_moves: bool,
}
