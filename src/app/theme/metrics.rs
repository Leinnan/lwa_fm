//! Fluent sizing tokens; cards use the requested 8px radius.
#[derive(Clone, Copy)]
pub struct Metrics {
    pub control_radius: u8,
    pub card_radius: u8,
    pub overlay_radius: u8,
    pub control_height: f32,
    pub nav_row_height: f32,
}
pub const FLUENT_METRICS: Metrics = Metrics {
    control_radius: 4,
    card_radius: 8,
    overlay_radius: 8,
    control_height: 32.0,
    nav_row_height: 36.0,
};
