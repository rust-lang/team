#[derive(serde::Deserialize)]
pub(super) struct RoutesResponse {
    pub(super) items: Vec<Route>,
    pub(super) total_count: usize,
}
#[derive(serde::Deserialize)]
pub(super) struct Route {
    pub(super) actions: Vec<String>,
    pub(super) expression: String,
    pub(super) id: String,
    pub(super) priority: i32,
    pub(super) description: serde_json::Value,
}

#[derive(serde::Deserialize)]
pub(super) struct Empty {}
