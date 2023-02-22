use anyhow::{bail, Context};
use hyper_old_types::header::{Link, RelationType};
use log::{debug, trace};
use reqwest::{
    blocking::{Client, RequestBuilder, Response},
    header::{self, HeaderValue},
    Method, StatusCode,
};
use serde::{de::DeserializeOwned, Deserialize};
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
        self.send_option(Method::GET, &format!("orgs/{org}/teams/{team}"))
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
                .send(Method::POST, &format!("orgs/{org}/teams"), body)?
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
                .req(Method::DELETE, &format!("orgs/{org}/teams/{team}"))?
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
            auto_init: bool,
        }
        let req = &Req {
            name,
            description,
            auto_init: true,
        };
        debug!("Creating the repo {org}/{name} with {req:?}");
        if self.dry_run {
            Ok(Repo {
                id: String::from("ID"),
                name: name.to_string(),
                org: org.to_string(),
                description: Some(description.to_string()),
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
            self.send(Method::PATCH, &format!("repos/{org}/{repo_name}"), &req)?;
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
        let res: Wrapper = self.graphql(QUERY, Params { org, repo })?;
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

    /// Create or update a branch protection.
    pub(crate) fn upsert_branch_protection(
        &self,
        op: BranchProtectionOp,
        pattern: &str,
        branch_protection: &BranchProtection,
    ) -> anyhow::Result<()> {
        debug!("Updating '{}' branch protection", pattern);
        #[derive(Debug, serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Params<'a> {
            id: &'a str,
            pattern: &'a str,
            contexts: &'a [String],
            dismiss_stale: bool,
            review_count: u8,
            push_actor_ids: &'a [String],
        }
        let mutation_name = match op {
            BranchProtectionOp::CreateForRepo(_) => "createBranchProtectionRule",
            BranchProtectionOp::UpdateBranchProtection(_) => "updateBranchProtectionRule",
        };
        let id_field = match op {
            BranchProtectionOp::CreateForRepo(_) => "repositoryId",
            BranchProtectionOp::UpdateBranchProtection(_) => "branchProtectionRuleId",
        };
        let id = &match op {
            BranchProtectionOp::CreateForRepo(id) => id,
            BranchProtectionOp::UpdateBranchProtection(id) => id,
        };
        let query = format!("
        mutation($id: String!, $pattern:String!, $contexts: [String!], $dismissStale: bool, $reviewCount: int, $pushActorIds: [ID!]) {{
            {mutation_name}(input: {{
                {id_field}: $id, 
                pattern: $pattern, 
                requiresStatusChecks: true, 
                requiredStatusCheckContexts: $contexts, 
                isAdminEnforced: true, 
                requiredApprovingReviewCount: $reviewCount, 
                dismissesStaleReviews: $dismissStale, 
                requiresApprovingReviews:true
                pushActorIds: $pushActorIds
            }}) {{
              branchProtectionRule {{
                id
              }}
            }}
          }}
        ");
        let mut push_actor_ids = vec![];
        for name in &branch_protection.push_allowances {
            push_actor_ids.push(self.user_id(name)?);
        }

        if !self.dry_run {
            self.graphql(
                &query,
                Params {
                    id,
                    pattern,
                    contexts: &branch_protection.required_status_check_contexts,
                    dismiss_stale: branch_protection.dismisses_stale_reviews,
                    review_count: branch_protection.required_approving_review_count,
                    push_actor_ids: &push_actor_ids,
                },
            )?;
        }
        Ok(())
    }

    fn user_id(&self, name: &str) -> anyhow::Result<String> {
        #[derive(serde::Serialize)]
        struct Params<'a> {
            name: &'a str,
        }
        let query = "
            query($name: String!) {
                user(login: $name) {
                    id
                }
            }
        ";
        #[derive(serde::Deserialize)]
        struct Data {
            user: User,
        }
        #[derive(serde::Deserialize)]
        struct User {
            id: String,
        }

        let data: Data = self.graphql(query, Params { name })?;
        Ok(data.user.id)
    }

    /// Delete a branch protection
    pub(crate) fn delete_branch_protection(
        &self,
        org: &str,
        repo_name: &str,
        id: &str,
    ) -> anyhow::Result<()> {
        debug!("Removing protection in {}/{}", org, repo_name);
        println!("Remove protection {id}");
        if !self.dry_run {
            #[derive(serde::Serialize)]
            #[serde(rename_all = "camelCase")]
            struct Params<'a> {
                id: &'a str,
            }
            let query = "
                mutation($id: String!) {
                    deleteBranchProtectionRule(input: { branchProtectionRuleId: $id }) {
                        clientMutationId
                    }
                }
            ";
            self.graphql(query, Params { id })?;
        }
        Ok(())
    }

    fn req(&self, method: Method, url: &str) -> anyhow::Result<RequestBuilder> {
        let url = if url.starts_with("https://") {
            Cow::Borrowed(url)
        } else {
            Cow::Owned(format!("https://api.github.com/{url}"))
        };
        trace!("http request: {} {}", method, url);
        if self.dry_run && method != Method::GET && !url.contains("graphql") {
            panic!("Called a non-GET request in dry run mode: {method}");
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
    pub(crate) push_allowances: Vec<String>,
}

fn nullable<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::de::Deserializer<'de>,
    T: Default + DeserializeOwned,
{
    let opt = Option::deserialize(deserializer)?;
    Ok(opt.unwrap_or_default())
}

fn allowances<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::de::Deserializer<'de>,
{
    #[derive(Deserialize)]
    struct Allowances {
        nodes: Vec<Actor>,
    }
    #[derive(Deserialize)]
    struct Actor {
        actor: Login,
    }
    #[derive(Deserialize)]
    struct Login {
        login: String,
    }

    let allowances = Allowances::deserialize(deserializer)?;
    Ok(allowances
        .nodes
        .into_iter()
        .map(|a| a.actor.login)
        .collect())
}

pub(crate) enum BranchProtectionOp {
    CreateForRepo(String),
    UpdateBranchProtection(String),
}
