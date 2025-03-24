mod read;
mod tokens;
mod url;
mod write;

use crate::utils::ResponseExt;
use anyhow::{Context, bail};
use base64::Engine as _;
use base64::prelude::BASE64_STANDARD;
use hyper_old_types::header::{Link, RelationType};
use log::{debug, trace};
use reqwest::header::HeaderMap;
use reqwest::{
    Method, StatusCode,
    blocking::{Client, RequestBuilder, Response},
    header::{self, HeaderValue},
};
use secrecy::ExposeSecret;
use serde::{Deserialize, de::DeserializeOwned};
use std::fmt;
use tokens::GitHubTokens;
use url::GitHubUrl;

pub(crate) use read::{GitHubApiRead, GithubRead};
pub(crate) use write::GitHubWrite;

#[derive(Clone)]
pub(crate) struct HttpClient {
    client: Client,
    github_tokens: GitHubTokens,
}

impl HttpClient {
    pub(crate) fn new() -> anyhow::Result<Self> {
        let mut builder = reqwest::blocking::ClientBuilder::default();
        let mut map = HeaderMap::default();

        map.insert(
            header::USER_AGENT,
            HeaderValue::from_static(crate::USER_AGENT),
        );
        builder = builder.default_headers(map);

        Ok(Self {
            client: builder.build()?,
            github_tokens: GitHubTokens::from_env()?,
        })
    }

    fn auth_header(&self, org: &str) -> anyhow::Result<HeaderValue> {
        let token = self.github_tokens.get_token(org)?;
        let mut auth = HeaderValue::from_str(&format!("token {}", token.expose_secret()))?;
        auth.set_sensitive(true);
        Ok(auth)
    }

    fn req(&self, method: Method, url: &GitHubUrl) -> anyhow::Result<RequestBuilder> {
        trace!("http request: {} {}", method, url.url());
        let token = self.auth_header(url.org())?;
        let client = self
            .client
            .request(method, url.url())
            .header(header::AUTHORIZATION, token);
        Ok(client)
    }

    fn send<T: serde::Serialize + std::fmt::Debug>(
        &self,
        method: Method,
        url: &GitHubUrl,
        body: &T,
    ) -> Result<Response, anyhow::Error> {
        let resp = self.req(method, url)?.json(body).send()?;
        resp.custom_error_for_status()
    }

    fn send_option<T: DeserializeOwned>(
        &self,
        method: Method,
        url: &GitHubUrl,
    ) -> Result<Option<T>, anyhow::Error> {
        let resp = self.req(method.clone(), url)?.send()?;
        match resp.status() {
            StatusCode::OK => Ok(Some(resp.json_annotated().with_context(|| {
                format!(
                    "Failed to decode response body on {method} request to '{}'",
                    url.url()
                )
            })?)),
            StatusCode::NOT_FOUND => Ok(None),
            _ => Err(resp.custom_error_for_status().unwrap_err()),
        }
    }

    /// Send a request to the GitHub API and return the response.
    fn graphql<R, V>(&self, query: &str, variables: V, org: &str) -> anyhow::Result<R>
    where
        R: serde::de::DeserializeOwned,
        V: serde::Serialize,
    {
        let res = self.send_graphql_req(query, variables, org)?;

        if let Some(error) = res.errors.first() {
            bail!("graphql error: {}", error.message);
        }

        read_graphql_data(res)
    }

    /// Send a request to the GitHub API and return the response.
    /// If the request contains the error type `NOT_FOUND`, this method returns `Ok(None)`.
    fn graphql_opt<R, V>(&self, query: &str, variables: V, org: &str) -> anyhow::Result<Option<R>>
    where
        R: serde::de::DeserializeOwned,
        V: serde::Serialize,
    {
        let res = self.send_graphql_req(query, variables, org)?;

        if let Some(error) = res.errors.first() {
            if error.type_ == Some(GraphErrorType::NotFound) {
                return Ok(None);
            }
            bail!("graphql error: {}", error.message);
        }

        read_graphql_data(res)
    }

    fn send_graphql_req<R, V>(
        &self,
        query: &str,
        variables: V,
        org: &str,
    ) -> anyhow::Result<GraphResult<R>>
    where
        R: serde::de::DeserializeOwned,
        V: serde::Serialize,
    {
        #[derive(serde::Serialize)]
        struct Request<'a, V> {
            query: &'a str,
            variables: V,
        }
        let resp = self
            .req(Method::POST, &GitHubUrl::new("graphql", org))?
            .json(&Request { query, variables })
            .send()
            .context("failed to send graphql request")?
            .custom_error_for_status()?;

