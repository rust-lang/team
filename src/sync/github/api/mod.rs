mod read;
mod tokens;
mod url;
mod write;

use crate::sync::utils::ResponseExt;
use anyhow::{Context, bail};
use base64::Engine as _;
use base64::prelude::BASE64_STANDARD;
use hyper_old_types::header::{Link, RelationType};
use log::{debug, trace};
use octocrab::models::RateLimit;
use reqwest::header::HeaderMap;
use reqwest::{
    Client, Method, RequestBuilder, Response, StatusCode,
    header::{self, HeaderValue},
};
use secrecy::ExposeSecret;
use serde::{Deserialize, de::DeserializeOwned};
use std::collections::BTreeSet;
use std::fmt;
use thiserror::Error;
use tokens::GitHubTokens;
use url::GitHubUrl;

use crate::sync::Config;
pub(crate) use read::{GitHubApiRead, GithubRead};
pub(crate) use write::GitHubWrite;

#[derive(Debug, Error)]
pub(crate) enum RestPaginatedError {
    #[error("{method} request to '{url}' failed with status {status}")]
    Http {
        method: Method,
        url: String,
        status: StatusCode,
        #[source]
        source: anyhow::Error,
    },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

#[derive(Clone)]
pub(crate) struct HttpClient {
    client: Client,
    github_tokens: GitHubTokens,
}

impl HttpClient {
    pub(crate) async fn new(config: &Config) -> anyhow::Result<Self> {
        let mut builder = reqwest::ClientBuilder::default();
        let mut map = HeaderMap::default();

        map.insert(
            header::USER_AGENT,
            HeaderValue::from_static(crate::USER_AGENT),
        );
        builder = builder.default_headers(map);

        Ok(Self {
            client: builder.build()?,
            github_tokens: GitHubTokens::from_env(config).await?,
        })
    }

    pub fn uses_pat(&self) -> bool {
        matches!(self.github_tokens, GitHubTokens::Pat(_))
    }

    pub(crate) async fn rate_limit(&self) -> anyhow::Result<RateLimit> {
        self.req(Method::GET, &GitHubUrl::new("rate_limit", "rust-lang"))?
            .send()
            .await?
            .json_annotated()
            .await
    }

    fn auth_header(&self, url: &GitHubUrl) -> anyhow::Result<HeaderValue> {
        let token = self
            .github_tokens
            .get_token_for_org(url.org(), url.token_type())?;
        let mut auth = HeaderValue::from_str(&format!("token {}", token.expose_secret()))?;
        auth.set_sensitive(true);
        Ok(auth)
    }

    fn req(&self, method: Method, url: &GitHubUrl) -> anyhow::Result<RequestBuilder> {
        trace!("http request: {} {}", method, url.url());
        let token = self.auth_header(url)?;
        let client = self
            .client
            .request(method, url.url())
            .header(header::AUTHORIZATION, token);
        Ok(client)
    }

    async fn send<T: serde::Serialize + std::fmt::Debug>(
        &self,
        method: Method,
        url: &GitHubUrl,
        body: &T,
    ) -> Result<Response, anyhow::Error> {
        let resp = self.req(method, url)?.json(body).send().await?;
        resp.custom_error_for_status().await
    }

    async fn send_option<T: DeserializeOwned>(
        &self,
        method: Method,
        url: &GitHubUrl,
    ) -> Result<Option<T>, anyhow::Error> {
        let resp = self.req(method.clone(), url)?.send().await?;
        match resp.status() {
            StatusCode::OK => Ok(Some(resp.json_annotated().await.with_context(|| {
                format!(
                    "Failed to decode response body on {method} request to '{}'",
                    url.url()
                )
            })?)),
            StatusCode::NOT_FOUND => Ok(None),
            _ => Err(resp.custom_error_for_status().await.unwrap_err()),
        }
    }

    /// Send a request to the GitHub API and return the response.
    async fn graphql<R, V>(&self, query: &str, variables: V, org: &str) -> anyhow::Result<R>
    where
        R: serde::de::DeserializeOwned,
        V: serde::Serialize,
    {
        let res = self.send_graphql_req(query, variables, org).await?;
        read_graphql_data(res)
    }

