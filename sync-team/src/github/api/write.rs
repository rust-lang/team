use log::debug;
use reqwest::Method;
use std::collections::HashSet;

use crate::github::api::url::GitHubUrl;
use crate::github::api::{
    AppPushAllowanceActor, BranchProtection, BranchProtectionOp, HttpClient, Login,
    PushAllowanceActor, Repo, RepoPermission, RepoSettings, Team, TeamPrivacy,
    TeamPushAllowanceActor, TeamRole, UserPushAllowanceActor, allow_not_found,
};
use crate::utils::ResponseExt;

#[derive(Debug)]
struct BranchPolicyInfo {
    id: u64,
    name: String,
    pattern_type: String,
}

pub(crate) struct GitHubWrite {
    client: HttpClient,
    dry_run: bool,
}

impl GitHubWrite {
    pub(crate) fn new(client: HttpClient, dry_run: bool) -> anyhow::Result<Self> {
        Ok(Self {
            client: client.clone(),
            dry_run,
        })
    }

    fn user_id(&self, name: &str, org: &str) -> anyhow::Result<String> {
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

        let data: Data = self.client.graphql(query, Params { name }, org)?;
        Ok(data.user.id)
    }

    fn team_id(&self, org: &str, name: &str) -> anyhow::Result<String> {
        #[derive(serde::Serialize)]
        struct Params<'a> {
            org: &'a str,
            team: &'a str,
        }
        let query = "
            query($org: String!, $team: String!) {
                organization(login: $org) {
                    team(slug: $team) {
                        id
                    }
                }
            }
        ";
        #[derive(serde::Deserialize)]
        struct Data {
            organization: Organization,
        }
        #[derive(serde::Deserialize)]
        struct Organization {
            team: Team,
        }
        #[derive(serde::Deserialize)]
        struct Team {
            id: String,
        }

