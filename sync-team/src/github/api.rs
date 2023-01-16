use anyhow::{bail, Context};
use hyper_old_types::header::{Link, RelationType};
use log::{debug, trace};
use reqwest::{
    blocking::{Client, RequestBuilder, Response},
    header::{self, HeaderValue},
    Method, StatusCode,
};
use serde::de::DeserializeOwned;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt;

pub(crate) struct GitHub {
    token: String,
    dry_run: bool,
    client: Client,
}

impl GitHub {
    pub(crate) fn new(token: String, dry_run: bool) -> Self {
        GitHub {
            token,
            dry_run,
            client: Client::new(),
        }
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
            let res: GraphNodes<Usernames> = self.graphql(
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
        self.rest_paginated(
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
    pub(crate) fn org_teams(&self, org: &str) -> anyhow::Result<HashSet<String>> {
        let mut teams = HashSet::new();

        self.rest_paginated(
            &Method::GET,
            format!("orgs/{org}/teams"),
            |resp: Vec<Team>| {
                teams.extend(resp.into_iter().map(|t| t.name));
                Ok(())
            },
        )?;

        Ok(teams)
    }

    /// Get the team by name and org
    pub(crate) fn team(&self, org: &str, team: &str) -> anyhow::Result<Option<Team>> {
        self.send_option(Method::GET, &format!("orgs/{}/teams/{}", org, team))
    }

    /// Create a team in a org
    pub(crate) fn create_team(
        &self,
        org: &str,
        name: &str,
        description: &str,
        privacy: TeamPrivacy,
    ) -> anyhow::Result<Team> {
        #[derive(serde::Serialize, Debug)]
        struct Req<'a> {
            name: &'a str,
            description: &'a str,
            privacy: TeamPrivacy,
        }
        debug!("Creating team '{name}' in '{org}'");
        if self.dry_run {
            Ok(Team {
                // The `None` marks that the team is "created" by the dry run and
                // doesn't actually exist on GitHub
                id: None,
                name: name.to_string(),
                description: description.to_string(),
                privacy,
            })
        } else {
            let body = &Req {
                name,
                description,
                privacy,
            };
            Ok(self
                .send(Method::POST, &format!("orgs/{}/teams", org), body)?
                .json()?)
        }
    }

    /// Edit a team
    pub(crate) fn edit_team(
        &self,
        org: &str,
        name: &str,
        new_name: Option<&str>,
        new_description: Option<&str>,
        new_privacy: Option<TeamPrivacy>,
    ) -> anyhow::Result<()> {
        #[derive(serde::Serialize, Debug)]
        struct Req<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            name: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            description: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            privacy: Option<TeamPrivacy>,
        }
        let req = Req {
            name: new_name,
            description: new_description,
            privacy: new_privacy,
        };
        debug!(
            "Editing team '{name}' in '{org}' with request: {}",
            serde_json::to_string(&req).unwrap_or_else(|_| "INVALID_REQUEST".to_string())
        );
        if !self.dry_run {
            self.send(Method::PATCH, &format!("orgs/{org}/teams/{name}"), &req)?;
        }

        Ok(())
    }

    /// Delete a team by name and org
    pub(crate) fn delete_team(&self, org: &str, team: &str) -> anyhow::Result<()> {
        debug!("Deleting team '{team}' in '{org}'");
        if !self.dry_run {
            let resp = self
                .req(Method::DELETE, &format!("orgs/{}/teams/{}", org, team))?
                .send()?;
            match resp.status() {
                StatusCode::OK | StatusCode::NOT_FOUND => {}
                _ => {
                    resp.error_for_status()?;
                }
            }
        }
        Ok(())
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
                let res: GraphNode<RespTeam> = self.graphql(
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

    /// Set a user's membership in a team to a role
    pub(crate) fn set_team_membership(
        &self,
        org: &str,
        team: &str,
        user: &str,
        role: TeamRole,
    ) -> anyhow::Result<()> {
        debug!("Setting membership of '{user}' in team '{team}' in org '{org}' to role '{role}'");
        #[derive(serde::Serialize, Debug)]
        struct Req {
            role: TeamRole,
        }
        if !self.dry_run {
            self.send(
                Method::PUT,
                &format!("orgs/{org}/teams/{team}/memberships/{user}"),
                &Req { role },
            )?;
        }

        Ok(())
    }

    /// Remove a user from a team
    pub(crate) fn remove_team_membership(
        &self,
        org: &str,
        team: &str,
        user: &str,
    ) -> anyhow::Result<()> {
        debug!("Removing membership of '{user}' from team '{team}' in org '{org}'");
        if !self.dry_run {
            self.req(
                Method::DELETE,
                &format!("orgs/{org}/teams/{team}/memberships/{user}"),
            )?
            .send()?
            .error_for_status()?;
        }

        Ok(())
    }

    /// Get a repo by org and name
    pub(crate) fn repo(&self, org: &str, repo: &str) -> anyhow::Result<Option<Repo>> {
        self.send_option(Method::GET, &format!("repos/{org}/{repo}"))
    }

    /// Create a repo
    pub(crate) fn create_repo(
        &self,
        org: &str,
        name: &str,
        description: &str,
    ) -> anyhow::Result<Repo> {
        #[derive(serde::Serialize, Debug)]
        struct Req<'a> {
            name: &'a str,
            description: &'a str,
        }
        let req = &Req { name, description };
        debug!("Creating the repo {org}/{name} with {req:?}");
        if self.dry_run {
            Ok(Repo {
                name: name.to_string(),
                org: org.to_string(),
                description: Some(description.to_string()),
                default_branch: String::from("main"),
            })
        } else {
            Ok(self
                .send(Method::POST, &format!("orgs/{org}/repos"), req)?
                .json()?)
        }
    }

    pub(crate) fn edit_repo(
        &self,
        org: &str,
        repo_name: &str,
        description: &str,
    ) -> anyhow::Result<()> {
        #[derive(serde::Serialize, Debug)]
        struct Req<'a> {
            description: &'a str,
        }
        let req = Req { description };
        debug!("Editing repo {}/{} with {:?}", org, repo_name, req);
        if !self.dry_run {
            self.send(Method::PATCH, &format!("repos/{}/{}", org, repo_name), &req)?;
        }
        Ok(())
    }

    /// Get teams in a repo
    pub(crate) fn repo_teams(&self, org: &str, repo: &str) -> anyhow::Result<Vec<RepoTeam>> {
        let mut teams = Vec::new();

        self.rest_paginated(
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

        self.rest_paginated(
            &Method::GET,
            format!("repos/{org}/{repo}/collaborators?affiliation=direct"),
            |resp: Vec<RepoUser>| {
                users.extend(resp);
                Ok(())
            },
        )?;

        Ok(users)
    }

    /// Update a team's permissions to a repo
    pub(crate) fn update_team_repo_permissions(
        &self,
        org: &str,
        repo: &str,
        team: &str,
        permission: &RepoPermission,
    ) -> anyhow::Result<()> {
        #[derive(serde::Serialize, Debug)]
        struct Req<'a> {
            permission: &'a RepoPermission,
        }
        debug!("Updating permission for team {team} on {org}/{repo} to {permission:?}");
        if !self.dry_run {
            self.send(
                Method::PUT,
                &format!("orgs/{org}/teams/{team}/repos/{org}/{repo}"),
                &Req { permission },
            )?;
        }

        Ok(())
    }

    /// Update a user's permissions to a repo
    pub(crate) fn update_user_repo_permissions(
        &self,
        org: &str,
        repo: &str,
        user: &str,
        permission: &RepoPermission,
    ) -> anyhow::Result<()> {
        #[derive(serde::Serialize, Debug)]
        struct Req<'a> {
            permission: &'a RepoPermission,
        }
        debug!("Updating permission for user {user} on {org}/{repo} to {permission:?}");
        if !self.dry_run {
            self.send(
                Method::PUT,
                &format!("repos/{org}/{repo}/collaborators/{user}"),
                &Req { permission },
            )?;
        }
        Ok(())
    }