    /// Send a request to the GitHub API and return the response.
    ///
    /// If all the errors in the response are of type `NOT_FOUND` and refer to a top-level field
    /// of the query (e.g. `repository(owner: ..., name: ...)`), meaning that the queried resource
    /// doesn't exist or isn't visible to the token, this method returns `Ok(None)`.
    /// Any other error, including a `NOT_FOUND` error for a nested field, is returned as an error.
    ///
    /// This assumes that the query has a single top-level field. With multiple top-level fields,
    /// `Ok(None)` would be returned if any of them isn't found, discarding the data of the others.
    async fn graphql_opt<R, V>(
        &self,
        query: &str,
        variables: V,
        org: &str,
    ) -> anyhow::Result<Option<R>>
    where
        R: serde::de::DeserializeOwned,
        V: serde::Serialize,
    {
        let res = self.send_graphql_req(query, variables, org).await?;
        read_graphql_data_opt(res)
    }

    async fn send_graphql_req<R, V>(
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
            .await
            .context("failed to send graphql request")?
            .custom_error_for_status()
            .await?;

        resp.json_annotated().await.with_context(|| {
            format!("Failed to decode response body on graphql request with query '{query}'")
        })
    }

    async fn rest_paginated<F, T>(
        &self,
        method: &Method,
        url: &GitHubUrl,
        mut f: F,
    ) -> Result<(), RestPaginatedError>
    where
        F: FnMut(T) -> anyhow::Result<()>,
        T: DeserializeOwned,
    {
        let url = url.with_page_size(100);
        let mut next = Some(url.clone());
        while let Some(next_url) = next.take() {
            let resp = self
                .req(method.clone(), &next_url)?
                .send()
                .await
                .with_context(|| format!("failed to send request to {}", next_url.url()))?;

            let status = resp.status();
            let resp = resp.custom_error_for_status().await.map_err(|source| {
                RestPaginatedError::Http {
                    method: method.clone(),
                    url: next_url.url().to_string(),
                    status,
                    source,
                }
            })?;

            // Extract the next page
            if let Some(links) = resp.headers().get(header::LINK) {
                let links: Link = links
                    .to_str()
                    .context("failed to convert LINK header to string")?
                    .parse()
                    .context("failed to parse LINK header")?;
                for link in links.values() {
                    if link
                        .rel()
                        .map(|r| r.contains(&RelationType::Next))
                        .unwrap_or(false)
                    {
                        next = Some(next_url.clone().with_url(link.link()));
                        break;
                    }
                }
            }

            f(resp.json_annotated().await.with_context(|| {
                format!(
                    "Failed to deserialize response body for {method} request to '{}'",
                    next_url.url()
                )
            })?)?;
        }
        Ok(())
    }
}

fn read_graphql_data<R>(res: GraphResult<R>) -> anyhow::Result<R> {
    if let Some(error) = res.errors.first() {
        bail!("graphql error: {error}");
    }
    res.data.context("missing graphql data")
}

/// See [`HttpClient::graphql_opt`].
fn read_graphql_data_opt<R>(res: GraphResult<R>) -> anyhow::Result<Option<R>> {
    if let Some(error) = res.errors.iter().find(|error| !error.is_root_not_found()) {
        bail!("graphql error: {error}");
    }
    if !res.errors.is_empty() {
        // All the errors are `NOT_FOUND` errors for top-level fields.
        return Ok(None);
    }

    res.data.context("missing graphql data").map(Some)
}

async fn allow_not_found(resp: Response, method: Method, url: &str) -> Result<(), anyhow::Error> {
    match resp.status() {
        StatusCode::NOT_FOUND => {
            debug!("Response from {method} {url} returned 404 which is treated as success");
        }
        _ => {
            resp.custom_error_for_status().await?;
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
    /// Path of the field that produced the error, e.g. `["repository", "collaborators"]`.
    /// Segments are field names or list indices.
    /// `None` if the error isn't tied to a field (the key is missing or `null`).
    path: Option<Vec<serde_json::Value>>,
}

impl GraphError {
    /// Returns `true` if this is a `NOT_FOUND` error for a top-level field of the query,
    /// i.e. the queried resource itself (e.g. a repository) doesn't exist or isn't visible to the
    /// token.
    fn is_root_not_found(&self) -> bool {
        self.type_ == Some(GraphErrorType::NotFound) && matches!(self.path.as_deref(), Some([_]))
    }
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.message)?;
        // The path tells which field of the query failed, which the message alone often doesn't
        // (e.g. "Could not resolve to a node.").
        if let Some(path) = &self.path {
            // Printed as JSON, e.g. `["repository","collaborators","edges",0,"node"]`.
            let path = serde_json::to_string(path).map_err(|_| fmt::Error)?;
            write!(f, " (path: {path})")?;
        }
        Ok(())
    }
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

#[derive(serde::Deserialize, Debug, Clone)]
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
    Moderate,
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
            Self::Moderate => write!(f, "moderate"),
            Self::Read => write!(f, "read"),
        }
    }
}

