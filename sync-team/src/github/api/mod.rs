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

    pub fn uses_pat(&self) -> bool {
        matches!(self.github_tokens, GitHubTokens::Pat(_))
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
                        .map(|r| r.contains(&RelationType::Next))
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
    pub(crate) private: bool,
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
    App(AppPushAllowanceActor),
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

/// GitHub app that can be allowed to push to a branch in a repo
#[derive(Clone, Deserialize, Debug, PartialEq, Eq)]
pub(crate) struct AppPushAllowanceActor {
    pub(crate) name: String,
    /// Node ID, which can be used as a push actor ID
    pub(crate) id: String,
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

/// GitHub Repository Ruleset
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Ruleset {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) id: Option<i64>,
    pub(crate) name: String,
    pub(crate) target: RulesetTarget,
    pub(crate) source_type: RulesetSourceType,
    pub(crate) enforcement: RulesetEnforcement,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bypass_actors: Option<Vec<RulesetBypassActor>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) conditions: Option<RulesetConditions>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) rules: Vec<RulesetRule>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum RulesetTarget {
    Branch,
    Tag,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) enum RulesetSourceType {
    Repository,
    Organization,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum RulesetEnforcement {
    Active,
    Disabled,
    Evaluate,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct RulesetBypassActor {
    /// The ID of the actor that can bypass a ruleset.
    /// Required for Team and Integration actor types.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) actor_id: Option<i64>,
    pub(crate) actor_type: RulesetActorType,
    /// The bypass mode for the actor. Defaults to "always" per GitHub API.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bypass_mode: Option<RulesetBypassMode>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) enum RulesetActorType {
    /// GitHub App integration
    Integration,
    /// GitHub Team
    Team,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RulesetBypassMode {
    Always,
    PullRequest,
    Exempt,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct RulesetConditions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ref_name: Option<RulesetRefNameCondition>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct RulesetRefNameCondition {
    pub(crate) include: Vec<String>,
    pub(crate) exclude: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct MergeQueueParameters {
    pub(crate) check_response_timeout_minutes: i32,
    pub(crate) grouping_strategy: MergeQueueGroupingStrategy,
    pub(crate) max_entries_to_build: i32,
    pub(crate) max_entries_to_merge: i32,
    pub(crate) merge_method: MergeQueueMergeMethod,
    pub(crate) min_entries_to_merge: i32,
    pub(crate) min_entries_to_merge_wait_minutes: i32,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum MergeQueueGroupingStrategy {
    Allgreen,
    Headgreen,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum MergeQueueMergeMethod {
    Merge,
    Squash,
    Rebase,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct RequiredDeploymentsParameters {
    pub(crate) required_deployment_environments: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PullRequestParameters {
    pub(crate) dismiss_stale_reviews_on_push: bool,
    pub(crate) require_code_owner_review: bool,
    pub(crate) require_last_push_approval: bool,
    pub(crate) required_approving_review_count: i32,
    pub(crate) required_review_thread_resolution: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct RequiredStatusChecksParameters {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) do_not_enforce_on_create: Option<bool>,
    pub(crate) required_status_checks: Vec<RequiredStatusCheck>,
    pub(crate) strict_required_status_checks_policy: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct RequiredStatusCheck {
    pub(crate) context: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) integration_id: Option<i64>,
}

pub(crate) enum RulesetOp {
    CreateForRepo,
    UpdateRuleset(i64),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bypass_actor_serialization() {
        // Test Team actor with ID
        let team_actor = RulesetBypassActor {
            actor_id: Some(234),
            actor_type: RulesetActorType::Team,
            bypass_mode: Some(RulesetBypassMode::Always),
        };
        let json =
            serde_json::to_string(&team_actor).expect("Team actor serialization should succeed");
        assert_eq!(
            json, r#"{"actor_id":234,"actor_type":"Team","bypass_mode":"always"}"#,
            "Team actor should serialize with numeric actor_id, PascalCase actor_type, and snake_case bypass_mode"
        );

        // Test Integration actor with ID
        let integration_actor = RulesetBypassActor {
            actor_id: Some(123456),
            actor_type: RulesetActorType::Integration,
            bypass_mode: Some(RulesetBypassMode::Always),
        };
        let json = serde_json::to_string(&integration_actor)
            .expect("Integration actor serialization should succeed");
        assert_eq!(
            json, r#"{"actor_id":123456,"actor_type":"Integration","bypass_mode":"always"}"#,
            "Integration actor should serialize with numeric actor_id"
        );

        // Test with None actor_id (field omitted)
        let actor_no_id = RulesetBypassActor {
            actor_id: None,
            actor_type: RulesetActorType::Team,
            bypass_mode: Some(RulesetBypassMode::Always),
        };
        let json = serde_json::to_string(&actor_no_id)
            .expect("Actor without ID serialization should succeed");
        assert_eq!(
            json, r#"{"actor_type":"Team","bypass_mode":"always"}"#,
            "Actor without ID should omit actor_id field"
        );

        // Test pull_request bypass mode
        let pr_actor = RulesetBypassActor {
            actor_id: Some(789),
            actor_type: RulesetActorType::Team,
            bypass_mode: Some(RulesetBypassMode::PullRequest),
        };
        let json = serde_json::to_string(&pr_actor)
            .expect("PullRequest bypass mode serialization should succeed");
        assert_eq!(
            json, r#"{"actor_id":789,"actor_type":"Team","bypass_mode":"pull_request"}"#,
            "PullRequest bypass mode should serialize as 'pull_request' with underscore"
        );
    }

    #[test]
    fn test_bypass_actor_deserialization() {
        // Test deserializing Team actor from GitHub API response
        let json = r#"{"actor_id":234,"actor_type":"Team","bypass_mode":"always"}"#;
        let actor: RulesetBypassActor =
            serde_json::from_str(json).expect("Should deserialize valid Team actor");
        assert_eq!(actor.actor_id, Some(234), "actor_id should be numeric");
        assert_eq!(
            actor.actor_type,
            RulesetActorType::Team,
            "actor_type should be Team"
        );
        assert_eq!(
            actor.bypass_mode,
            Some(RulesetBypassMode::Always),
            "bypass_mode should be Always"
        );

        // Test deserializing Integration actor
        let json = r#"{"actor_id":456,"actor_type":"Integration","bypass_mode":"always"}"#;
        let actor: RulesetBypassActor =
            serde_json::from_str(json).expect("Should deserialize valid Integration actor");
        assert_eq!(actor.actor_id, Some(456));
        assert_eq!(actor.actor_type, RulesetActorType::Integration);

        // Test with missing bypass_mode (should default to None)
        let json = r#"{"actor_id":1,"actor_type":"Team"}"#;
        let actor: RulesetBypassActor =
            serde_json::from_str(json).expect("Should deserialize Team without bypass_mode");
        assert_eq!(actor.actor_id, Some(1));
        assert_eq!(
            actor.bypass_mode, None,
            "bypass_mode should be None when omitted from JSON"
        );

        // Test all bypass modes can be deserialized
        let bypass_modes = [
            ("always", RulesetBypassMode::Always),
            ("pull_request", RulesetBypassMode::PullRequest),
            ("exempt", RulesetBypassMode::Exempt),
        ];
        for (mode_str, expected_mode) in bypass_modes {
            let json = format!(
                r#"{{"actor_id":1,"actor_type":"Team","bypass_mode":"{}"}}"#,
                mode_str
            );
            let actor: RulesetBypassActor = serde_json::from_str(&json)
                .unwrap_or_else(|e| panic!("Should deserialize bypass mode {}: {}", mode_str, e));
            assert_eq!(
                actor.bypass_mode,
                Some(expected_mode),
                "bypass_mode {} should deserialize correctly",
                mode_str
            );
        }
    }
}