        resp.json_annotated().with_context(|| {
            format!("Failed to decode response body on graphql request with query '{query}'")
        })
    }

    fn rest_paginated<F, T>(&self, method: &Method, url: &GitHubUrl, mut f: F) -> anyhow::Result<()>
    where
        F: FnMut(T) -> anyhow::Result<()>,
        T: DeserializeOwned,
    {
        let mut next = Some(url.clone());
        while let Some(next_url) = next.take() {
            let resp = self
                .req(method.clone(), &next_url)?
                .send()
                .with_context(|| format!("failed to send request to {}", next_url.url()))?
                .custom_error_for_status()?;

            // Extract the next page
            if let Some(links) = resp.headers().get(header::LINK) {
                let links: Link = links.to_str()?.parse()?;
                for link in links.values() {
                    if link
                        .rel()
                        .map(|r| r.iter().any(|r| *r == RelationType::Next))
                        .unwrap_or(false)
                    {
                        next = Some(GitHubUrl::new(link.link(), next_url.org()));
                        break;
                    }
                }
            }

            f(resp.json_annotated().with_context(|| {
                format!(
                    "Failed to deserialize response body for {method} request to '{}'",
                    next_url.url()
                )
            })?)?;
        }
        Ok(())
    }
}

fn read_graphql_data<R>(res: GraphResult<R>) -> anyhow::Result<R>
where
    R: serde::de::DeserializeOwned,
{
    if let Some(data) = res.data {
        Ok(data)
    } else {
        bail!("missing graphql data");
    }
}

fn allow_not_found(resp: Response, method: Method, url: &str) -> Result<(), anyhow::Error> {
    match resp.status() {
        StatusCode::NOT_FOUND => {
            debug!("Response from {method} {url} returned 404 which is treated as success");
        }
        _ => {
            resp.custom_error_for_status()?;
        }
    }
    Ok(())
}

#[derive(Debug, serde::Deserialize)]
struct GraphResult<T> {
    data: Option<T>,
    #[serde(default)]
    errors: Vec<GraphError>,
}

#[derive(Debug, serde::Deserialize)]
struct GraphError {
    #[serde(rename = "type")]
    type_: Option<GraphErrorType>,
    message: String,
}

#[derive(Debug, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum GraphErrorType {
    NotFound,
    #[serde(other)]
    Other,
}

#[derive(serde::Deserialize)]
struct GraphNodes<T> {
    nodes: Vec<Option<T>>,
}

#[derive(serde::Deserialize)]
struct GraphNode<T> {
    node: Option<T>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphPageInfo {
    end_cursor: Option<String>,
    has_next_page: bool,
}

impl GraphPageInfo {
    fn start() -> Self {
        GraphPageInfo {
            end_cursor: None,
            has_next_page: true,
        }
    }
}

#[derive(serde::Deserialize, Debug, Clone)]
pub(crate) struct Team {
    /// The ID returned by the GitHub API can't be empty, but the None marks teams "created" during
    /// a dry run and not actually present on GitHub, so other methods can avoid acting on them.
    pub(crate) id: Option<u64>,
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    pub(crate) privacy: TeamPrivacy,
    /// The slug usually matches the name but can differ.
    /// For example, a team named rustup.rs would have a slug rustup-rs.
    pub(crate) slug: String,
}

#[derive(serde::Deserialize, Debug, Clone)]
pub(crate) struct RepoTeam {
    pub(crate) name: String,
    pub(crate) permission: RepoPermission,
}

#[derive(serde::Deserialize, Clone)]
pub(crate) struct RepoUser {
    #[serde(alias = "login")]
    pub(crate) name: String,
    #[serde(rename = "role_name")]
    pub(crate) permission: RepoPermission,
}

#[derive(Copy, Clone, serde::Serialize, serde::Deserialize, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RepoPermission {
    // While the GitHub UI uses the term 'write', the API still uses the older term 'push'
    #[serde(rename(serialize = "push"), alias = "push")]
    Write,
    Admin,
    Maintain,
    Triage,
    #[serde(alias = "pull")]
    Read,
}

impl fmt::Display for RepoPermission {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Write => write!(f, "write"),
            Self::Admin => write!(f, "admin"),
            Self::Maintain => write!(f, "maintain"),
            Self::Triage => write!(f, "triage"),
            Self::Read => write!(f, "read"),
        }
    }
}