#[derive(serde::Deserialize, Debug)]
pub(crate) struct OrgAppInstallation {
    #[serde(rename = "id")]
    pub(crate) installation_id: u64,
    pub(crate) app_id: u64,
}

#[derive(serde::Deserialize, Debug)]
pub(crate) struct RepoAppInstallation {
    pub(crate) name: String,
}

#[derive(serde::Deserialize, Debug, Clone)]
pub(crate) struct Repo {
    #[serde(rename = "id")]
    pub(crate) repo_id: u64,
    pub(crate) name: String,
    #[serde(alias = "owner", deserialize_with = "repo_owner")]
    pub(crate) org: String,
    #[serde(deserialize_with = "repo_description")]
    pub(crate) description: String,
    pub(crate) homepage: Option<String>,
    pub(crate) archived: bool,
    pub(crate) private: bool,
    #[serde(default)]
    pub(crate) allow_auto_merge: Option<bool>,
    // (pattern, branch protection ID)
    #[serde(default)]
    pub(crate) branch_protections: Vec<(String, String)>,
    #[serde(default)]
    pub(crate) collaborators: Vec<RepoUser>,
    /// IDs of rulesets defined directly on the repository (not inherited from the org).
    #[serde(default)]
    pub(crate) ruleset_ids: Vec<u64>,
    #[serde(default)]
    pub(crate) environments: Vec<RepoEnvironment>,
}

/// An environment of a repository.
#[derive(serde::Deserialize, Debug, Clone)]
pub(crate) struct RepoEnvironment {
    pub(crate) name: String,
    /// Whether the environment has deployment branch policies, which have to be loaded separately.
    pub(crate) has_branch_policies: bool,
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
#[derive(Deserialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
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

fn nullable<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::de::Deserializer<'de>,
    T: Default + DeserializeOwned,
{
    let opt = Option::deserialize(deserializer)?;
    Ok(opt.unwrap_or_default())
}

#[derive(PartialEq, Debug)]
pub(crate) struct RepoSettings {
    pub description: String,
    pub homepage: Option<String>,
    pub archived: bool,
    pub auto_merge_enabled: bool,
}

#[derive(Debug)]
pub(crate) struct RepoMetadata {
    pub name: String,
    pub has_pages: bool,
}

/// GitHub Repository Ruleset
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Ruleset {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) id: Option<i64>,
    pub(crate) name: String,
    pub(crate) target: RulesetTarget,
    pub(crate) source_type: RulesetSourceType,
    pub(crate) enforcement: RulesetEnforcement,
    // When we use a read-only token, GitHub does not return the `bypass_actors` field from the
    // get ruleset endpoint (https://docs.github.com/en/rest/repos/rules?apiVersion=2022-11-28#get-a-repository-ruleset).
    // Normalize that missing/null state to an empty vector immediately after deserialization.
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub(crate) bypass_actors: Vec<RulesetBypassActor>,
    pub(crate) conditions: RulesetConditions,
    pub(crate) rules: BTreeSet<RulesetRule>,
}

#[derive(
    Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub(crate) enum RulesetTarget {
    #[default]
    Branch,
    Tag,
}

#[derive(
    Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "PascalCase")]
pub(crate) enum RulesetSourceType {
    #[default]
    Repository,
    Organization,
}

