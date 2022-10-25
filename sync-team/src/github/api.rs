use anyhow::bail;
use hyper_old_types::header::{Link, RelationType};
use log::{debug, trace};
use reqwest::{
    blocking::{Client, RequestBuilder, Response},
    header::{self, HeaderValue},
    Method, StatusCode,
};
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

    fn rest_paginated<F>(&self, method: &Method, url: String, mut f: F) -> anyhow::Result<()>
    where
        F: FnMut(Response) -> anyhow::Result<()>,
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

            f(resp)?;
        }
        Ok(())
    }

    pub(crate) fn team(&self, org: &str, team: &str) -> anyhow::Result<Option<Team>> {
        let resp = self
            .req(Method::GET, &format!("orgs/{}/teams/{}", org, team))?
            .send()?;
        match resp.status() {
            StatusCode::OK => Ok(Some(resp.json()?)),
            StatusCode::NOT_FOUND => Ok(None),
            _ => Err(resp.error_for_status().unwrap_err().into()),
        }
    }

    pub(crate) fn create_team(
        &self,
        org: &str,
        name: &str,
        description: &str,
        privacy: TeamPrivacy,
    ) -> anyhow::Result<Team> {
        #[derive(serde::Serialize)]
        struct Req<'a> {
            name: &'a str,
            description: &'a str,
            privacy: TeamPrivacy,
        }
        if self.dry_run {
            debug!("dry: created team {}/{}", org, name);
            Ok(Team {
                // The None marks that the team is "created" by the dry run and doesn't actually
                // exists on GitHub
                id: None,
                name: name.to_string(),
                description: description.to_string(),
                privacy,
            })
        } else {
            Ok(self
                .req(Method::POST, &format!("orgs/{}/teams", org))?
                .json(&Req {
                    name,
                    description,
                    privacy,
                })
                .send()?
                .error_for_status()?
                .json()?)
        }
    }

    pub(crate) fn update_team_repo_permissions(
        &self,
        org: &str,
        repo: &str,
        team_name: &str,
        permission: &RepoPermission,
    ) -> anyhow::Result<()> {
        #[derive(serde::Serialize)]
        struct Req<'a> {
            permission: &'a RepoPermission,
        }
        if self.dry_run {
            debug!(
                "dry: updating permission for team {team_name} on {org}/{repo} to {permission:?}"
            );
            Ok(())
        } else {
            let _ = self
                .req(
                    Method::PUT,
                    &format!("orgs/{org}/teams/{team_name}/repos/{org}/{repo}"),
                )?
                .json(&Req { permission })
                .send()?
                .error_for_status()?;
            Ok(())
        }
    }

    pub(crate) fn update_user_repo_permissions(
        &self,
        org: &str,
        repo: &str,
        user_name: &str,
        permission: &RepoPermission,
    ) -> anyhow::Result<()> {
        #[derive(serde::Serialize)]
        struct Req<'a> {
            permission: &'a RepoPermission,
        }
        if self.dry_run {
            debug!(
                "dry: updating permission for user {user_name} on {org}/{repo} to {permission:?}"
            );
            Ok(())
        } else {
            let _ = self
                .req(
                    Method::PUT,
                    &format!("repos/{org}/{repo}/collaborators/{user_name}"),
                )?
                .json(&Req { permission })
                .send()?
                .error_for_status()?;
            Ok(())
        }
    }

    pub(crate) fn edit_team(
        &self,
        team: &Team,
        name: &str,
        description: &str,
        privacy: TeamPrivacy,
    ) -> anyhow::Result<()> {
        #[derive(serde::Serialize)]
        struct Req<'a> {
            name: &'a str,
            description: &'a str,
            privacy: TeamPrivacy,
        }
        if let (false, Some(id)) = (self.dry_run, team.id) {
            self.req(Method::PATCH, &format!("teams/{}", id))?
                .json(&Req {
                    name,
                    description,
                    privacy,
                })
                .send()?
                .error_for_status()?;
        } else {
            debug!("dry: edit team {}", name)
        }
        Ok(())
    }

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

    pub(crate) fn org_owners(&self, org: &str) -> anyhow::Result<HashSet<usize>> {
        #[derive(serde::Deserialize, Eq, PartialEq, Hash)]
        struct User {
            id: usize,
        }
        let mut owners = HashSet::new();
        self.rest_paginated(
            &Method::GET,
            format!("orgs/{}/members?role=admin", org),
            |resp| {
                let partial: Vec<User> = resp.json()?;
                for owner in partial {
                    owners.insert(owner.id);
                }
                Ok(())
            },
        )?;
        Ok(owners)
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

    pub(crate) fn set_membership(
        &self,
        team: &Team,
        username: &str,
        role: TeamRole,
    ) -> anyhow::Result<()> {
        #[derive(serde::Serialize)]
        struct Req {
            role: TeamRole,
        }
        if let (false, Some(id)) = (self.dry_run, team.id) {
            self.req(
                Method::PUT,
                &format!("teams/{}/memberships/{}", id, username),
            )?
            .json(&Req { role })
            .send()?
            .error_for_status()?;
        } else {
            debug!("dry: set membership of {} to {}", username, role);
        }
        Ok(())
    }

    pub(crate) fn remove_membership(&self, team: &Team, username: &str) -> anyhow::Result<()> {
        if let (false, Some(id)) = (self.dry_run, team.id) {
            self.req(
                Method::DELETE,
                &format!("teams/{}/memberships/{}", id, username),
            )?
            .send()?
            .error_for_status()?;
        } else {
            debug!("dry: remove membership of {}", username);
        }
        Ok(())
    }

    pub(crate) fn repo(&self, org: &str, repo: &str) -> anyhow::Result<Option<Repo>> {
        let resp = self
            .req(Method::GET, &format!("repos/{}/{}", org, repo))?
            .send()?;
        match resp.status() {
            StatusCode::OK => Ok(Some(resp.json()?)),
            StatusCode::NOT_FOUND => Ok(None),
            _ => Err(resp.error_for_status().unwrap_err().into()),
        }
    }

    pub(crate) fn create_repo(
        &self,
        org: &str,
        name: &str,
        description: &str,
    ) -> anyhow::Result<Repo> {
        #[derive(serde::Serialize)]
        struct Req<'a> {
            name: &'a str,
            description: &'a str,
        }
        if self.dry_run {
            debug!("dry: created repo {}/{}", org, name);
            Ok(Repo {
                name: name.to_string(),
                org: org.to_string(),
                description: description.to_string(),
                default_branch: String::from("main"),
            })
        } else {
            Ok(self
                .req(Method::POST, &format!("orgs/{}/repos", org))?
                .json(&Req { name, description })
                .send()?
                .error_for_status()?
                .json()?)
        }
    }

    pub(crate) fn edit_repo(&self, repo: &Repo, description: &str) -> anyhow::Result<()> {
        #[derive(serde::Serialize)]
        struct Req<'a> {
            description: &'a str,
        }
        if !self.dry_run {
            self.req(Method::PATCH, &format!("repos/{}/{}", repo.org, repo.name))?
                .json(&Req { description })
                .send()?
                .error_for_status()?;
        } else {
            debug!("dry: editing repo {}/{}", repo.org, repo.name)
        }
        Ok(())
    }

    pub(crate) fn teams(&self, org: &str, repo: &str) -> anyhow::Result<HashSet<String>> {
        let mut teams = HashSet::new();

        self.rest_paginated(&Method::GET, format!("repos/{org}/{repo}/teams"), |resp| {
            let partial: Vec<Team> = resp.json()?;
            for team in partial {
                teams.insert(team.name);
            }
            Ok(())
        })?;

        Ok(teams)
    }

    pub(crate) fn collaborators(&self, org: &str, repo: &str) -> anyhow::Result<HashSet<String>> {
        let mut users = HashSet::new();

        #[derive(serde::Deserialize)]
        struct User {
            #[serde(alias = "login")]
            name: String,
        }

        self.rest_paginated(
            &Method::GET,
            format!("repos/{org}/{repo}/collaborators?affiliation=direct"),
            |resp| {
                let partial: Vec<User> = resp.json()?;
                for user in partial {
                    users.insert(user.name);
                }
                Ok(())
            },
        )?;

        Ok(users)
    }

    pub(crate) fn remove_team_from_repo(
        &self,
        org: &str,
        repo: &str,
        team: &str,
    ) -> anyhow::Result<()> {
        if !self.dry_run {
            self.req(
                Method::DELETE,
                &format!("orgs/{org}/teams/{team}/repos/{org}/{repo}"),
            )?
            .send()?
            .error_for_status()?;
        } else {
            debug!("dry: removing team {team} from repo {org}/{repo}")
        }
        Ok(())
    }

    pub(crate) fn remove_collaborator_from_repo(
        &self,
        org: &str,
        repo: &str,
        collaborator: &str,
    ) -> anyhow::Result<()> {
        if !self.dry_run {
            self.req(
                Method::DELETE,
                &format!("repos/{org}/{repo}/collaborators/{collaborator}"),
            )?
            .send()?
            .error_for_status()?;
        } else {
            debug!("dry: removing collaborator {collaborator} from repo {org}/{repo}")
        }
        Ok(())
    }

    /// Get the head commit of the supplied branch
    pub(crate) fn branch(&self, repo: &Repo, name: &str) -> anyhow::Result<Option<String>> {
        let resp = self
            .req(
                Method::GET,
                &format!("repos/{}/{}/branches/{}", repo.org, repo.name, name),
            )?
            .send()?;
        match resp.status() {
            StatusCode::OK => Ok(Some(resp.json::<Branch>()?.commit.sha)),
            StatusCode::NOT_FOUND => Ok(None),
            _ => Err(resp.error_for_status().unwrap_err().into()),
        }
    }

    pub(crate) fn create_branch(
        &self,
        repo: &Repo,
        name: &str,
        commit: &str,
    ) -> anyhow::Result<()> {
        #[derive(serde::Serialize)]
        struct Req<'a> {
            r#ref: &'a str,
            sha: &'a str,
        }
        if self.dry_run {
            debug!(
                "dry: created branch in {}/{}: {} with commit {}",
                repo.org, repo.name, name, commit
            );
            Ok(())
        } else {
            Ok(self
                .req(
                    Method::POST,
                    &format!("repos/{}/{}/git/refs", repo.org, repo.name),
                )?
                .json(&Req {
                    r#ref: &format!("refs/heads/{}", name),
                    sha: commit,
                })
                .send()?
                .error_for_status()?
                .json()?)
        }
    }

    /// Update the given branch's permissions.
    ///
    /// Returns `Ok(true)` on success, `Ok(false)` if the branch doesn't exist, and `Err(_)` otherwise.
    pub(crate) fn update_branch_protection(
        &self,
        repo: &Repo,
        branch_name: &str,
        branch_protection: BranchProtection,
    ) -> anyhow::Result<bool> {
        #[derive(serde::Serialize)]
        struct Req<'a> {
            required_status_checks: Req1<'a>,
            enforce_admins: bool,
            required_pull_request_reviews: Req2,
            restrictions: HashMap<String, Vec<String>>,
        }
        #[derive(serde::Serialize)]
        struct Req1<'a> {
            strict: bool,
            checks: Vec<Check<'a>>,
        }
        #[derive(serde::Serialize)]
        struct Check<'a> {
            context: &'a str,
        }
        #[derive(serde::Serialize)]
        struct Req2 {
            // Even though we don't want dismissal restrictions, it cannot be ommited
            dismissal_restrictions: HashMap<(), ()>,
            dismiss_stale_reviews: bool,
            required_approving_review_count: u8,
        }
        let req = Req {
            required_status_checks: Req1 {
                strict: false,
                checks: branch_protection
                    .required_checks
                    .iter()
                    .map(|c| Check {
                        context: c.as_str(),
                    })
                    .collect(),
            },
            enforce_admins: true,
            required_pull_request_reviews: Req2 {
                dismissal_restrictions: HashMap::new(),
                dismiss_stale_reviews: branch_protection.dismiss_stale_reviews,
                required_approving_review_count: branch_protection.required_approving_review_count,
            },
            restrictions: vec![
                ("users".to_string(), branch_protection.allowed_users),
                ("teams".to_string(), Vec::new()),
            ]
            .into_iter()
            .collect(),
        };
        if !self.dry_run {
            let resp = self
                .req(
                    Method::PUT,
                    &format!(
                        "repos/{}/{}/branches/{}/protection",
                        repo.org, repo.name, branch_name
                    ),
                )?
                .json(&req)
                .send()?;

            match resp.status() {
                StatusCode::OK => Ok(true),
                StatusCode::NOT_FOUND => Ok(false),
                _ => Err(resp.error_for_status().unwrap_err().into()),
            }
        } else {
            debug!(
                "dry: updating branch protection on repo {}/{} for {}: {}",
                repo.org,
                repo.name,
                branch_name,
                serde_json::to_string_pretty(&req).unwrap_or_else(|_| "<invalid json>".to_string())
            );
            Ok(true)
        }
    }

    pub(crate) fn protected_branches(&self, repo: &Repo) -> anyhow::Result<HashSet<String>> {
        let mut names = HashSet::new();
        self.rest_paginated(
            &Method::GET,
            format!("repos/{}/{}/branches?protected=true", repo.org, repo.name),
            |resp| {
                let resp = resp.error_for_status()?.json::<Vec<Branch>>()?;
                names.extend(resp.into_iter().map(|b| b.name));

                Ok(())
            },
        )?;
        Ok(names)
    }

    pub(crate) fn delete_branch_protection(&self, repo: &Repo, branch: &str) -> anyhow::Result<()> {
        if !self.dry_run {
            self.req(
                Method::DELETE,
                &format!(
                    "repos/{}/{}/branches/{}/protection",
                    repo.org, repo.name, branch
                ),
            )?
            .send()?
            .error_for_status()?;
        } else {
            debug!(
                "dry: removing branch protection in {}/{} from {} branch",
                repo.org, repo.name, branch
            );
        }
        Ok(())
    }

    /// Get all teams associated with a given org
    pub(crate) fn org_teams(&self, org: &str) -> anyhow::Result<HashSet<String>> {
        let mut teams = HashSet::new();

        self.rest_paginated(&Method::GET, format!("orgs/{org}/teams"), |resp| {
            let partial: Vec<Team> = resp.json()?;
            for team in partial {
                teams.insert(team.name);
            }
            Ok(())
        })?;

        Ok(teams)
    }

    /// Delete a team with the given name from inside the given org
    pub(crate) fn delete_team(&self, org: &str, team: &str) -> anyhow::Result<()> {
        if !self.dry_run {
            self.req(Method::DELETE, &format!("orgs/{}/teams/{}", org, team))?
                .send()?
                .error_for_status()?;
        } else {
            debug!("dry: deleting team '{}' from org '{}'", team, org);
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

#[derive(serde::Serialize, Debug)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RepoPermission {
    // While the GitHub UI uses the term 'write', the API still uses the older term 'push'
    #[serde(rename = "push")]
    Write,
    Admin,
    Maintain,
    Triage,
}

#[derive(serde::Deserialize, Debug)]
pub(crate) struct Repo {
    pub(crate) name: String,
    #[serde(alias = "owner", deserialize_with = "repo_owner")]
    pub(crate) org: String,
    pub(crate) description: String,
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
    base64::encode(&format!("04:User{}", id))
}

fn team_node_id(id: usize) -> String {
    base64::encode(&format!("04:Team{}", id))
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

#[derive(Debug)]
pub(crate) struct BranchProtection {
    pub(crate) dismiss_stale_reviews: bool,
    pub(crate) required_approving_review_count: u8,
    pub(crate) required_checks: Vec<String>,
    pub(crate) allowed_users: Vec<String>,
}