#[derive(serde::Deserialize, Debug, Clone)]
pub(crate) struct Repo {
    pub(crate) node_id: String,
    pub(crate) name: String,
    #[serde(alias = "owner", deserialize_with = "repo_owner")]
    pub(crate) org: String,
    #[serde(deserialize_with = "repo_description")]
    pub(crate) description: String,
    pub(crate) homepage: Option<String>,
    pub(crate) archived: bool,
    #[serde(default)]
    pub(crate) allow_auto_merge: Option<bool>,
}

fn repo_owner<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::de::Deserializer<'de>,
{
    let owner = Login::deserialize(deserializer)?;
    Ok(owner.login)
}

/// We represent repository description with just a string,
/// to avoid two default states (`None` or `Some("")`) and to simplify code.
/// However, GitHub can return the description as `null`.
/// So using this function, we treat both an empty string and `null` as an
/// empty string.
fn repo_description<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::de::Deserializer<'de>,
{
    let description = <Option<String>>::deserialize(deserializer)?;
    Ok(description.unwrap_or_default())
}

/// An object with a `login` field
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
pub(crate) struct Login {
    pub(crate) login: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Eq, PartialEq, Copy, Clone)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TeamPrivacy {
    Closed,
    Secret,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Eq, PartialEq, Copy, Clone)]
#[serde(rename_all(serialize = "snake_case", deserialize = "SCREAMING_SNAKE_CASE"))]
pub(crate) enum TeamRole {
    Member,
    Maintainer,
}

impl fmt::Display for TeamRole {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            TeamRole::Member => write!(f, "member"),
            TeamRole::Maintainer => write!(f, "maintainer"),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TeamMember {
    pub(crate) username: String,
    pub(crate) role: TeamRole,
}

fn user_node_id(id: u64) -> String {
    BASE64_STANDARD.encode(format!("04:User{id}"))
}

fn team_node_id(id: u64) -> String {
    BASE64_STANDARD.encode(format!("04:Team{id}"))
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BranchProtection {
    pub(crate) pattern: String,
    pub(crate) is_admin_enforced: bool,
    pub(crate) dismisses_stale_reviews: bool,
    #[serde(default, deserialize_with = "nullable")]
    pub(crate) required_approving_review_count: u8,
    #[serde(default, deserialize_with = "nullable")]
    pub(crate) required_status_check_contexts: Vec<String>,
    #[serde(deserialize_with = "allowances")]
    pub(crate) push_allowances: Vec<PushAllowanceActor>,
    pub(crate) requires_approving_reviews: bool,
}

fn nullable<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::de::Deserializer<'de>,
    T: Default + DeserializeOwned,
{
    let opt = Option::deserialize(deserializer)?;
    Ok(opt.unwrap_or_default())
}

fn allowances<'de, D>(deserializer: D) -> Result<Vec<PushAllowanceActor>, D::Error>
where
    D: serde::de::Deserializer<'de>,
{
    #[derive(Deserialize)]
    struct Allowances {
        nodes: Vec<Actor>,
    }
    #[derive(Deserialize)]
    struct Actor {
        actor: PushAllowanceActor,
    }
    let allowances = Allowances::deserialize(deserializer)?;
    Ok(allowances.nodes.into_iter().map(|a| a.actor).collect())
}

/// Entities that can be allowed to push to a branch in a repo
#[derive(Clone, Deserialize, Debug, PartialEq, Eq)]
#[serde(untagged)]
pub(crate) enum PushAllowanceActor {
    User(UserPushAllowanceActor),
    Team(TeamPushAllowanceActor),
}

/// User who can be allowed to push to a branch in a repo
#[derive(Clone, Deserialize, Debug, PartialEq, Eq)]
pub(crate) struct UserPushAllowanceActor {
    pub(crate) login: String,
}

/// Team that can be allowed to push to a branch in a repo
#[derive(Clone, Deserialize, Debug, PartialEq, Eq)]
pub(crate) struct TeamPushAllowanceActor {
    pub(crate) organization: Login,
    pub(crate) name: String,
}

pub(crate) enum BranchProtectionOp {
    CreateForRepo(String),
    UpdateBranchProtection(String),
}

#[derive(PartialEq, Debug)]
pub(crate) struct RepoSettings {
    pub description: String,
    pub homepage: Option<String>,
    pub archived: bool,
    pub auto_merge_enabled: bool,
}