    /// Remove a team from a repo
    pub(crate) fn remove_team_from_repo(
        &self,
        org: &str,
        repo: &str,
        team: &str,
    ) -> anyhow::Result<()> {
        debug!("Removing team {team} from repo {org}/{repo}");
        if !self.dry_run {
            self.req(
                Method::DELETE,
                &format!("orgs/{org}/teams/{team}/repos/{org}/{repo}"),
            )?
            .send()?
            .error_for_status()?;
        }

        Ok(())
    }

    /// Remove a collaborator from a repo
    pub(crate) fn remove_collaborator_from_repo(
        &self,
        org: &str,
        repo: &str,
        collaborator: &str,
    ) -> anyhow::Result<()> {
        debug!("Removing collaborator {collaborator} from repo {org}/{repo}");
        if !self.dry_run {
            self.req(
                Method::DELETE,
                &format!("repos/{org}/{repo}/collaborators/{collaborator}"),
            )?
            .send()?
            .error_for_status()?;
        }
        Ok(())
    }

    /// Get the head commit of the supplied branch
    pub(crate) fn branch(
        &self,
        org: &str,
        repo_name: &str,
        branch_name: &str,
    ) -> anyhow::Result<Option<String>> {
        let branch = self.send_option::<Branch>(
            Method::GET,
            &format!("repos/{}/{}/branches/{}", org, repo_name, branch_name),
        )?;
        Ok(branch.map(|b| b.commit.sha))
    }

