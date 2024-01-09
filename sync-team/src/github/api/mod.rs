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
use std::collections::{HashMap, HashSet};
use std::fmt;

pub(crate) use write::GitHubWrite;

struct HttpClient {
    client: Client,
}

impl HttpClient {
    fn from_token(token: String) -> anyhow::Result<Self> {
        let builder = reqwest::blocking::ClientBuilder::default();
        let mut map = HeaderMap::default();
        let mut auth = HeaderValue::from_str(&format!("token {}", token))?;
        auth.set_sensitive(true);

        map.insert(header::AUTHORIZATION, auth);
        map.insert(
            header::USER_AGENT,
            HeaderValue::from_static(crate::USER_AGENT),
        );

        Ok(Self {
            client: builder.build()?,
        })
    }

    fn req(&self, method: Method, url: &str) -> anyhow::Result<RequestBuilder> {
        let url = if url.starts_with("https://") {
            Cow::Borrowed(url)
        } else {
            Cow::Owned(format!("https://api.github.com/{url}"))
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

pub struct GitHub {
    client: HttpClient,
}

impl GitHub {
    pub(crate) fn new(token: String) -> anyhow::Result<Self> {
        Ok(Self {
            client: HttpClient::from_token(token)?,
        })
    }

    /// Get user names by user ids
    pub(crate) fn usernames(&self, ids: &[usize]) -> anyhow::Result<HashMap<usize, String>> {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Usernames {
            database_id: usize,
            login: String,
        }
        #[derive(serde::Serialize)]
        struct Params {
            ids: Vec<String>,
        }
        static QUERY: &str = "
            query($ids: [ID!]!) {
                nodes(ids: $ids) {
                    ... on User {
                        databaseId
                        login
                    }
                }
            }
        ";

        let mut result = HashMap::new();
        for chunk in ids.chunks(100) {
            let res: GraphNodes<Usernames> = self.client.graphql(
                QUERY,
                Params {
                    ids: chunk.iter().map(|id| user_node_id(*id)).collect(),
                },
            )?;
            for node in res.nodes.into_iter().flatten() {
                result.insert(node.database_id, node.login);
            }
        }
        Ok(result)
    }

    /// Get the owners of an org
    pub(crate) fn org_owners(&self, org: &str) -> anyhow::Result<HashSet<usize>> {
        #[derive(serde::Deserialize, Eq, PartialEq, Hash)]
        struct User {
            id: usize,
        }
        let mut owners = HashSet::new();
        self.client.rest_paginated(
            &Method::GET,
            format!("orgs/{org}/members?role=admin"),
            |resp: Vec<User>| {
                owners.extend(resp.into_iter().map(|u| u.id));
                Ok(())
            },
        )?;
        Ok(owners)
    }

    /// Get all teams associated with a org
    ///
    /// Returns a list of tuples of team name and slug
    pub(crate) fn org_teams(&self, org: &str) -> anyhow::Result<Vec<(String, String)>> {
        let mut teams = Vec::new();

        self.client.rest_paginated(
            &Method::GET,
            format!("orgs/{org}/teams"),
            |resp: Vec<Team>| {
                teams.extend(resp.into_iter().map(|t| (t.name, t.slug)));
                Ok(())
            },
        )?;

        Ok(teams)
    }

    /// Get the team by name and org
    pub(crate) fn team(&self, org: &str, team: &str) -> anyhow::Result<Option<Team>> {
        self.client
            .send_option(Method::GET, &format!("orgs/{org}/teams/{team}"))
    }

    pub(crate) fn team_memberships(
        &self,
        team: &Team,
    ) -> anyhow::Result<HashMap<usize, TeamMember>> {
        #[derive(serde::Deserialize)]
        struct RespTeam {
            members: RespMembers,
        }
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct RespMembers {
            page_info: GraphPageInfo,
            edges: Vec<RespEdge>,
        }
        #[derive(serde::Deserialize)]
        struct RespEdge {
            role: TeamRole,
            node: RespNode,
        }
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct RespNode {
            database_id: usize,
            login: String,
        }
        #[derive(serde::Serialize)]
        struct Params<'a> {
            team: String,
            cursor: Option<&'a str>,
        }
        static QUERY: &str = "
            query($team: ID!, $cursor: String) {
                node(id: $team) {
                    ... on Team {
                        members(after: $cursor) {
                            pageInfo {
                                endCursor
                                hasNextPage
                            }
                            edges {
                                role
                                node {
                                    databaseId
                                    login
                                }
                            }
                        }
                    }
                }
            }
        ";

        let mut memberships = HashMap::new();
        // Return the empty HashMap on new teams from dry runs
        if let Some(id) = team.id {
            let mut page_info = GraphPageInfo::start();
            while page_info.has_next_page {
                let res: GraphNode<RespTeam> = self.client.graphql(
                    QUERY,
                    Params {
                        team: team_node_id(id),
                        cursor: page_info.end_cursor.as_deref(),
                    },
                )?;
                if let Some(team) = res.node {
                    page_info = team.members.page_info;
                    for edge in team.members.edges.into_iter() {
                        memberships.insert(
                            edge.node.database_id,
                            TeamMember {
                                username: edge.node.login,
                                role: edge.role,
                            },
                        );
                    }
                }
            }
        }

        Ok(memberships)
    }

    /// The GitHub names of users invited to the given team
    pub(crate) fn team_membership_invitations(
        &self,
        org: &str,
        team: &str,
    ) -> anyhow::Result<HashSet<String>> {
        let mut invites = HashSet::new();

        self.client.rest_paginated(
            &Method::GET,
            format!("orgs/{org}/teams/{team}/invitations"),
            |resp: Vec<Login>| {
                invites.extend(resp.into_iter().map(|l| l.login));
                Ok(())
            },
        )?;

        Ok(invites)
    }

    /// Get a repo by org and name
    pub(crate) fn repo(&self, org: &str, repo: &str) -> anyhow::Result<Option<Repo>> {
        self.client
            .send_option(Method::GET, &format!("repos/{org}/{repo}"))
    }

    /// Get teams in a repo
    pub(crate) fn repo_teams(&self, org: &str, repo: &str) -> anyhow::Result<Vec<RepoTeam>> {
        let mut teams = Vec::new();

        self.client.rest_paginated(
            &Method::GET,
            format!("repos/{org}/{repo}/teams"),
            |resp: Vec<RepoTeam>| {
                teams.extend(resp);
                Ok(())
            },
        )?;

        Ok(teams)
    }

    /// Get collaborators in a repo
    ///
    /// Only fetches those who are direct collaborators (i.e., not a collaborator through a repo team)
    pub(crate) fn repo_collaborators(
        &self,
        org: &str,
        repo: &str,
    ) -> anyhow::Result<Vec<RepoUser>> {
        let mut users = Vec::new();

        self.client.rest_paginated(
            &Method::GET,
            format!("repos/{org}/{repo}/collaborators?affiliation=direct"),
            |resp: Vec<RepoUser>| {
                users.extend(resp);
                Ok(())
            },
        )?;

        Ok(users)
    }

    /// Get branch_protections
    pub(crate) fn branch_protections(
        &self,
        org: &str,
        repo: &str,
    ) -> anyhow::Result<HashMap<String, (String, BranchProtection)>> {
        #[derive(serde::Serialize)]
        struct Params<'a> {
            org: &'a str,
            repo: &'a str,
        }
        static QUERY: &str = "
            query($org:String!,$repo:String!) {
                repository(owner:$org, name:$repo) {
                    branchProtectionRules(first:100) {
                        nodes { 
                            id,
                            pattern,
                            isAdminEnforced,
                            dismissesStaleReviews,
                            requiredStatusCheckContexts,
                            requiredApprovingReviewCount
                            pushAllowances(first: 100) {
                                nodes {
                                    actor {
                                        ... on Actor {
                                            login
                                        }
                                        ... on Team {
                                            organization {
                                                login
                                            },
                                            name
                                        }
                                    }
                                }
                            }
                         }
                    }
                }
            }
        ";

        #[derive(serde::Deserialize)]
        struct Wrapper {
            repository: Respository,
        }
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Respository {
            branch_protection_rules: GraphNodes<BranchProtectionWrapper>,
        }
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct BranchProtectionWrapper {
            id: String,
            #[serde(flatten)]
            protection: BranchProtection,
        }

        let mut result = HashMap::new();
        let res: Wrapper = self.client.graphql(QUERY, Params { org, repo })?;
        for node in res
            .repository
            .branch_protection_rules
            .nodes
            .into_iter()
            .flatten()
        {
            result.insert(node.protection.pattern.clone(), (node.id, node.protection));
        }
        Ok(result)
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

#[derive(serde::Deserialize, Debug)]
pub(crate) struct Team {
    /// The ID returned by the GitHub API can't be empty, but the None marks teams "created" during
    /// a dry run and not actually present on GitHub, so other methods can avoid acting on them.
    pub(crate) id: Option<usize>,
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
struct Login {
    login: String,
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

#[derive(Debug)]
pub(crate) struct TeamMember {
    pub(crate) username: String,
    pub(crate) role: TeamRole,
}

fn user_node_id(id: usize) -> String {
    base64::encode(format!("04:User{id}"))
}

fn team_node_id(id: usize) -> String {
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
    organization: Login,
    name: String,
}

pub(crate) enum BranchProtectionOp {
    CreateForRepo(String),
    UpdateBranchProtection(String),
}
