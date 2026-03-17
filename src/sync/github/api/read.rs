use crate::sync::github::api;
use crate::sync::github::api::{BranchPolicy, Ruleset};
use crate::sync::github::api::{
    BranchProtection, GraphNode, GraphNodes, GraphPageInfo, HttpClient, Login, Repo, RepoTeam,
    RepoUser, RestPaginatedError, Team, TeamMember, TeamRole, team_node_id, url::GitHubUrl,
    user_node_id,
};
use crate::sync::utils::ResponseExt;
use anyhow::Context as _;
use async_trait::async_trait;
use reqwest::{Method, StatusCode};
use rust_team_data::v1::Environment;
use std::collections::{HashMap, HashSet};

#[async_trait]
pub(crate) trait GithubRead {
    fn uses_pat(&self) -> bool;

    /// Get user names by user ids
    async fn usernames(&self, ids: &[u64]) -> anyhow::Result<HashMap<u64, String>>;

    /// Get the owners of an org
    async fn org_owners(&self, org: &str) -> anyhow::Result<HashSet<u64>>;

    /// Get the members of an org
    async fn org_members(&self, org: &str) -> anyhow::Result<HashMap<u64, String>>;

    /// Get all teams associated with a org
    ///
    /// Returns a list of tuples of team name and slug
    async fn org_teams(&self, org: &str) -> anyhow::Result<Vec<(String, String)>>;

    /// Get the team by name and org
    async fn team(&self, org: &str, team: &str) -> anyhow::Result<Option<Team>>;

    async fn team_memberships(
        &self,
        team: &Team,
        org: &str,
    ) -> anyhow::Result<HashMap<u64, TeamMember>>;

    /// The GitHub names of users invited to the given team
    async fn team_membership_invitations(
        &self,
        org: &str,
        team: &str,
    ) -> anyhow::Result<HashSet<String>>;

    /// Get a repo by org and name
    async fn repo(&self, org: &str, repo: &str) -> anyhow::Result<Option<Repo>>;

    /// Get teams in a repo
    async fn repo_teams(&self, org: &str, repo: &str) -> anyhow::Result<Vec<RepoTeam>>;

    /// Get collaborators in a repo
    ///
    /// Only fetches those who are direct collaborators (i.e., not a collaborator through a repo team)
    async fn repo_collaborators(&self, org: &str, repo: &str) -> anyhow::Result<Vec<RepoUser>>;

    /// Get branch_protections
    /// Returns a map branch pattern -> (protection ID, protection data)
    async fn branch_protections(
        &self,
        org: &str,
        repo: &str,
    ) -> anyhow::Result<HashMap<String, (String, BranchProtection)>>;

    /// Get environments for a repository
    /// Returns a map of environment names to their Environment data
    async fn repo_environments(
        &self,
        org: &str,
        repo: &str,
    ) -> anyhow::Result<HashMap<String, Environment>>;

    /// Get rulesets for a repository
    /// Returns a vector of rulesets
    async fn repo_rulesets(&self, org: &str, repo: &str) -> anyhow::Result<Vec<Ruleset>>;

    async fn environment_branch_policies(
        &self,
        org: &str,
        repo: &str,
        environment: &str,
    ) -> anyhow::Result<Vec<BranchPolicy>>;
}

pub(crate) struct GitHubApiRead {
    client: HttpClient,
}

impl GitHubApiRead {
    pub(crate) fn from_client(client: HttpClient) -> anyhow::Result<Self> {
        Ok(Self { client })
    }
}

#[async_trait]
impl GithubRead for GitHubApiRead {
    fn uses_pat(&self) -> bool {
        self.client.uses_pat()
    }

    async fn usernames(&self, ids: &[u64]) -> anyhow::Result<HashMap<u64, String>> {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Usernames {
            database_id: u64,
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
            let res: GraphNodes<Usernames> = self
                .client
                .graphql(
                    QUERY,
                    Params {
                        ids: chunk.iter().map(|id| user_node_id(*id)).collect(),
                    },
                    "rust-lang", // any of our orgs will work for this query
                )
                .await?;
            for node in res.nodes.into_iter().flatten() {
                result.insert(node.database_id, node.login);
            }
        }
        Ok(result)
    }

    async fn org_owners(&self, org: &str) -> anyhow::Result<HashSet<u64>> {
        #[derive(serde::Deserialize, Eq, PartialEq, Hash)]
        struct User {
            id: u64,
        }
        let mut owners = HashSet::new();
        self.client
            .rest_paginated(
                &Method::GET,
                &GitHubUrl::orgs(org, "members?role=admin")?,
                |resp: Vec<User>| {
                    owners.extend(resp.into_iter().map(|u| u.id));
                    Ok(())
                },
            )
            .await?;
        Ok(owners)
    }