    /// Create a branch
    pub(crate) fn create_branch(
        &self,
        org: &str,
        repo_name: &str,
        branch_name: &str,
        commit: &str,
    ) -> anyhow::Result<()> {
        #[derive(serde::Serialize, Debug)]
        struct Req<'a> {
            r#ref: &'a str,
            sha: &'a str,
        }
        debug!(
            "Creating branch in {}/{}: {} with commit {}",
            org, repo_name, branch_name, commit
        );
        if !self.dry_run {
            self.send(
                Method::POST,
                &format!("repos/{}/{}/git/refs", org, repo_name),
                &Req {
                    r#ref: &format!("refs/heads/{}", branch_name),
                    sha: commit,
                },
            )?;
        }
        Ok(())
    }

    /// Get protected branches from a repo
    pub(crate) fn protected_branches(&self, repo: &Repo) -> anyhow::Result<HashSet<String>> {
        let mut names = HashSet::new();
        self.rest_paginated(
            &Method::GET,
            format!("repos/{}/{}/branches?protected=true", repo.org, repo.name),
            |resp: Vec<Branch>| {
                names.extend(resp.into_iter().map(|b| b.name));

                Ok(())
            },
        )?;
        Ok(names)
    }

    pub(crate) fn branch_protection(
        &self,
        org: &str,
        repo_name: &str,
        branch_name: &str,
    ) -> anyhow::Result<Option<BranchProtection>> {
        self.send_option::<BranchProtection>(
            Method::GET,
            &format!(
                "repos/{}/{}/branches/{}/protection",
                org, repo_name, branch_name
            ),
        )
    }

    /// Update a branch's permissions.
    ///
    /// Returns `Ok(true)` on success, `Ok(false)` if the branch doesn't exist, and `Err(_)` otherwise.
    pub(crate) fn update_branch_protection(
        &self,
        org: &str,
        repo_name: &str,
        branch_name: &str,
        branch_protection: &BranchProtection,
    ) -> anyhow::Result<bool> {
        debug!(
            "Updating branch protection on repo {}/{} for {}: {}",
            org,
            repo_name,
            branch_name,
            serde_json::to_string_pretty(&branch_protection)
                .unwrap_or_else(|_| "<invalid json>".to_string())
        );
        if !self.dry_run {
            let resp = self
                .req(
                    Method::PUT,
                    &format!(
                        "repos/{}/{}/branches/{}/protection",
                        org, repo_name, branch_name
                    ),
                )?
                .json(branch_protection)
                .send()?;
            match resp.status() {
                StatusCode::OK => Ok(true),
                StatusCode::NOT_FOUND => Ok(false),
                _ => {
                    resp.error_for_status()?;
                    Ok(false)
                }
            }
        } else {
            Ok(true)
        }
    }

    /// Delete a branch protection
    pub(crate) fn delete_branch_protection(
        &self,
        org: &str,
        repo_name: &str,
        branch: &str,
    ) -> anyhow::Result<()> {
        debug!(
            "Removing protection in {}/{} from {} branch",
            org, repo_name, branch
        );
        if !self.dry_run {
            self.req(
                Method::DELETE,
                &format!("repos/{}/{}/branches/{}/protection", org, repo_name, branch),
            )?
            .send()?
            .error_for_status()?;
        }
        Ok(())
    }

    fn req(&self, method: Method, url: &str) -> anyhow::Result<RequestBuilder> {
        let url = if url.starts_with("https://") {
            Cow::Borrowed(url)
        } else {
            Cow::Owned(format!("https://api.github.com/{}", url))
        };
        trace!("http request: {} {}", method, url);
        if self.dry_run && method != Method::GET && !url.contains("graphql") {
            panic!("Called a non-GET request in dry run mode: {}", method);
        }
        Ok(self
            .client
            .request(method, url.as_ref())
            .header(
                header::AUTHORIZATION,
                HeaderValue::from_str(&format!("token {}", self.token))?,
            )
            .header(
                header::USER_AGENT,
                HeaderValue::from_static(crate::USER_AGENT),
            ))
    }

    fn send<T: serde::Serialize + std::fmt::Debug>(
        &self,
        method: Method,
        url: &str,
        body: &T,
    ) -> Result<Response, anyhow::Error> {
        Ok(self
            .req(method, url)?
            .json(body)
            .send()?
            .error_for_status()?)
    }

    fn send_option<T: DeserializeOwned>(
        &self,
        method: Method,
        url: &str,
    ) -> Result<Option<T>, anyhow::Error> {
        let resp = self.req(method.clone(), url)?.send()?;
        match resp.status() {
            StatusCode::OK => Ok(Some(resp.json().with_context(|| {
                format!("Failed to decode response body on {method} request to '{url}'")
            })?)),
            StatusCode::NOT_FOUND => Ok(None),
            _ => Err(resp.error_for_status().unwrap_err().into()),
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
        let res: GraphResult<R> = self
            .req(Method::POST, "graphql")?
            .json(&Request { query, variables })
            .send()?
            .error_for_status()?
            .json()?;
        if let Some(error) = res.errors.get(0) {
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
                .error_for_status()?;

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

#[derive(serde::Deserialize)]
struct GraphResult<T> {
    data: Option<T>,
    #[serde(default)]
    errors: Vec<GraphError>,
}

#[derive(serde::Deserialize)]
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
    pub(crate) description: String,
    pub(crate) privacy: TeamPrivacy,
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
}

impl fmt::Display for RepoPermission {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Write => write!(f, "write"),
            Self::Admin => write!(f, "admin"),
            Self::Maintain => write!(f, "maintain"),
            Self::Triage => write!(f, "triage"),
        }
    }
}

