use log::debug;
use reqwest::Method;

use crate::github::api::url::GitHubUrl;
use crate::github::api::{
    AppPushAllowanceActor, BranchProtection, BranchProtectionOp, HttpClient, Login,
    PushAllowanceActor, Repo, RepoPermission, RepoSettings, Team, TeamPrivacy,
    TeamPushAllowanceActor, TeamRole, UserPushAllowanceActor, allow_not_found,
};
use crate::utils::ResponseExt;

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
        debug!("Editing repo {}/{} with {:?}", org, repo_name, req);
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
        debug!("Updating '{}' branch protection", pattern);
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
        debug!("Removing protection in {}/{}", org, repo_name);
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
}