    async fn org_members(&self, org: &str) -> anyhow::Result<HashMap<u64, String>> {
        #[derive(serde::Deserialize, Eq, PartialEq, Hash)]
        struct User {
            id: u64,
            login: String,
        }
        let mut members = HashMap::new();
        self.client
            .rest_paginated(
                &Method::GET,
                &GitHubUrl::orgs(org, "members")?,
                |resp: Vec<User>| {
                    for user in resp {
                        members.insert(user.id, user.login);
                    }
                    Ok(())
                },
            )
            .await?;
        Ok(members)
    }

    async fn org_teams(&self, org: &str) -> anyhow::Result<Vec<(String, String)>> {
        let mut teams = Vec::new();

        self.client
            .rest_paginated(
                &Method::GET,
                &GitHubUrl::orgs(org, "teams")?,
                |resp: Vec<Team>| {
                    teams.extend(resp.into_iter().map(|t| (t.name, t.slug)));
                    Ok(())
                },
            )
            .await?;

        Ok(teams)
    }

    async fn team(&self, org: &str, team: &str) -> anyhow::Result<Option<Team>> {
        self.client
            .send_option(
                Method::GET,
                &GitHubUrl::orgs(org, &format!("teams/{team}"))?,
            )
            .await
    }

    async fn team_memberships(
        &self,
        team: &Team,
        org: &str,
    ) -> anyhow::Result<HashMap<u64, TeamMember>> {
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
            database_id: u64,
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
                let res: GraphNode<RespTeam> = self
                    .client
                    .graphql(
                        QUERY,
                        Params {
                            team: team_node_id(id),
                            cursor: page_info.end_cursor.as_deref(),
                        },
                        org,
                    )
                    .await?;
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

    async fn team_membership_invitations(
        &self,
        org: &str,
        team: &str,
    ) -> anyhow::Result<HashSet<String>> {
        let mut invites = HashSet::new();

        self.client
            .rest_paginated(
                &Method::GET,
                &GitHubUrl::orgs(org, &format!("teams/{team}/invitations"))?,
                |resp: Vec<Login>| {
                    invites.extend(resp.into_iter().map(|l| l.login));
                    Ok(())
                },
            )
            .await?;

        Ok(invites)
    }

    async fn repo(&self, org: &str, repo: &str) -> anyhow::Result<Option<Repo>> {
        // We use the GraphQL API instead of REST because of
        // this bug: https://github.com/orgs/community/discussions/153258
        #[derive(serde::Serialize)]
        struct Params<'a> {
            owner: &'a str,
            name: &'a str,
        }

        static QUERY: &str = r#"
            query($owner: String!, $name: String!) {
                repository(owner: $owner, name: $name) {
                    id
                    autoMergeAllowed
                    description
                    homepageUrl
                    isArchived
                    isPrivate
                }
            }
        "#;

        #[derive(serde::Deserialize)]
        struct Wrapper {
            repository: Option<RepoResponse>,
        }

        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct RepoResponse {
            // Equivalent of `node_id` of the Rest API
            id: String,
            // Equivalent of `id` of the Rest API
            auto_merge_allowed: Option<bool>,
            description: Option<String>,
            homepage_url: Option<String>,
            is_archived: bool,
            is_private: bool,
        }

        let result: Option<Wrapper> = self
            .client
            .graphql_opt(
                QUERY,
                Params {
                    owner: org,
                    name: repo,
                },
                org,
            )
            .await
            .with_context(|| format!("failed to retrieve repo `{org}/{repo}`"))?;

        let repo = result.and_then(|r| r.repository).map(|repo_response| Repo {
            node_id: repo_response.id,
            name: repo.to_string(),
            description: repo_response.description.unwrap_or_default(),
            allow_auto_merge: repo_response.auto_merge_allowed,
            archived: repo_response.is_archived,
            homepage: repo_response.homepage_url,
            org: org.to_string(),
            private: repo_response.is_private,
        });

        Ok(repo)
    }

    async fn repo_teams(&self, org: &str, repo: &str) -> anyhow::Result<Vec<RepoTeam>> {
        let mut teams = Vec::new();

        self.client
            .rest_paginated(
                &Method::GET,
                &GitHubUrl::repos(org, repo, "teams")?,
                |resp: Vec<RepoTeam>| {
                    teams.extend(resp);
                    Ok(())
                },
            )
            .await?;

        Ok(teams)
    }

    async fn repo_collaborators(&self, org: &str, repo: &str) -> anyhow::Result<Vec<RepoUser>> {
        let mut users = Vec::new();

        self.client
            .rest_paginated(
                &Method::GET,
                &GitHubUrl::repos(org, repo, "collaborators?affiliation=direct")?,
                |resp: Vec<RepoUser>| {
                    users.extend(resp);
                    Ok(())
                },
            )
            .await?;

        Ok(users)
    }