#[derive(serde::Deserialize, Debug)]
pub(crate) struct Repo {
    pub(crate) name: String,
    #[serde(alias = "owner", deserialize_with = "repo_owner")]
    pub(crate) org: String,
    pub(crate) description: Option<String>,
    pub(crate) default_branch: String,
}

fn repo_owner<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::de::Deserializer<'de>,
{
    use serde::de::Deserialize;
    let owner = RepoOwner::deserialize(deserializer)?;
    Ok(owner.login)
}

#[derive(serde::Deserialize, Debug)]
pub(crate) struct RepoOwner {
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
    base64::encode(format!("04:User{}", id))
}

fn team_node_id(id: usize) -> String {
    base64::encode(format!("04:Team{}", id))
}

#[derive(serde::Deserialize, Debug)]
pub(crate) struct Branch {
    pub(crate) name: String,
    pub(crate) commit: Commit,
}

#[derive(serde::Deserialize, Debug)]
pub(crate) struct Commit {
    pub(crate) sha: String,
}

pub(crate) mod branch_protection {
    use super::*;

    #[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
    pub(crate) struct BranchProtection {
        pub(crate) required_status_checks: RequiredStatusChecks,
        pub(crate) enforce_admins: EnforceAdmins,
        pub(crate) required_pull_request_reviews: PullRequestReviews,
        pub(crate) restrictions: Option<Restrictions>,
    }

    #[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
    pub(crate) struct RequiredStatusChecks {
        pub(crate) strict: bool,
        pub(crate) checks: Vec<Check>,
    }

    #[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
    pub(crate) struct Check {
        pub(crate) context: String,
    }

    impl std::fmt::Debug for Check {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(&self.context)
        }
    }

    #[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
    pub(crate) struct PullRequestReviews {
        // Even though we don't want dismissal restrictions, it cannot be omitted
        #[serde(default)]
        pub(crate) dismissal_restrictions: HashMap<(), ()>,
        pub(crate) dismiss_stale_reviews: bool,
        pub(crate) required_approving_review_count: u8,
    }

    #[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
    pub(crate) struct Restrictions {
        pub(crate) users: Vec<UserRestriction>,
        pub(crate) teams: Vec<String>,
    }

    #[derive(Clone, serde::Serialize, serde::Deserialize)]
    #[serde(untagged)]
    pub(crate) enum EnforceAdmins {
        // Used for serialization
        Bool(bool),
        // Used for deserialization
        Object { enabled: bool },
    }

    impl EnforceAdmins {
        fn enabled(&self) -> bool {
            match *self {
                EnforceAdmins::Bool(e) => e,
                EnforceAdmins::Object { enabled } => enabled,
            }
        }
    }

    impl PartialEq for EnforceAdmins {
        fn eq(&self, other: &Self) -> bool {
            self.enabled() == other.enabled()
        }
    }

    #[derive(Clone, serde::Serialize, serde::Deserialize)]
    #[serde(untagged)]
    pub(crate) enum UserRestriction {
        // Used for serialization
        Name(String),
        // Used for deserialization
        Object {
            #[serde(rename = "login")]
            name: String,
        },
    }

    impl UserRestriction {
        fn name(&self) -> &str {
            match self {
                Self::Name(n) => n,
                Self::Object { name } => name,
            }
        }
    }

    impl std::fmt::Debug for UserRestriction {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.name())
        }
    }

    impl PartialEq for UserRestriction {
        fn eq(&self, other: &Self) -> bool {
            self.name() == other.name()
        }
    }
}

pub(crate) use branch_protection::BranchProtection;