#[derive(
    Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub(crate) enum RulesetEnforcement {
    #[default]
    Active,
    Disabled,
    Evaluate,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub(crate) struct RulesetBypassActor {
    pub(crate) actor_id: i64,
    pub(crate) actor_type: RulesetActorType,
    pub(crate) bypass_mode: RulesetBypassMode,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub(crate) enum RulesetActorType {
    Integration,
    OrganizationAdmin,
    RepositoryRole,
    Team,
    DeployKey,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RulesetBypassMode {
    Always,
    PullRequest,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct RulesetConditions {
    pub(crate) ref_name: RulesetRefNameCondition,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct RulesetRefNameCondition {
    pub(crate) include: Vec<String>,
    pub(crate) exclude: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum RulesetRule {
    Creation,
    Update,
    Deletion,
    RequiredLinearHistory,
    MergeQueue {
        parameters: MergeQueueParameters,
    },
    RequiredDeployments {
        parameters: RequiredDeploymentsParameters,
    },
    RequiredSignatures,
    PullRequest {
        parameters: PullRequestParameters,
    },
    RequiredStatusChecks {
        parameters: RequiredStatusChecksParameters,
    },
    NonFastForward,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub(crate) struct MergeQueueParameters {
    pub(crate) check_response_timeout_minutes: i32,
    pub(crate) grouping_strategy: MergeQueueGroupingStrategy,
    pub(crate) max_entries_to_build: i32,
    pub(crate) max_entries_to_merge: i32,
    pub(crate) merge_method: MergeQueueMergeMethod,
    pub(crate) min_entries_to_merge: i32,
    pub(crate) min_entries_to_merge_wait_minutes: i32,
}

pub(crate) const DEFAULT_MERGE_QUEUE_TIMEOUT_MINUTES: i32 = 60;
pub(crate) const DEFAULT_MERGE_QUEUE_MAX_ENTRIES_TO_BUILD: i32 = 5;
pub(crate) const DEFAULT_MERGE_QUEUE_MAX_ENTRIES_TO_MERGE: i32 = 5;
pub(crate) const DEFAULT_MERGE_QUEUE_MIN_ENTRIES_TO_MERGE: i32 = 1;
pub(crate) const DEFAULT_MERGE_QUEUE_MIN_ENTRIES_TO_MERGE_WAIT_MINUTES: i32 = 5;

#[derive(
    Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum MergeQueueGroupingStrategy {
    #[default]
    Allgreen,
    Headgreen,
}

#[derive(
    Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum MergeQueueMergeMethod {
    #[default]
    Merge,
    Squash,
    Rebase,
}

impl Default for MergeQueueParameters {
    fn default() -> Self {
        Self {
            check_response_timeout_minutes: DEFAULT_MERGE_QUEUE_TIMEOUT_MINUTES,
            grouping_strategy: MergeQueueGroupingStrategy::default(),
            max_entries_to_build: DEFAULT_MERGE_QUEUE_MAX_ENTRIES_TO_BUILD,
            max_entries_to_merge: DEFAULT_MERGE_QUEUE_MAX_ENTRIES_TO_MERGE,
            merge_method: MergeQueueMergeMethod::default(),
            min_entries_to_merge: DEFAULT_MERGE_QUEUE_MIN_ENTRIES_TO_MERGE,
            min_entries_to_merge_wait_minutes:
                DEFAULT_MERGE_QUEUE_MIN_ENTRIES_TO_MERGE_WAIT_MINUTES,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub(crate) struct RequiredDeploymentsParameters {
    pub(crate) required_deployment_environments: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub(crate) struct PullRequestParameters {
    pub(crate) dismiss_stale_reviews_on_push: bool,
    pub(crate) require_code_owner_review: bool,
    pub(crate) require_last_push_approval: bool,
    pub(crate) required_approving_review_count: i32,
    pub(crate) required_review_thread_resolution: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub(crate) struct RequiredStatusChecksParameters {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) do_not_enforce_on_create: Option<bool>,
    pub(crate) required_status_checks: Vec<RequiredStatusCheck>,
    /// Whether pull requests targeting a matching branch must be tested with the latest code.
    pub(crate) strict_required_status_checks_policy: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub(crate) struct RequiredStatusCheck {
    pub(crate) context: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) integration_id: Option<i64>,
}

pub(crate) enum RulesetOp {
    CreateForRepo,
    UpdateRuleset(i64),
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct BranchPolicy {
    pub(crate) id: u64,
    pub(crate) name: String,
    #[serde(rename = "type", default = "default_branch_policy_type")]
    pattern_type: String,
}

fn default_branch_policy_type() -> String {
    "branch".to_string()
}

/// A GitHub repository custom property.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct CustomPropertyValue {
    pub(crate) property_name: String,
    #[serde(serialize_with = "serialize_custom_property_value")]
    pub(crate) value: Option<rust_team_data::v1::CustomPropertyValue>,
}

fn serialize_custom_property_value<S>(
    value: &Option<rust_team_data::v1::CustomPropertyValue>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use rust_team_data::v1::CustomPropertyValue;

    match value {
        Some(CustomPropertyValue::String(value)) => serializer.serialize_some(value),
        Some(CustomPropertyValue::Bool(true)) => serializer.serialize_some("true"),
        Some(CustomPropertyValue::Bool(false)) => serializer.serialize_some("false"),
        Some(CustomPropertyValue::StringArray(value)) => serializer.serialize_some(value),
        None => serializer.serialize_none(),
    }
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct SetCustomPropertiesRequest {
    pub(crate) properties: Vec<CustomPropertyValue>,
}

#[cfg(test)]
mod tests {
    use super::{
        CustomPropertyValue, GraphResult, SetCustomPropertiesRequest, read_graphql_data,
        read_graphql_data_opt,
    };
    use serde_json::json;

    fn read_opt(response: serde_json::Value) -> anyhow::Result<Option<serde_json::Value>> {
        let res: GraphResult<serde_json::Value> = serde_json::from_value(response).unwrap();
        read_graphql_data_opt(res)
    }

    fn read_opt_err(response: serde_json::Value) -> String {
        read_opt(response).unwrap_err().to_string()
    }

    #[test]
    fn graphql_opt_returns_data_without_errors() {
        let data = json!({ "repository": { "databaseId": 1 } });
        let result = read_opt(json!({ "data": data })).unwrap();
        assert_eq!(result, Some(data));
    }

    #[test]
    fn graphql_opt_returns_none_when_root_field_is_not_found() {
        let result = read_opt(json!({
            "data": { "repository": null },
            "errors": [{
                "type": "NOT_FOUND",
                "path": ["repository"],
                "locations": [{ "line": 3, "column": 17 }],
                "message": "Could not resolve to a Repository with the name 'rust-lang/foo'.",
            }],
        }))
        .unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn graphql_opt_returns_none_when_all_errors_are_root_not_found() {
        // Having several errors doesn't prevent returning `None`, as long as all of them are
        // `NOT_FOUND` errors for top-level fields.
        let result = read_opt(json!({
            "data": { "a": null, "b": null },
            "errors": [
                {
                    "type": "NOT_FOUND",
                    "path": ["a"],
                    "message": "Could not resolve to a Repository with the name 'rust-lang/foo'.",
                },
                {
                    "type": "NOT_FOUND",
                    "path": ["b"],
                    "message": "Could not resolve to a Repository with the name 'rust-lang/bar'.",
                },
            ],
        }))
        .unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn graphql_fails_on_root_not_found() {
        // Unlike `graphql_opt`, `graphql` doesn't treat a missing resource as `None`.
        let res: GraphResult<serde_json::Value> = serde_json::from_value(json!({
            "data": { "repository": null },
            "errors": [{
                "type": "NOT_FOUND",
                "path": ["repository"],
                "message": "Could not resolve to a Repository with the name 'rust-lang/foo'.",
            }],
        }))
        .unwrap();
        let error = read_graphql_data(res).unwrap_err().to_string();
        assert_eq!(
            error,
            r#"graphql error: Could not resolve to a Repository with the name 'rust-lang/foo'. (path: ["repository"])"#
        );
    }

    #[test]
    fn graphql_opt_fails_on_missing_data_without_errors() {
        // Only a `NOT_FOUND` error proves that the resource is missing, not the lack of data.
        let error = read_opt_err(json!({ "data": null }));
        assert_eq!(error, "missing graphql data");
    }

    #[test]
    fn graphql_opt_fails_when_nested_field_is_not_found() {
        let error = read_opt_err(json!({
            "data": { "repository": { "collaborators": { "edges": [null] } } },
            "errors": [{
                "type": "NOT_FOUND",
                "path": ["repository", "collaborators", "edges", 0, "node"],
                "message": "Could not resolve to a node.",
            }],
        }));
        assert_eq!(
            error,
            r#"graphql error: Could not resolve to a node. (path: ["repository","collaborators","edges",0,"node"])"#
        );
    }

    #[test]
    fn graphql_opt_fails_when_contains_nested_not_found_error_and_root_field_null() {
        let error = read_opt_err(json!({
            "data": { "repository": null },
            "errors": [{
                "type": "NOT_FOUND",
                "path": ["repository", "branchProtectionRules"],
                "message": "Could not resolve to a node.",
            }],
        }));
        assert_eq!(
            error,
            r#"graphql error: Could not resolve to a node. (path: ["repository","branchProtectionRules"])"#
        );
    }

    #[test]
    fn graphql_opt_fails_when_not_found_is_accompanied_by_other_errors() {
        let error = read_opt_err(json!({
            "data": { "repository": null },
            "errors": [
                {
                    "type": "NOT_FOUND",
                    "path": ["repository"],
                    "message": "Could not resolve to a Repository with the name 'rust-lang/foo'.",
                },
                {
                    "type": "FORBIDDEN",
                    "path": ["repository"],
                    "message": "Resource not accessible by integration",
                },
            ],
        }));
        // The reported error is the one that prevents treating the resource as missing, not the
        // first one.
        assert_eq!(
            error,
            r#"graphql error: Resource not accessible by integration (path: ["repository"])"#
        );
    }

    #[test]
    fn graphql_opt_fails_on_errors_other_than_root_not_found() {
        let cases = [
            (
                json!({
                    "type": "FORBIDDEN",
                    "path": ["repository"],
                    "message": "Resource not accessible by integration",
                }),
                r#"graphql error: Resource not accessible by integration (path: ["repository"])"#,
            ),
            // Query validation errors have no `type`, even if their path has a single segment.
            (
                json!({
                    "path": ["query"],
                    "extensions": { "code": "variableNotUsed", "variableName": "x" },
                    "message": "Variable $x is declared by anonymous query but not used",
                }),
                r#"graphql error: Variable $x is declared by anonymous query but not used (path: ["query"])"#,
            ),
            // `NOT_FOUND` errors not tied to a top-level field: the path is missing, `null` or
            // empty. An explicit `null` path must deserialize like a missing one.
            (
                json!({ "type": "NOT_FOUND", "message": "Something went wrong" }),
                "graphql error: Something went wrong",
            ),
            (
                json!({ "type": "NOT_FOUND", "path": null, "message": "Something went wrong" }),
                "graphql error: Something went wrong",
            ),
            (
                json!({ "type": "NOT_FOUND", "path": [], "message": "Something went wrong" }),
                "graphql error: Something went wrong (path: [])",
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(read_opt_err(json!({ "errors": [error] })), expected);
        }
    }

    #[test]
    fn serializes_bool_custom_property_values_as_strings() {
        let request = SetCustomPropertiesRequest {
            properties: vec![CustomPropertyValue {
                property_name: "crabwatch".to_string(),
                value: Some(rust_team_data::v1::CustomPropertyValue::Bool(true)),
            }],
        };

        let serialized = serde_json::to_value(request).unwrap();

        assert_eq!(
            serialized,
            serde_json::json!({
                "properties": [
                    {
                        "property_name": "crabwatch",
                        "value": "true",
                    },
                ],
            })
        );
    }

    #[test]
    fn serializes_string_array_custom_property_values_as_arrays() {
        let request = SetCustomPropertiesRequest {
            properties: vec![CustomPropertyValue {
                property_name: "colors".to_string(),
                value: Some(rust_team_data::v1::CustomPropertyValue::StringArray(vec![
                    "red".to_string(),
                    "green".to_string(),
                ])),
            }],
        };

        let serialized = serde_json::to_value(request).unwrap();

        assert_eq!(
            serialized,
            serde_json::json!({
                "properties": [
                    {
                        "property_name": "colors",
                        "value": ["red", "green"],
                    },
                ],
            })
        );
    }
}
