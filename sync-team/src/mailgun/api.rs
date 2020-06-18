#[derive(serde::Deserialize)]
pub struct ListResponse {
    pub items: Vec<List>,
    pub paging: Paging,
}

#[derive(serde::Deserialize)]
pub struct RoutesResponse {
    pub items: Vec<Route>,
    pub total_count: usize,
}
#[derive(serde::Deserialize)]
pub struct Route {
    pub actions: Vec<String>,
    pub expression: String,
    pub id: String,
    pub priority: i32,
    pub description: serde_json::Value,
}

#[derive(serde::Deserialize)]
pub struct List {
    pub access_level: String,
    pub address: String,
    pub members_count: u64,
}

#[derive(serde::Deserialize)]
pub struct Paging {
    pub first: String,
    pub last: String,
    pub next: String,
    pub previous: String,
}

#[derive(serde::Deserialize)]
pub struct MembersResponse {
    pub items: Vec<Member>,
    pub paging: Paging,
}

#[derive(serde::Deserialize)]
pub struct Member {
    pub address: String,
}

#[derive(serde::Deserialize)]
pub struct Empty {}
