mod read;
mod write;

use crate::utils::ResponseExt;
use anyhow::{bail, Context};
use hyper_old_types::header::{Link, RelationType};
use log::{debug, trace};
use reqwest::header::HeaderMap;
use reqwest::{
    blocking::{Client, RequestBuilder, Response},
    header::{self, HeaderValue},
    Method, StatusCode,
};
use serde::{de::DeserializeOwned, Deserialize};
use std::borrow::Cow;
use std::fmt;

pub(crate) use read::{GitHubApiRead, GithubRead};
pub(crate) use write::GitHubWrite;

#[derive(Clone)]
pub(crate) struct HttpClient {
    client: Client,
    base_url: String,
}

impl HttpClient {
    pub(crate) fn from_url_and_token(mut base_url: String, token: String) -> anyhow::Result<Self> {
        let mut builder = reqwest::blocking::ClientBuilder::default();
        let mut map = HeaderMap::default();
        let mut auth = HeaderValue::from_str(&format!("token {}", token))?;
        auth.set_sensitive(true);

        map.insert(header::AUTHORIZATION, auth);
        map.insert(
            header::USER_AGENT,
            HeaderValue::from_static(crate::USER_AGENT),
        );
        builder = builder.default_headers(map);

        if !base_url.ends_with('/') {
            base_url.push('/');
        }

        Ok(Self {
            client: builder.build()?,
            base_url,
        })
    }

    fn req(&self, method: Method, url: &str) -> anyhow::Result<RequestBuilder> {
        let url = if url.starts_with("https://") {
            Cow::Borrowed(url)
        } else {
            Cow::Owned(format!("{}{url}", self.base_url))
        };
        trace!("http request: {} {}", method, url);
        Ok(self.client.request(method, url.as_ref()))
    }

    fn send<T: serde::Serialize + std::fmt::Debug>(
        &self,
        method: Method,
        url: &str,
        body: &T,
    ) -> Result<Response, anyhow::Error> {
        let resp = self.req(method, url)?.json(body).send()?;
        resp.custom_error_for_status()
    }

    fn send_option<T: DeserializeOwned>(
        &self,
        method: Method,
        url: &str,
    ) -> Result<Option<T>, anyhow::Error> {
        let resp = self.req(method.clone(), url)?.send()?;
        match resp.status() {
            StatusCode::OK => Ok(Some(resp.json_annotated().with_context(|| {
                format!("Failed to decode response body on {method} request to '{url}'")
            })?)),
            StatusCode::NOT_FOUND => Ok(None),
            _ => Err(resp.custom_error_for_status().unwrap_err()),
        }
    }

    fn graphql<R, V>(&self, query: &str, variables: V) -> anyhow::Result<R>
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
            .req(Method::POST, "graphql")?
            .json(&Request { query, variables })
            .send()?
            .custom_error_for_status()?;

        let res: GraphResult<R> = resp.json_annotated().with_context(|| {
            format!("Failed to decode response body on graphql request with query '{query}'")
        })?;
        if let Some(error) = res.errors.first() {
            bail!("graphql error: {}", error.message);
        } else if let Some(data) = res.data {
            Ok(data)
        } else {
            bail!("missing graphql data");
        }
    }

    fn rest_paginated<F, T>(&self, method: &Method, url: String, mut f: F) -> anyhow::Result<()>
    where
        F: FnMut(Vec<T>) -> anyhow::Result<()>,
        T: DeserializeOwned,
    {
        let mut next = Some(url);
        while let Some(next_url) = next.take() {
            let resp = self
                .req(method.clone(), &next_url)?
                .send()?
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
                        next = Some(link.link().to_string());
                        break;
                    }
                }
            }

            f(resp.json().with_context(|| {
                format!("Failed to deserialize response body for {method} request to '{next_url}'")
            })?)?;
        }
        Ok(())
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
    message: String,
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

#[derive(serde::Deserialize, Debug)]
pub(crate) struct RepoTeam {
    pub(crate) name: String,
    pub(crate) permission: RepoPermission,
}

#[derive(serde::Deserialize)]
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

#[derive(serde::Deserialize, Debug)]
pub(crate) struct Repo {
    #[serde(rename = "node_id")]
    pub(crate) id: String,
    pub(crate) name: String,
    #[serde(alias = "owner", deserialize_with = "repo_owner")]
    pub(crate) org: String,
    pub(crate) description: Option<String>,
    pub(crate) homepage: Option<String>,
    pub(crate) archived: bool,
}

fn repo_owner<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::de::Deserializer<'de>,
{
    let owner = Login::deserialize(deserializer)?;
    Ok(owner.login)
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
    base64::encode(format!("04:User{id}"))
}

fn team_node_id(id: u64) -> String {
    base64::encode(format!("04:Team{id}"))
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

#[derive(PartialEq)]
pub(crate) struct RepoSettings {
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub archived: bool,
}