        let data: Data = self
            .client
            .graphql(query, Params { org, team: name }, org)?;
        Ok(data.organization.team.id)
    }

    /// Resolve a team's database ID for use in rulesets
    /// Returns None if the team doesn't exist in the organization
    pub(crate) fn resolve_team_database_id(
        &self,
        org: &str,
        name: &str,
    ) -> anyhow::Result<Option<i64>> {
        #[derive(serde::Serialize)]
        struct Params<'a> {
            org: &'a str,
            team: &'a str,
        }
        let query = "
            query($org: String!, $team: String!) {
                organization(login: $org) {
                    team(slug: $team) {
                        databaseId
                    }
                }
            }
        ";
        #[derive(serde::Deserialize)]
        struct Data {
            organization: Option<Organization>,
        }
        #[derive(serde::Deserialize)]
        struct Organization {
            team: Option<Team>,
        }
        #[derive(serde::Deserialize)]
        struct Team {
            #[serde(rename = "databaseId")]
            database_id: Option<i64>,
        }

        let data: Data = self
            .client
            .graphql(query, Params { org, team: name }, org)?;

        Ok(data
            .organization
            .and_then(|org| org.team)
            .and_then(|team| team.database_id))
    }

    /// Resolve a user's database ID for use in rulesets
    /// Returns None if the user doesn't exist
    pub(crate) fn resolve_user_database_id(
        &self,
        login: &str,
        org: &str,
    ) -> anyhow::Result<Option<i64>> {
        #[derive(serde::Serialize)]
        struct Params<'a> {
            login: &'a str,
        }
        let query = "
            query($login: String!) {
                user(login: $login) {
                    databaseId
                }
            }
        ";
        #[derive(serde::Deserialize)]
        struct Data {
            user: Option<User>,
        }
        #[derive(serde::Deserialize)]
        struct User {
            #[serde(rename = "databaseId")]
            database_id: Option<i64>,
        }

        let data: Data = self.client.graphql(query, Params { login }, org)?;
        Ok(data.user.and_then(|u| u.database_id))
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
                description: Some(description.to_string()),
                privacy,
                slug: name.to_string(),
            })
        } else {
            let body = &Req {
                name,
                description,
                privacy,
            };
            Ok(self
                .client
                .send(Method::POST, &GitHubUrl::orgs(org, "teams")?, body)?
                .json_annotated()?)
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
            self.client.send(
                Method::PATCH,
                &GitHubUrl::orgs(org, &format!("teams/{name}"))?,
                &req,
            )?;
        }

        Ok(())
    }

    /// Delete a team by name and org
    pub(crate) fn delete_team(&self, org: &str, slug: &str) -> anyhow::Result<()> {
        debug!("Deleting team with slug '{slug}' in '{org}'");
        if !self.dry_run {
            let method = Method::DELETE;
            let url = GitHubUrl::orgs(org, &format!("teams/{slug}"))?;
            let resp = self.client.req(method.clone(), &url)?.send()?;
            allow_not_found(resp, method, url.url())?;
        }
        Ok(())
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
            self.client.send(
                Method::PUT,
                &GitHubUrl::orgs(org, &format!("teams/{team}/memberships/{user}"))?,
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
            let url = &GitHubUrl::orgs(org, &format!("teams/{team}/memberships/{user}"))?;
            let method = Method::DELETE;
            let resp = self.client.req(method.clone(), url)?.send()?;
            allow_not_found(resp, method, url.url())?;
        }

        Ok(())
    }

    /// Create a repo
    pub(crate) fn create_repo(
        &self,
        org: &str,
        name: &str,
        settings: &RepoSettings,
    ) -> anyhow::Result<Repo> {
        #[derive(serde::Serialize, Debug)]
        struct Req<'a> {
            name: &'a str,
            description: &'a str,
            homepage: &'a Option<&'a str>,
            auto_init: bool,
            allow_auto_merge: bool,
        }
        let req = &Req {
            name,
            description: &settings.description,
            homepage: &settings.homepage.as_deref(),
            auto_init: true,
            allow_auto_merge: settings.auto_merge_enabled,
        };
        debug!("Creating the repo {org}/{name} with {req:?}");
        if self.dry_run {
            Ok(Repo {
                node_id: String::from("ID"),
                name: name.to_string(),
                org: org.to_string(),
                description: settings.description.clone(),
                homepage: settings.homepage.clone(),
                archived: false,
                allow_auto_merge: Some(settings.auto_merge_enabled),
            })
        } else {
            Ok(self
                .client
                .send(Method::POST, &GitHubUrl::orgs(org, "repos")?, req)?
                .json_annotated()?)
        }
    }

    pub(crate) fn edit_repo(
        &self,
        org: &str,
        repo_name: &str,
        settings: &RepoSettings,
    ) -> anyhow::Result<()> {
        #[derive(serde::Serialize, Debug)]
        struct Req<'a> {
            description: &'a str,
            homepage: &'a Option<&'a str>,
            archived: bool,
            allow_auto_merge: bool,
        }
        let req = Req {
            description: &settings.description,
            homepage: &settings.homepage.as_deref(),
            archived: settings.archived,
            allow_auto_merge: settings.auto_merge_enabled,
        };
        debug!("Editing repo {org}/{repo_name} with {req:?}");
        if !self.dry_run {
            self.client
                .send(Method::PATCH, &GitHubUrl::repos(org, repo_name, "")?, &req)?;
        }
        Ok(())
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
            self.client.send(
                Method::PUT,
                &GitHubUrl::orgs(org, &format!("teams/{team}/repos/{org}/{repo}"))?,
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
            self.client.send(
                Method::PUT,
                &GitHubUrl::repos(org, repo, &format!("collaborators/{user}"))?,
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
            let method = Method::DELETE;
            let url = GitHubUrl::orgs(org, &format!("teams/{team}/repos/{org}/{repo}"))?;
            let resp = self.client.req(method.clone(), &url)?.send()?;
            allow_not_found(resp, method, url.url())?;
        }

        Ok(())
    }

    /// Remove a member from an org
    pub(crate) fn remove_gh_member_from_org(&self, org: &str, user: &str) -> anyhow::Result<()> {
        debug!("Removing user {user} from org {org}");
        if !self.dry_run {
            let method = Method::DELETE;
            let url = GitHubUrl::orgs(org, &format!("members/{user}"))?;
            let resp = self.client.req(method.clone(), &url)?.send()?;
            allow_not_found(resp, method, url.url())?;
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
            let method = Method::DELETE;
            let url = &GitHubUrl::repos(org, repo, &format!("collaborators/{collaborator}"))?;
            let resp = self.client.req(method.clone(), url)?.send()?;
            allow_not_found(resp, method, url.url())?;
        }
        Ok(())
    }

    /// Create or update a branch protection.
    pub(crate) fn upsert_branch_protection(
        &self,
        op: BranchProtectionOp,
        pattern: &str,
        branch_protection: &BranchProtection,
        org: &str,
    ) -> anyhow::Result<()> {
        debug!("Updating '{pattern}' branch protection");
        #[derive(Debug, serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Params<'a> {
            id: &'a str,
            pattern: &'a str,
            contexts: &'a [String],
            dismiss_stale: bool,
            review_count: u8,
            restricts_pushes: bool,
            // Is a PR required to push into this branch?
            requires_approving_reviews: bool,
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
        mutation($id: ID!, $pattern:String!, $contexts: [String!], $dismissStale: Boolean, $reviewCount: Int, $pushActorIds: [ID!], $restrictsPushes: Boolean, $requiresApprovingReviews: Boolean) {{
            {mutation_name}(input: {{
                {id_field}: $id,
                pattern: $pattern,
                requiresStatusChecks: true,
                requiredStatusCheckContexts: $contexts,
                # Disable 'Require branch to be up-to-date before merging'
                requiresStrictStatusChecks: false,
                isAdminEnforced: true,
                requiredApprovingReviewCount: $reviewCount,
                dismissesStaleReviews: $dismissStale,
                requiresApprovingReviews: $requiresApprovingReviews,
                restrictsPushes: $restrictsPushes,
                pushActorIds: $pushActorIds
            }}) {{
              branchProtectionRule {{
                id
              }}
            }}
          }}
        ");
        let mut push_actor_ids = vec![];
        for actor in &branch_protection.push_allowances {
            match actor {
                PushAllowanceActor::User(UserPushAllowanceActor { login: name }) => {
                    push_actor_ids.push(self.user_id(name, org)?);
                }
                PushAllowanceActor::Team(TeamPushAllowanceActor {
                    organization: Login { login: org },
                    name,
                }) => push_actor_ids.push(self.team_id(org, name)?),
                PushAllowanceActor::App(AppPushAllowanceActor { id, .. }) => {
                    push_actor_ids.push(id.clone())
                }
            }
        }

        if !self.dry_run {
            let _: serde_json::Value = self.client.graphql(
                &query,
                Params {
                    id,
                    pattern,
                    contexts: &branch_protection.required_status_check_contexts,
                    dismiss_stale: branch_protection.dismisses_stale_reviews,
                    review_count: branch_protection.required_approving_review_count,
                    // We restrict merges, if we have explicitly set some actors to be
                    // able to merge (i.e., we allow allow those with write permissions
                    // to merge *or* we only allow those in `push_actor_ids`)
                    restricts_pushes: !push_actor_ids.is_empty(),
                    push_actor_ids: &push_actor_ids,
                    requires_approving_reviews: branch_protection.requires_approving_reviews,
                },
                org,
            )?;
        }
        Ok(())
    }

    /// Delete a branch protection
    pub(crate) fn delete_branch_protection(
        &self,
        org: &str,
        repo_name: &str,
        id: &str,
    ) -> anyhow::Result<()> {
        debug!("Removing protection in {org}/{repo_name}");
        println!("Remove protection {id}");
        if !self.dry_run {
            #[derive(serde::Serialize)]
            #[serde(rename_all = "camelCase")]
            struct Params<'a> {
                id: &'a str,
            }
            let query = "
                mutation($id: ID!) {
                    deleteBranchProtectionRule(input: { branchProtectionRuleId: $id }) {
                        clientMutationId
                    }
                }
            ";
            let _: serde_json::Value = self.client.graphql(query, Params { id }, org)?;
        }
        Ok(())
    }

    /// Create an environment in a repository
    pub(crate) fn create_environment(
        &self,
        org: &str,
        repo: &str,
        name: &str,
        branches: &[String],
        tags: &[String],
    ) -> anyhow::Result<()> {
        debug!(
            "Creating environment '{name}' in '{org}/{repo}' with branches: {:?}, tags: {:?}",
            branches, tags
        );
        self.upsert_environment(org, repo, name, branches, tags)
    }

    /// Update an environment in a repository
    pub(crate) fn update_environment(
        &self,
        org: &str,
        repo: &str,
        name: &str,
        branches: &[String],
        tags: &[String],
    ) -> anyhow::Result<()> {
        debug!(
            "Updating environment '{name}' in '{org}/{repo}' with branches: {:?}, tags: {:?}",
            branches, tags
        );
        self.upsert_environment(org, repo, name, branches, tags)
    }

    /// Internal helper to create or update an environment
    fn upsert_environment(
        &self,
        org: &str,
        repo: &str,
        name: &str,
        branches: &[String],
        tags: &[String],
    ) -> anyhow::Result<()> {
        if !self.dry_run {
            // REST API: PUT /repos/{owner}/{repo}/environments/{environment_name}
            // https://docs.github.com/en/rest/deployments/environments#create-or-update-an-environment
            let url = GitHubUrl::repos(org, repo, &format!("environments/{}", name))?;

            let body = if branches.is_empty() && tags.is_empty() {
                serde_json::json!({
                    "deployment_branch_policy": null
                })
            } else {
                serde_json::json!({
                    "deployment_branch_policy": {
                        "protected_branches": false,
                        "custom_branch_policies": true
                    }
                })
            };

            self.client.send(Method::PUT, &url, &body)?;

            // Always sync branch/tag policies to ensure cleanup of old policies
            self.set_environment_deployment_patterns(org, repo, name, branches, tags)?;
        }
        Ok(())
    }

    /// Get existing branch policies for an environment
    fn get_environment_branch_policies(
        &self,
        org: &str,
        repo: &str,
        environment: &str,
    ) -> anyhow::Result<Vec<BranchPolicyInfo>> {
        #[derive(serde::Deserialize)]
        struct BranchPolicy {
            id: u64,
            name: String,
            #[serde(rename = "type", default = "default_branch_type")]
            pattern_type: String,
        }

        fn default_branch_type() -> String {
            "branch".to_string()
        }

        #[derive(serde::Deserialize)]
        struct BranchPoliciesResponse {
            branch_policies: Vec<BranchPolicy>,
        }

        let url = GitHubUrl::repos(
            org,
            repo,
            &format!("environments/{}/deployment-branch-policies", environment),
        )?;

        let response: BranchPoliciesResponse =
            self.client.req(Method::GET, &url)?.send()?.json()?;

        Ok(response
            .branch_policies
            .into_iter()
            .map(|bp| BranchPolicyInfo {
                id: bp.id,
                name: bp.name,
                pattern_type: bp.pattern_type,
            })
            .collect())
    }

    /// Delete a specific branch policy by ID
    fn delete_environment_branch_policy(
        &self,
        org: &str,
        repo: &str,
        environment: &str,
        policy_id: u64,
    ) -> anyhow::Result<()> {
        let url = GitHubUrl::repos(
            org,
            repo,
            &format!(
                "environments/{}/deployment-branch-policies/{}",
                environment, policy_id
            ),
        )?;
        self.client
            .send(Method::DELETE, &url, &serde_json::json!({}))?;
        Ok(())
    }

    /// Set custom deployment patterns (branch/tag policies) for an environment
    /// This method properly handles updates by:
    /// 1. Fetching all existing policies
    /// 2. Deleting policies that are no longer needed
    /// 3. Adding new policies that don't exist
    fn set_environment_deployment_patterns(
        &self,
        org: &str,
        repo: &str,
        environment: &str,
        branches: &[String],
        tags: &[String],
    ) -> anyhow::Result<()> {
        // 1. Fetch existing policies
        let existing_policies = self.get_environment_branch_policies(org, repo, environment)?;

        #[derive(Hash, Eq, PartialEq)]
        struct PatternKey {
            name: String,
            pattern_type: String,
        }

        let existing_patterns: HashSet<PatternKey> = existing_policies
            .iter()
            .map(|p| PatternKey {
                name: p.name.clone(),
                pattern_type: p.pattern_type.clone(),
            })
            .collect();

        let mut new_patterns = HashSet::new();
        for branch in branches {
            new_patterns.insert(PatternKey {
                name: branch.clone(),
                pattern_type: "branch".to_string(),
            });
        }
        for tag in tags {
            new_patterns.insert(PatternKey {
                name: tag.clone(),
                pattern_type: "tag".to_string(),
            });
        }

        // 2. Delete policies that are no longer needed
        for policy in &existing_policies {
            let key = PatternKey {
                name: policy.name.clone(),
                pattern_type: policy.pattern_type.clone(),
            };
            if !new_patterns.contains(&key) {
                debug!(
                    "Deleting deployment policy '{}' (type: {}, id: {}) from environment '{}' in '{}/{}'",
                    policy.name, policy.pattern_type, policy.id, environment, org, repo
                );
                self.delete_environment_branch_policy(org, repo, environment, policy.id)?;
            }
        }

        // 3. Add new branch policies that don't exist yet
        for branch in branches {
            let key = PatternKey {
                name: branch.clone(),
                pattern_type: "branch".to_string(),
            };
            if !existing_patterns.contains(&key) {
                debug!(
                    "Adding branch pattern '{}' to environment '{}' in '{}/{}'",
                    branch, environment, org, repo
                );
                let url = GitHubUrl::repos(
                    org,
                    repo,
                    &format!("environments/{}/deployment-branch-policies", environment),
                )?;
                self.client.send(
                    Method::POST,
                    &url,
                    &serde_json::json!({
                        "name": branch,
                        "type": "branch"
                    }),
                )?;
            }
        }

        // 4. Add new tag policies that don't exist yet
        for tag in tags {
            let key = PatternKey {
                name: tag.clone(),
                pattern_type: "tag".to_string(),
            };
            if !existing_patterns.contains(&key) {
                debug!(
                    "Adding tag pattern '{}' to environment '{}' in '{}/{}'",
                    tag, environment, org, repo
                );
                let url = GitHubUrl::repos(
                    org,
                    repo,
                    &format!("environments/{}/deployment-branch-policies", environment),
                )?;
                self.client.send(
                    Method::POST,
                    &url,
                    &serde_json::json!({
                        "name": tag,
                        "type": "tag"
                    }),
                )?;
            }
        }
        Ok(())
    }

    /// Delete an environment from a repository
    pub(crate) fn delete_environment(
        &self,
        org: &str,
        repo: &str,
        name: &str,
    ) -> anyhow::Result<()> {
        debug!("Deleting environment '{name}' from '{org}/{repo}'");
        if !self.dry_run {
            // REST API: DELETE /repos/{owner}/{repo}/environments/{environment_name}
            // https://docs.github.com/en/rest/deployments/environments#delete-an-environment
            let url = GitHubUrl::repos(org, repo, &format!("environments/{}", name))?;
            self.client
                .send(Method::DELETE, &url, &serde_json::json!({}))?;
        }
        Ok(())
    }

    /// Create or update a ruleset for a repository
    pub(crate) fn upsert_ruleset(
        &self,
        op: crate::github::api::RulesetOp,
        org: &str,
        repo: &str,
        ruleset: &crate::github::api::Ruleset,
    ) -> anyhow::Result<()> {
        use crate::github::api::RulesetOp;

        match op {
            RulesetOp::CreateForRepo => {
                debug!("Creating ruleset '{}' in '{}/{}'", ruleset.name, org, repo);
                if !self.dry_run {
                    // REST API: POST /repos/{owner}/{repo}/rulesets
                    // https://docs.github.com/en/rest/repos/rules#create-a-repository-ruleset
                    let url = GitHubUrl::repos(org, repo, "rulesets")?;
                    self.client.send(Method::POST, &url, ruleset)?;
                }
            }
            RulesetOp::UpdateRuleset(id) => {
                debug!(
                    "Updating ruleset '{}' (id: {}) in '{}/{}'",
                    ruleset.name, id, org, repo
                );
                if !self.dry_run {
                    // REST API: PUT /repos/{owner}/{repo}/rulesets/{ruleset_id}
                    // https://docs.github.com/en/rest/repos/rules#update-a-repository-ruleset
                    let url = GitHubUrl::repos(org, repo, &format!("rulesets/{}", id))?;
                    self.client.send(Method::PUT, &url, ruleset)?;
                }
            }
        }
        Ok(())
    }

    /// Delete a ruleset from a repository
    pub(crate) fn delete_ruleset(&self, org: &str, repo: &str, id: i64) -> anyhow::Result<()> {
        debug!("Deleting ruleset id {} from '{}/{}'", id, org, repo);
        if !self.dry_run {
            // REST API: DELETE /repos/{owner}/{repo}/rulesets/{ruleset_id}
            // https://docs.github.com/en/rest/repos/rules#delete-a-repository-ruleset
            let url = GitHubUrl::repos(org, repo, &format!("rulesets/{}", id))?;
            self.client
                .send(Method::DELETE, &url, &serde_json::json!({}))?;
        }
        Ok(())
    }
}