    async fn branch_protections(
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
                            allowsForcePushes,
                            dismissesStaleReviews,
                            requiredStatusCheckContexts,
                            requiredApprovingReviewCount,
                            requiresApprovingReviews
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
                                        ... on App {
                                            id,
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
            repository: Repository,
        }
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Repository {
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
        let res: Wrapper = self
            .client
            .graphql(QUERY, Params { org, repo }, org)
            .await?;
        for mut node in res
            .repository
            .branch_protection_rules
            .nodes
            .into_iter()
            .flatten()
        {
            // Normalize check order to avoid diffs based only on the ordering difference
            node.protection.required_status_check_contexts.sort();
            result.insert(node.protection.pattern.clone(), (node.id, node.protection));
        }
        Ok(result)
    }

    async fn repo_environments(
        &self,
        org: &str,
        repo: &str,
    ) -> anyhow::Result<HashMap<String, Environment>> {
        #[derive(serde::Deserialize)]
        struct ProtectionRule {
            #[serde(rename = "type")]
            rule_type: String,
        }

        #[derive(serde::Deserialize)]
        struct GitHubEnvironment {
            name: String,
            protection_rules: Vec<ProtectionRule>,
        }

        #[derive(serde::Deserialize)]
        struct EnvironmentsResponse {
            environments: Vec<GitHubEnvironment>,
        }

        let mut env_infos = Vec::new();

        // Fetch all environments with their protection_rules metadata
        // REST API: https://docs.github.com/en/rest/deployments/environments#list-environments
        self.client
            .rest_paginated(
                &Method::GET,
                &GitHubUrl::repos(org, repo, "environments")?,
                |resp: EnvironmentsResponse| {
                    env_infos.extend(resp.environments);
                    Ok(())
                },
            )
            .await?;

        use futures_util::StreamExt;

        // For each environment, fetch deployment branch policies if they exist
        // REST API: https://docs.github.com/en/rest/deployments/branch-policies#list-deployment-branch-policies
        futures_util::stream::iter(env_infos)
            .then(|env_info| async move {
                // Check if branch policies exist by looking at protection_rules metadata
                let has_branch_policies = env_info
                    .protection_rules
                    .iter()
                    .any(|rule| rule.rule_type == "branch_policy");

                let (branches, tags) = if has_branch_policies {
                    let mut branches = Vec::new();
                    let mut tags = Vec::new();
                    let policies =
                        self.environment_branch_policies(org, repo, &env_info.name).await.with_context(
                            || {
                                format!(
                                    "failed to load deployment branch policies for environment '{}' in '{org}/{repo}'",
                                    env_info.name
                                )
                            },
                        )?;
                    for p in policies {
                        match p.pattern_type.as_str() {
                            "tag" => tags.push(p.name),
                            _ => branches.push(p.name),
                        }
                    }
                    (branches, tags)
                } else {
                    (Vec::new(), Vec::new())
                };

                Ok((env_info.name, Environment { branches, tags }))
            })
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<anyhow::Result<HashMap<_, _>>>()
    }

    async fn repo_rulesets(&self, org: &str, repo: &str) -> anyhow::Result<Vec<Ruleset>> {
        #[derive(serde::Deserialize)]
        struct RulesetInfo {
            id: u64,
        }

        let mut ruleset_ids = vec![];

        // REST API endpoint for rulesets
        // https://docs.github.com/en/rest/repos/rules#get-all-repository-rulesets
        // The API returns only a subset of data for each ruleset :/
        // So we then have to fetch the rulesets individually to get the full data.
        self.client
            .rest_paginated(
                &Method::GET,
                &GitHubUrl::repos(org, repo, "rulesets")?,
                |resp: Vec<RulesetInfo>| {
                    ruleset_ids.extend(resp.into_iter().map(|info| info.id));
                    Ok(())
                },
            )
            .await?;

        let mut rulesets: Vec<Ruleset> = vec![];
        for id in ruleset_ids {
            let ruleset: api::Ruleset = self
                .client
                .req(
                    Method::GET,
                    &GitHubUrl::repos(org, repo, &format!("rulesets/{id}"))?,
                )?
                .send()
                .await?
                .json_annotated()
                .await?;
            rulesets.push(ruleset);
        }

        Ok(rulesets)
    }

    async fn environment_branch_policies(
        &self,
        org: &str,
        repo: &str,
        environment: &str,
    ) -> anyhow::Result<Vec<BranchPolicy>> {
        #[derive(serde::Deserialize)]
        struct BranchPoliciesResponse {
            branch_policies: Vec<BranchPolicy>,
        }

        let mut policies = Vec::new();
        let url = GitHubUrl::repos(
            org,
            repo,
            &format!("environments/{environment}/deployment-branch-policies"),
        )?;

        if let Err(err) = self
            .client
            .rest_paginated(&Method::GET, &url, |resp: BranchPoliciesResponse| {
                policies.extend(resp.branch_policies);
                Ok(())
            })
            .await
        {
            match err {
                // If the environment doesn't have branch policies, GitHub returns a 404.
                // In this case, we return an empty list of policies.
                RestPaginatedError::Http {
                    status: StatusCode::NOT_FOUND,
                    ..
                } => return Ok(policies),
                other => {
                    return Err(anyhow::Error::from(other)).with_context(|| {
                        format!(
                            "failed to fetch deployment branch policies for environment '{environment}' in '{org}/{repo}'"
                        )
                    });
                }
            }
        }

        Ok(policies)
    }
}
