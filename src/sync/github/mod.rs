mod api;
#[cfg(test)]
mod tests;

use self::api::{BranchProtectionOp, TeamPrivacy, TeamRole};
pub(crate) use self::api::{GitHubApiRead, GitHubWrite, HttpClient};
use crate::schema;
use crate::sync::Config;
use crate::sync::github::api::{
    GithubRead, Login, PushAllowanceActor, RepoPermission, RepoSettings, Ruleset,
};
use anyhow::Context as _;
use futures_util::StreamExt;
use log::debug;
use rust_team_data::v1::{Bot, BranchProtectionMode, MergeBot, ProtectionTarget};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::{Display, Formatter, Write};

static DEFAULT_DESCRIPTION: &str = "Managed by the rust-lang/team repository.";
static DEFAULT_PRIVACY: TeamPrivacy = TeamPrivacy::Closed;

/// GitHub Actions integration ID
/// Verified via: https://api.github.com/repos/rust-lang/rust/commits/HEAD/check-runs
const GITHUB_ACTIONS_INTEGRATION_ID: i64 = 15368;

const REQUIRE_PULL_REQUESTS_DEFAULT: bool = true;
const REQUIRE_CODE_OWNER_REVIEW_DEFAULT: bool = false;
const REQUIRE_LAST_PUSH_APPROVAL_DEFAULT: bool = false;
const REQUIRED_REVIEW_THREAD_RESOLUTION_DEFAULT: bool = false;
const STRICT_REQUIRED_STATUS_CHECKS_POLICY_DEFAULT: bool = false;

pub(crate) async fn create_diff(
    github: Box<dyn GithubRead>,
    teams: Vec<rust_team_data::v1::Team>,
    repos: Vec<rust_team_data::v1::Repo>,
    config: Config,
) -> anyhow::Result<Diff> {
    let github = SyncGitHub::new(github, teams, repos, config).await?;
    github.diff_all().await
}

type OrgName = String;
type RepoName = String;

#[derive(Copy, Clone, Debug, PartialEq)]
enum GithubApp {
    RenovateBot,
    ForkingRenovateBot,
    Bors,
}

impl GithubApp {
    /// You can find the GitHub app ID e.g. through `gh api apps/<name>` or through the
    /// app settings page (if we own the app).
    fn from_id(app_id: u64) -> Option<Self> {
        match app_id {
            2740 => Some(GithubApp::RenovateBot),
            7402 => Some(GithubApp::ForkingRenovateBot),
            278306 => Some(GithubApp::Bors),
            _ => None,
        }
    }
}

impl Display for GithubApp {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            GithubApp::ForkingRenovateBot => f.write_str("Forking RenovateBot"),
            GithubApp::RenovateBot => f.write_str("RenovateBot"),
            GithubApp::Bors => f.write_str("Bors"),
        }
    }
}

#[derive(Clone, Debug)]
struct OrgAppInstallation {
    app: GithubApp,
    installation_id: u64,
    repositories: HashSet<RepoName>,
}

#[derive(Clone, Debug, PartialEq)]
struct AppInstallation {
    app: GithubApp,
    installation_id: u64,
}

struct SyncGitHub {
    github: Box<dyn GithubRead>,
    teams: Vec<rust_team_data::v1::Team>,
    repos: Vec<rust_team_data::v1::Repo>,
    config: Config,
    usernames_cache: HashMap<u64, String>,
    org_owners: HashMap<OrgName, HashSet<u64>>,
    org_members: HashMap<OrgName, HashMap<u64, String>>,
    org_apps: HashMap<OrgName, Vec<OrgAppInstallation>>,
}

impl SyncGitHub {
    pub(crate) async fn new(
        github: Box<dyn GithubRead>,
        teams: Vec<rust_team_data::v1::Team>,
        repos: Vec<rust_team_data::v1::Repo>,
        config: Config,
    ) -> anyhow::Result<Self> {
        debug!("caching mapping between user ids and usernames");
        let users = teams
            .iter()
            .filter_map(|t| t.github.as_ref().map(|gh| &gh.teams))
            .flatten()
            .flat_map(|team| &team.members)
            .copied()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let usernames_cache = github.usernames(&users).await?;

        debug!("caching organization owners");
        let orgs = teams
            .iter()
            .filter_map(|t| t.github.as_ref())
            .flat_map(|gh| &gh.teams)
            .map(|gh_team| &gh_team.org)
            .collect::<HashSet<_>>();

        let mut org_owners = HashMap::new();
        let mut org_members = HashMap::new();
        let mut org_apps = HashMap::new();

        for org in &orgs {
            org_owners.insert((*org).to_string(), github.org_owners(org).await?);
            org_members.insert((*org).to_string(), github.org_members(org).await?);

            let mut installations: Vec<OrgAppInstallation> = vec![];

            // We cannot get app installations for organizations outside the rust-lang enterprise
            // with an GitHub App token authentication, so we skip those.
            if !github.uses_pat() && config.independent_github_orgs.contains(org.as_str()) {
                continue;
            }

            for installation in github.org_app_installations(org).await? {
                // Only load installations from apps that we know about, to avoid removing
                // installations of unknown apps.
                if let Some(app) = GithubApp::from_id(installation.app_id) {
                    let mut repositories = HashSet::new();
                    for repo_installation in github
                        .app_installation_repos(installation.installation_id, org)
                        .await?
                    {
                        repositories.insert(repo_installation.name);
                    }
                    installations.push(OrgAppInstallation {
                        app,
                        installation_id: installation.installation_id,
                        repositories,
                    });
                }
            }
            org_apps.insert(org.to_string(), installations);
        }

        Ok(SyncGitHub {
            github,
            teams,
            repos,
            config,
            usernames_cache,
            org_owners,
            org_members,
            org_apps,
        })
    }

    pub(crate) async fn diff_all(&self) -> anyhow::Result<Diff> {
        let team_diffs = self.diff_teams().await?;
        let repo_diffs = self.diff_repos().await?;
        let org_membership_diffs = self.diff_org_memberships().await?;

        Ok(Diff {
            team_diffs,
            repo_diffs,
            org_membership_diffs,
        })
    }

    /// Collect all org members from the respective teams
    fn get_org_members_from_teams(&self) -> HashMap<OrgName, HashSet<u64>> {
        let mut org_team_members: HashMap<OrgName, HashSet<u64>> = HashMap::new();

        for team in &self.teams {
            if let Some(gh) = &team.github {
                for toml_gh_team in &gh.teams {
                    org_team_members
                        .entry(toml_gh_team.org.clone())
                        .or_default()
                        .extend(toml_gh_team.members.iter().copied());
                }
            }
        }
        org_team_members
    }

    /// Diff organization memberships between TOML teams and GitHub
    async fn diff_org_memberships(&self) -> anyhow::Result<Vec<OrgMembershipDiff>> {
        let toml_org_team_members = self.get_org_members_from_teams();

        let mut org_diffs: BTreeMap<String, OrgMembershipDiff> = BTreeMap::new();

        for (org, toml_members) in toml_org_team_members {
            // Skip independent organizations - they manage their own members
            if self.config.independent_github_orgs.contains(&org) {
                debug!("Skipping member sync for independent organization: {}", org);
                continue;
            }

            let Some(gh_org_members) = self.org_members.get(&org) else {
                panic!("GitHub organization {org} not found");
            };

            let members_to_remove = self.members_to_remove(toml_members, gh_org_members);

            // The rest are members that should be removed
            if !members_to_remove.is_empty() {
                let mut members_to_remove: Vec<String> = members_to_remove.into_values().collect();
                members_to_remove.sort();

                org_diffs.insert(
                    org.clone(),
                    OrgMembershipDiff {
                        org,
                        members_to_remove,
                    },
                );
            }
        }

        Ok(org_diffs.into_values().collect())
    }

    /// Return GitHub members that should be removed from the organization.
    fn members_to_remove(
        &self,
        toml_members: HashSet<u64>,
        gh_org_members: &HashMap<u64, String>,
    ) -> HashMap<u64, String> {
        // Initialize `members_to_remove` to all GitHub members in the org.
        // Next, we'll delete members from `members_to_remove` that don't respect certain criteria.
        let mut members_to_remove = gh_org_members.clone();

        // People who belong to a team should stay in the org.
        for member in toml_members {
            members_to_remove.remove(&member);
        }

        // Members that are explicitly allowed in the `config.toml` file should stay in the org.
        for allowed_member in &self.config.special_org_members {
            if let Some(member_to_retain) = members_to_remove
                .iter()
                .find(|(_, username)| username == &allowed_member)
                .map(|(id, _)| *id)
            {
                members_to_remove.remove(&member_to_retain);
            }
        }
        members_to_remove
    }

    async fn diff_teams(&self) -> anyhow::Result<Vec<TeamDiff>> {
        let mut diffs = Vec::new();
        let mut unseen_github_teams = HashMap::new();
        let mut teams_to_diff = vec![];
        for team in &self.teams {
            if let Some(gh) = &team.github {
                for github_team in &gh.teams {
                    // Get existing teams we haven't seen yet
                    let unseen_github_teams = match unseen_github_teams.get_mut(&github_team.org) {
                        Some(ts) => ts,
                        None => {
                            let ts: HashMap<_, _> = self
                                .github
                                .org_teams(&github_team.org)
                                .await?
                                .into_iter()
                                .collect();
                            unseen_github_teams
                                .entry(github_team.org.clone())
                                .or_insert(ts)
                        }
                    };
                    // Remove the current team from the collection of unseen GitHub teams
                    unseen_github_teams.remove(&github_team.name);
                    teams_to_diff.push(github_team);
                }
            }
        }

        // Diff teams concurrently
        let mut stream = futures_util::stream::iter(teams_to_diff)
            .map(|team| self.diff_team(team))
            .buffer_unordered(10);
        while let Some(diff_team) = stream.next().await {
            let diff_team = diff_team?;
            if !diff_team.noop() {
                diffs.push(diff_team);
            }
        }

        let delete_diffs = unseen_github_teams
            .into_iter()
            .filter(|(org, _)| matches!(org.as_str(), "rust-lang" | "rust-lang-nursery")) // Only delete unmanaged teams in `rust-lang` and `rust-lang-nursery` for now
            .flat_map(|(org, remaining_github_teams)| {
                remaining_github_teams
                    .into_iter()
                    .map(move |t| (org.clone(), t))
            })
            // Don't delete the special bot teams
            .filter(|(_, (remaining_github_team, _))| {
                !BOTS_TEAMS.contains(&remaining_github_team.as_str())
            })
            .map(|(org, (name, slug))| TeamDiff::Delete(DeleteTeamDiff { org, name, slug }));

        diffs.extend(delete_diffs);

        Ok(diffs)
    }

    async fn diff_team(
        &self,
        github_team: &rust_team_data::v1::GitHubTeam,
    ) -> anyhow::Result<TeamDiff> {
        debug!("Diffing team `{}/{}`", github_team.org, github_team.name);

        // Ensure the team exists and is consistent
        let team = match self
            .github
            .team(&github_team.org, &github_team.name)
            .await?
        {
            Some(team) => team,
            None => {
                let members = github_team
                    .members
                    .iter()
                    .map(|member| {
                        let expected_role = self.expected_role(&github_team.org, *member);
                        (self.usernames_cache[member].clone(), expected_role)
                    })
                    .collect();
                return Ok(TeamDiff::Create(CreateTeamDiff {
                    org: github_team.org.clone(),
                    name: github_team.name.clone(),
                    description: DEFAULT_DESCRIPTION.to_owned(),
                    privacy: DEFAULT_PRIVACY,
                    members,
                }));
            }
        };
        let mut name_diff = None;
        if team.name != github_team.name {
            name_diff = Some(github_team.name.clone())
        }
        let mut description_diff = None;
        match &team.description {
            Some(description) => {
                if description != DEFAULT_DESCRIPTION {
                    description_diff = Some((description.clone(), DEFAULT_DESCRIPTION.to_owned()));
                }
            }
            None => {
                description_diff = Some((String::new(), DEFAULT_DESCRIPTION.to_owned()));
            }
        }
        let mut privacy_diff = None;
        if team.privacy != DEFAULT_PRIVACY {
            privacy_diff = Some((team.privacy, DEFAULT_PRIVACY))
        }

        let mut member_diffs = Vec::new();

        let mut current_members = self
            .github
            .team_memberships(&team, &github_team.org)
            .await?;
        let invites = self
            .github
            .team_membership_invitations(&github_team.org, &github_team.name)
            .await?;

        // Ensure all expected members are in the team
        for member in &github_team.members {
            let expected_role = self.expected_role(&github_team.org, *member);
            let username = &self.usernames_cache[member];
            if let Some(member) = current_members.remove(member) {
                if member.role != expected_role {
                    member_diffs.push((
                        username.clone(),
                        MemberDiff::ChangeRole((member.role, expected_role)),
                    ));
                } else {
                    member_diffs.push((username.clone(), MemberDiff::Noop));
                }
            } else {
                // Check if the user has been invited already
                if invites.contains(username) {
                    member_diffs.push((username.clone(), MemberDiff::Noop));
                } else {
                    member_diffs.push((username.clone(), MemberDiff::Create(expected_role)));
                }
            }
        }

        // The previous cycle removed expected members from current_members, so it only contains
        // members to delete now.
        for member in current_members.values() {
            member_diffs.push((member.username.clone(), MemberDiff::Delete));
        }

        Ok(TeamDiff::Edit(EditTeamDiff {
            org: github_team.org.clone(),
            name: team.name,
            name_diff,
            description_diff,
            privacy_diff,
            member_diffs,
        }))
    }

    async fn diff_repos(&self) -> anyhow::Result<Vec<RepoDiff>> {
        let mut diffs = Vec::new();

        let mut stream = futures_util::stream::iter(self.repos.iter())
            .map(|repo| self.diff_repo(repo))
            .buffer_unordered(10);
        while let Some(repo_diff) = stream.next().await {
            let repo_diff = repo_diff?;
            if !repo_diff.noop() {
                diffs.push(repo_diff);
            }
        }
        diffs.sort_by(|left, right| left.org_and_name().cmp(&right.org_and_name()));
        Ok(diffs)
    }

    /// Check if a repository should use rulesets instead of branch protections
    fn should_use_rulesets(&self, repo: &rust_team_data::v1::Repo) -> bool {
        let repo_full_name = format!("{}/{}", repo.org, repo.name);
        !self.config.disable_rulesets_repos.contains(&repo_full_name)
    }

    async fn construct_ruleset(
        &self,
        expected_repo: &rust_team_data::v1::Repo,
        branch_protection: &rust_team_data::v1::BranchProtection,
    ) -> anyhow::Result<api::Ruleset> {
        let bypass_actors = self.bypass_actors(expected_repo, branch_protection).await?;

        Ok(construct_ruleset(branch_protection, bypass_actors))
    }

    async fn bypass_actors(
        &self,
        expected_repo: &rust_team_data::v1::Repo,
        branch_protection: &rust_team_data::v1::BranchProtection,
    ) -> Result<Vec<api::RulesetBypassActor>, anyhow::Error> {
        use api::{RulesetActorType, RulesetBypassActor, RulesetBypassMode};

        let mut bypass_actors = Vec::new();
        let allowed_teams = self
            .allowed_merge_teams(expected_repo, branch_protection)
            .await?;
        bypass_actors.extend(allowed_teams);
        let allowed_apps = branch_protection
            .allowed_merge_apps
            .iter()
            .filter_map(|app| {
                app.app_id().map(|app_id| RulesetBypassActor {
                    actor_id: app_id,
                    actor_type: RulesetActorType::Integration,
                    bypass_mode: RulesetBypassMode::Always,
                })
            });
        bypass_actors.extend(allowed_apps);
        Ok(bypass_actors)
    }

    async fn allowed_merge_teams(
        &self,
        expected_repo: &rust_team_data::v1::Repo,
        branch_protection: &rust_team_data::v1::BranchProtection,
    ) -> Result<Vec<api::RulesetBypassActor>, anyhow::Error> {
        use api::{RulesetActorType, RulesetBypassActor, RulesetBypassMode};

        let mut allowed = vec![];

        for team_name in &branch_protection.allowed_merge_teams {
            let github_team = self
                .github
                .team(&expected_repo.org, team_name)
                .await?
                .with_context(|| {
                    format!(
                        "failed to find GitHub team '{team_name}' in org '{}' for repo '{}/{}'",
                        expected_repo.org, expected_repo.org, expected_repo.name
                    )
                })?;
            let team_id = github_team.id.with_context(|| {
                format!(
                    "GitHub team '{team_name}' in org '{}' is missing an ID",
                    expected_repo.org
                )
            })?;

            allowed.push(RulesetBypassActor {
                actor_id: team_id as i64,
                actor_type: RulesetActorType::Team,
                bypass_mode: RulesetBypassMode::Always,
            });
        }
        Ok(allowed)
    }

    async fn diff_repo(
        &self,
        expected_repo: &rust_team_data::v1::Repo,
    ) -> anyhow::Result<RepoDiff> {
        debug!(
            "Diffing repo `{}/{}`",
            expected_repo.org, expected_repo.name
        );

        let actual_repo = match self
            .github
            .repo(&expected_repo.org, &expected_repo.name)
            .await?
        {
            Some(r) => r,
            None => {
                let permissions = calculate_permission_diffs(
                    expected_repo,
                    Default::default(),
                    Default::default(),
                )?;

                let mut branch_protections = Vec::new();
                for branch_protection in &expected_repo.branch_protections {
                    branch_protections.push((
                        branch_protection.pattern.clone(),
                        construct_branch_protection(expected_repo, branch_protection),
                    ));
                }

                let mut rulesets = Vec::new();
                let use_rulesets = self.should_use_rulesets(expected_repo);
                if use_rulesets {
                    for branch_protection in &expected_repo.branch_protections {
                        let ruleset = self
                            .construct_ruleset(expected_repo, branch_protection)
                            .await?;
                        rulesets.push(ruleset);
                    }
                }

                return Ok(RepoDiff::Create(CreateRepoDiff {
                    org: expected_repo.org.clone(),
                    name: expected_repo.name.clone(),
                    settings: RepoSettings {
                        description: expected_repo.description.clone(),
                        homepage: expected_repo.homepage.clone(),
                        archived: false,
                        auto_merge_enabled: expected_repo.auto_merge_enabled,
                    },
                    permissions,
                    // Don't create branch protections if using rulesets
                    branch_protections: if use_rulesets {
                        vec![]
                    } else {
                        branch_protections
                    },
                    rulesets,
                    environments: expected_repo
                        .environments
                        .iter()
                        .map(|(name, env)| (name.clone(), env.clone()))
                        .collect(),
                    app_installations: self.diff_app_installations(expected_repo, &[])?,
                }));
            }
        };

        if !expected_repo.private && actual_repo.private {
            return Err(anyhow::anyhow!(
                "Repository `{}/{}` is private on GitHub, but not marked as private in team. This can be a security concern!",
                actual_repo.org,
                actual_repo.name
            ));
        }

        let permission_diffs = self.diff_permissions(expected_repo).await?;

        let branch_protection_diffs = self
            .diff_branch_protections(&actual_repo, expected_repo)
            .await?;

        let ruleset_diffs = if self.should_use_rulesets(expected_repo) {
            self.diff_rulesets(expected_repo).await?
        } else {
            Vec::new()
        };

        let environment_diffs = self.diff_environments(expected_repo).await?;
        let old_settings = RepoSettings {
            description: actual_repo.description.clone(),
            homepage: actual_repo.homepage.clone(),
            archived: actual_repo.archived,
            auto_merge_enabled: actual_repo.allow_auto_merge.unwrap_or(false),
        };
        let new_settings = RepoSettings {
            description: expected_repo.description.clone(),
            homepage: expected_repo.homepage.clone(),
            archived: expected_repo.archived,
            auto_merge_enabled: expected_repo.auto_merge_enabled,
        };

        let existing_installations = self
            .org_apps
            .get(&expected_repo.org)
            .map(|installations| {
                installations
                    .iter()
                    .filter_map(|installation| {
                        if installation.repositories.contains(&actual_repo.name) {
                            Some(AppInstallation {
                                app: installation.app,
                                installation_id: installation.installation_id,
                            })
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let app_installation_diffs =
            self.diff_app_installations(expected_repo, &existing_installations)?;

        Ok(RepoDiff::Update(UpdateRepoDiff {
            org: expected_repo.org.clone(),
            name: actual_repo.name,
            repo_node_id: actual_repo.node_id,
            repo_id: actual_repo.repo_id,
            settings_diff: (old_settings, new_settings),
            permission_diffs,
            branch_protection_diffs,
            ruleset_diffs,
            environment_diffs,
            app_installation_diffs,
        }))
    }

    async fn diff_permissions(
        &self,
        expected_repo: &rust_team_data::v1::Repo,
    ) -> anyhow::Result<Vec<RepoPermissionAssignmentDiff>> {
        let actual_teams: HashMap<_, _> = self
            .github
            .repo_teams(&expected_repo.org, &expected_repo.name)
            .await?
            .into_iter()
            .map(|t| (t.name.clone(), t))
            .collect();
        let actual_collaborators: HashMap<_, _> = self
            .github
            .repo_collaborators(&expected_repo.org, &expected_repo.name)
            .await?
            .into_iter()
            .map(|u| (u.name.clone(), u))
            .collect();

        calculate_permission_diffs(expected_repo, actual_teams, actual_collaborators)
    }

    async fn diff_branch_protections(
        &self,
        actual_repo: &api::Repo,
        expected_repo: &rust_team_data::v1::Repo,
    ) -> anyhow::Result<Vec<BranchProtectionDiff>> {
        // The rust-lang/rust repository uses GitHub apps push allowance actors for its branch
        // protections, which cannot be read without a PAT.
        // To avoid errors, we simply return an empty diff here.
        if !self.github.uses_pat() && actual_repo.org == "rust-lang" && actual_repo.name == "rust" {
            return Ok(vec![]);
        }

        let mut branch_protection_diffs = Vec::new();
        let mut actual_protections = self
            .github
            .branch_protections(&actual_repo.org, &actual_repo.name)
            .await?;

        // If rulesets are enabled, delete all existing branch protections
        // to avoid conflicts between branch protections and rulesets
        if self.should_use_rulesets(expected_repo) {
            return Ok(actual_protections
                .into_iter()
                .map(|(name, (id, _))| BranchProtectionDiff {
                    pattern: name,
                    operation: BranchProtectionDiffOperation::Delete(id),
                })
                .collect());
        }
        for branch_protection in &expected_repo.branch_protections {
            let actual_branch_protection = actual_protections.remove(&branch_protection.pattern);
            let mut expected_branch_protection =
                construct_branch_protection(expected_repo, branch_protection);

            // We don't model GitHub App push allowance actors in team.
            // However, we don't want to remove existing accesses of GH apps to
            // branches.
            // So if there is an existing branch protection, we copy its GitHub app
            // push allowances into the expected branch protection, to roundtrip the app access.
            if let Some((_, actual_branch_protection)) = &actual_branch_protection {
                expected_branch_protection.push_allowances.extend(
                    actual_branch_protection
                        .push_allowances
                        .iter()
                        .filter(|allowance| matches!(allowance, PushAllowanceActor::App(_)))
                        .cloned(),
                );
            }

            let operation = {
                match actual_branch_protection {
                    Some((database_id, bp)) if bp != expected_branch_protection => {
                        BranchProtectionDiffOperation::Update(
                            database_id,
                            bp,
                            expected_branch_protection,
                        )
                    }
                    None => BranchProtectionDiffOperation::Create(expected_branch_protection),
                    // The branch protection doesn't need to change
                    Some(_) => continue,
                }
            };
            branch_protection_diffs.push(BranchProtectionDiff {
                pattern: branch_protection.pattern.clone(),
                operation,
            });
        }

        // `actual_branch_protections` now contains the branch protections that were not expected
        // but are still on GitHub. We want to delete them.
        branch_protection_diffs.extend(actual_protections.into_iter().map(|(name, (id, _))| {
            BranchProtectionDiff {
                pattern: name,
                operation: BranchProtectionDiffOperation::Delete(id),
            }
        }));

        Ok(branch_protection_diffs)
    }

    async fn diff_environments(
        &self,
        expected_repo: &rust_team_data::v1::Repo,
    ) -> anyhow::Result<Vec<EnvironmentDiff>> {
        let mut environment_diffs = Vec::new();

        let actual_environments_map = self
            .github
            .repo_environments(&expected_repo.org, &expected_repo.name)
            .await?;

        let actual_environments: BTreeSet<String> =
            actual_environments_map.keys().cloned().collect();
        let expected_environments: BTreeSet<String> =
            expected_repo.environments.keys().cloned().collect();

        // Environments to create (already sorted via BTreeSet)
        for env_name in expected_environments.difference(&actual_environments) {
            let env = expected_repo.environments.get(env_name).unwrap();
            environment_diffs.push(EnvironmentDiff::Create(env_name.clone(), env.clone()));
        }

        // Environments to update (already sorted via BTreeSet)
        for env_name in expected_environments.intersection(&actual_environments) {
            let expected_env = expected_repo.environments.get(env_name).unwrap();
            let actual_env = actual_environments_map.get(env_name).unwrap();

            let expected_branches: BTreeSet<_> = expected_env.branches.iter().collect();
            let actual_branches: BTreeSet<_> = actual_env.branches.iter().collect();

            let expected_tags: BTreeSet<_> = expected_env.tags.iter().collect();
            let actual_tags: BTreeSet<_> = actual_env.tags.iter().collect();

            let add_branches: Vec<_> = expected_branches
                .difference(&actual_branches)
                .map(|s| s.to_string())
                .collect();

            let remove_branches: Vec<_> = actual_branches
                .difference(&expected_branches)
                .map(|s| s.to_string())
                .collect();

            let add_tags: Vec<_> = expected_tags
                .difference(&actual_tags)
                .map(|s| s.to_string())
                .collect();

            let remove_tags: Vec<_> = actual_tags
                .difference(&expected_tags)
                .map(|s| s.to_string())
                .collect();

            if !add_branches.is_empty()
                || !remove_branches.is_empty()
                || !add_tags.is_empty()
                || !remove_tags.is_empty()
            {
                let mut new_branches = expected_env.branches.clone();
                new_branches.sort();
                let mut new_tags = expected_env.tags.clone();
                new_tags.sort();

                environment_diffs.push(EnvironmentDiff::Update {
                    name: env_name.clone(),
                    add_branches,
                    remove_branches,
                    add_tags,
                    remove_tags,
                    new_branches,
                    new_tags,
                });
            }
        }

        // Environments to delete (already sorted via BTreeSet)
        for env_name in actual_environments.difference(&expected_environments) {
            environment_diffs.push(EnvironmentDiff::Delete(env_name.clone()));
        }

        Ok(environment_diffs)
    }

    async fn diff_rulesets(
        &self,
        expected_repo: &rust_team_data::v1::Repo,
    ) -> anyhow::Result<Vec<RulesetDiff>> {
        let mut ruleset_diffs = Vec::new();

        // Fetch existing rulesets from GitHub
        let actual_rulesets = self
            .github
            .repo_rulesets(&expected_repo.org, &expected_repo.name)
            .await?;

        // Build a map of actual rulesets by branch name (the logical identity)
        let mut rulesets_by_name: HashMap<String, api::Ruleset> = HashMap::new();

        for ruleset in actual_rulesets {
            // If multiple rulesets have the same name, keep the last one
            // and mark others for deletion (they shouldn't exist)
            if let Some(existing) = rulesets_by_name.insert(ruleset.name.clone(), ruleset)
                && let Some(id) = existing.id
            {
                ruleset_diffs.push(RulesetDiff {
                    name: existing.name.clone(),
                    operation: RulesetDiffOperation::Delete(id),
                });
            }
        }

        // Process each branch protection as a potential ruleset
        for branch_protection in &expected_repo.branch_protections {
            let expected_ruleset = self
                .construct_ruleset(expected_repo, branch_protection)
                .await?;

            if let Some(actual_ruleset) = rulesets_by_name.remove(&expected_ruleset.name) {
                let Ruleset {
                    id: _,
                    name,
                    target,
                    source_type,
                    enforcement,
                    bypass_actors,
                    conditions,
                    rules,
                } = &actual_ruleset;
                let Ruleset {
                    id: _,
                    name: _,
                    target: expected_target,
                    source_type: expected_source_type,
                    enforcement: expected_enforcement,
                    bypass_actors: expected_bypass_actors,
                    conditions: expected_conditions,
                    rules: expected_rules,
                } = &expected_ruleset;

                // With a read-only GitHub App token, GitHub does not actually return bypass actors
                // from the API. So we should not check it, otherwise the diff will not be clean.
                let bypass_actors_differ = if !self.github.uses_pat() {
                    false
                } else {
                    bypass_actors != expected_bypass_actors
                };

                // Ruleset exists for this branch name, check if it needs updating
                if target != expected_target
                    || source_type != expected_source_type
                    || enforcement != expected_enforcement
                    || bypass_actors_differ
                    || conditions != expected_conditions
                    || rules != expected_rules
                {
                    let Some(id) = actual_ruleset.id else {
                        return Err(anyhow::anyhow!(
                            "Encountered ruleset without ID: {actual_ruleset:?}"
                        ));
                    };
                    ruleset_diffs.push(RulesetDiff {
                        name: name.clone(),
                        operation: RulesetDiffOperation::Update(
                            id,
                            actual_ruleset,
                            expected_ruleset,
                        ),
                    });
                }
            } else {
                ruleset_diffs.push(RulesetDiff {
                    name: expected_ruleset.name.clone(),
                    operation: RulesetDiffOperation::Create(expected_ruleset),
                });
            }
        }

        // Delete rulesets that have names not matching any expected branch protection
        for (_, ruleset) in rulesets_by_name {
            if let Some(id) = ruleset.id {
                ruleset_diffs.push(RulesetDiff {
                    name: ruleset.name.clone(),
                    operation: RulesetDiffOperation::Delete(id),
                });
            }
        }

        Ok(ruleset_diffs)
    }

    fn diff_app_installations(
        &self,
        expected_repo: &rust_team_data::v1::Repo,
        existing_installations: &[AppInstallation],
    ) -> anyhow::Result<Vec<AppInstallationDiff>> {
        let mut diff = vec![];
        let mut found_apps = Vec::new();

        // Find apps that should be enabled on the repository
        for app in expected_repo.bots.iter().filter_map(|bot| match bot {
            Bot::Renovate => Some(GithubApp::RenovateBot),
            Bot::ForkingRenovate => Some(GithubApp::ForkingRenovateBot),
            Bot::Bors => Some(GithubApp::Bors),
            Bot::Highfive
            | Bot::Rfcbot
            | Bot::RustTimer
            | Bot::Rustbot
            | Bot::Craterbot
            | Bot::Glacierbot
            | Bot::LogAnalyzer
            | Bot::HerokuDeployAccess => None,
        }) {
            // Find installation ID of this app on GitHub
            let gh_installation = self
                .org_apps
                .get(&expected_repo.org)
                .and_then(|installations| {
                    installations
                        .iter()
                        .find(|installation| installation.app == app)
                        .map(|i| i.installation_id)
                });
            let Some(gh_installation) = gh_installation else {
                log::warn!(
                    "Application {app} should be enabled for repository {}/{}, but it is not installed on GitHub",
                    expected_repo.org,
                    expected_repo.name
                );
                continue;
            };
            let installation = AppInstallation {
                app,
                installation_id: gh_installation,
            };
            found_apps.push(installation.clone());

            if !existing_installations.contains(&installation) {
                diff.push(AppInstallationDiff::Add(installation));
            }
        }
        for existing in existing_installations {
            if !found_apps.contains(existing) {
                diff.push(AppInstallationDiff::Remove(existing.clone()));
            }
        }

        Ok(diff)
    }

    fn expected_role(&self, org: &str, user: u64) -> TeamRole {
        if let Some(true) = self
            .org_owners
            .get(org)
            .map(|owners| owners.contains(&user))
        {
            TeamRole::Maintainer
        } else {
            TeamRole::Member
        }
    }
}

fn calculate_permission_diffs(
    expected_repo: &rust_team_data::v1::Repo,
    mut actual_teams: HashMap<String, api::RepoTeam>,
    mut actual_collaborators: HashMap<String, api::RepoUser>,
) -> anyhow::Result<Vec<RepoPermissionAssignmentDiff>> {
    let mut permissions = Vec::new();
    // Team permissions
    for expected_team in &expected_repo.teams {
        let permission = convert_permission(&expected_team.permission);
        let actual_team = actual_teams.remove(&expected_team.name);
        let collaborator = RepoCollaborator::Team(expected_team.name.clone());

        let diff = match actual_team {
            Some(t) if t.permission != permission => RepoPermissionAssignmentDiff {
                collaborator,
                diff: RepoPermissionDiff::Update(t.permission, permission),
            },
            // Team permission does not need to change
            Some(_) => continue,
            None => RepoPermissionAssignmentDiff {
                collaborator,
                diff: RepoPermissionDiff::Create(permission),
            },
        };
        permissions.push(diff);
    }
    // Bot permissions
    let bots = expected_repo
        .bots
        .iter()
        .filter_map(|b| match BotDetails::from(b) {
            BotDetails::User { name, permission } => {
                actual_teams.remove(name);
                Some((name, permission))
            }
            BotDetails::GitHubApp => None,
        });
    // Member permissions
    let members = expected_repo
        .members
        .iter()
        .map(|m| (m.name.as_str(), convert_permission(&m.permission)));
    for (name, permission) in bots.chain(members) {
        let actual_collaborator = actual_collaborators.remove(name);
        let collaborator = RepoCollaborator::User(name.to_owned());
        let diff = match actual_collaborator {
            Some(t) if t.permission != permission => RepoPermissionAssignmentDiff {
                collaborator,
                diff: RepoPermissionDiff::Update(t.permission, permission),
            },
            // Collaborator permission does not need to change
            Some(_) => continue,
            None => RepoPermissionAssignmentDiff {
                collaborator,
                diff: RepoPermissionDiff::Create(permission),
            },
        };
        permissions.push(diff);
    }
    // `actual_teams` now contains the teams that were not expected
    // but are still on GitHub. We now remove them.
    for (team, t) in actual_teams {
        if t.name == "security" && expected_repo.org == "rust-lang" {
            // Skip removing access permissions from security.
            // If we're in this branch we know that the team repo doesn't mention this team at all,
            // so this shouldn't remove intentionally granted non-read access.  Security is granted
            // read access to all repositories in the org by GitHub (via a "security manager"
            // role), and we can't remove that access.
            //
            // (FIXME: If we find security with non-read access, *that* probably should get dropped
            // to read access. But not worth doing in this commit, want to get us unblocked first).
            continue;
        }
        permissions.push(RepoPermissionAssignmentDiff {
            collaborator: RepoCollaborator::Team(team),
            diff: RepoPermissionDiff::Delete(t.permission),
        });
    }
    // `actual_collaborators` now contains the collaborators that were not expected
    // but are still on GitHub. We now remove them.
    for (collaborator, u) in actual_collaborators {
        permissions.push(RepoPermissionAssignmentDiff {
            collaborator: RepoCollaborator::User(collaborator),
            diff: RepoPermissionDiff::Delete(u.permission),
        });
    }
    Ok(permissions)
}

enum BotDetails {
    User {
        name: &'static str,
        permission: RepoPermission,
    },
    GitHubApp,
}

impl From<&Bot> for BotDetails {
    fn from(bot: &Bot) -> Self {
        let user = |name, permission| BotDetails::User { name, permission };
        let write_access = |name| user(name, RepoPermission::Write);
        let admin_access = |name| user(name, RepoPermission::Admin);

        match bot {
            Bot::Bors => write_access("bors"),
            Bot::Highfive => write_access("rust-highfive"),
            Bot::Rustbot => write_access("rustbot"),
            Bot::RustTimer => write_access("rust-timer"),
            Bot::Rfcbot => write_access("rust-rfcbot"),
            Bot::Craterbot => write_access("craterbot"),
            Bot::Glacierbot => write_access("rust-lang-glacier-bot"),
            Bot::LogAnalyzer => write_access("rust-log-analyzer"),
            Bot::Renovate | Bot::ForkingRenovate => BotDetails::GitHubApp,
            // Unfortunately linking to Heroku requires admin access, since the integration creates
            // GitHub webhooks, which require admin access.
            Bot::HerokuDeployAccess => admin_access("rust-heroku-deploy-access"),
        }
    }
}

pub fn convert_permission(p: &rust_team_data::v1::RepoPermission) -> RepoPermission {
    use rust_team_data::v1;
    match *p {
        v1::RepoPermission::Write => RepoPermission::Write,
        v1::RepoPermission::Admin => RepoPermission::Admin,
        v1::RepoPermission::Maintain => RepoPermission::Maintain,
        v1::RepoPermission::Triage => RepoPermission::Triage,
    }
}

fn get_branch_protection_mode(
    branch_protection: &rust_team_data::v1::BranchProtection,
) -> BranchProtectionMode {
    let is_managed_by_bors = branch_protection
        .allowed_merge_apps
        .contains(&MergeBot::Bors);
    // When bors manages a branch, we should not require a PR nor approvals
    // for that branch, because it will (force) push to these branches directly.
    if is_managed_by_bors {
        BranchProtectionMode::PrNotRequired
    } else {
        branch_protection.mode.clone()
    }
}

pub fn construct_branch_protection(
    expected_repo: &rust_team_data::v1::Repo,
    branch_protection: &rust_team_data::v1::BranchProtection,
) -> api::BranchProtection {
    let branch_protection_mode = get_branch_protection_mode(branch_protection);

    let required_approving_review_count: u8 = match branch_protection_mode {
        BranchProtectionMode::PrRequired {
            required_approvals, ..
        } => required_approvals
            .try_into()
            .expect("Too large required approval count"),
        BranchProtectionMode::PrNotRequired => 0,
    };
    let mut push_allowances: Vec<PushAllowanceActor> = branch_protection
        .allowed_merge_teams
        .iter()
        .map(|team| {
            api::PushAllowanceActor::Team(api::TeamPushAllowanceActor {
                organization: Login {
                    login: expected_repo.org.clone(),
                },
                name: team.to_string(),
            })
        })
        .collect();

    for merge_bot in &branch_protection.allowed_merge_apps {
        let allowance = match merge_bot {
            MergeBot::Homu => PushAllowanceActor::User(api::UserPushAllowanceActor {
                login: "bors".to_owned(),
            }),
            MergeBot::RustTimer => PushAllowanceActor::User(api::UserPushAllowanceActor {
                login: "rust-timer".to_owned(),
            }),
            MergeBot::Bors | MergeBot::WorkflowsCratesIo | MergeBot::PromoteRelease => {
                // These use GitHub apps, which are not configured through team (set manually).
                // Their push allowance will be roundtripped by sync-team.
                continue;
            }
        };
        push_allowances.push(allowance);
    }

    let mut checks = match &branch_protection_mode {
        BranchProtectionMode::PrRequired { ci_checks, .. } => ci_checks.clone(),
        BranchProtectionMode::PrNotRequired => {
            vec![]
        }
    };
    // Normalize check order to avoid diffs based only on the ordering difference
    checks.sort();

    api::BranchProtection {
        pattern: branch_protection.pattern.clone(),
        is_admin_enforced: true,
        allows_force_pushes: !branch_protection.prevent_force_push,
        dismisses_stale_reviews: branch_protection.dismiss_stale_review,
        requires_conversation_resolution: branch_protection.require_conversation_resolution,
        requires_linear_history: branch_protection.require_linear_history,
        requires_strict_status_checks: branch_protection.require_up_to_date_branches,
        required_approving_review_count,
        required_status_check_contexts: checks,
        push_allowances,
        requires_approving_reviews: matches!(
            branch_protection_mode,
            BranchProtectionMode::PrRequired { .. }
        ),
    }
}

/// Convert a branch or tag pattern to a full ref pattern for use in rulesets.
/// GitHub rulesets require full ref paths like "refs/heads/main" and "refs/tags/0.*".
pub(crate) fn convert_pattern_to_ref_pattern(target: ProtectionTarget, pattern: &str) -> String {
    if pattern.starts_with("refs/") {
        return pattern.to_string();
    }

    match target {
        ProtectionTarget::Branch => format!("refs/heads/{pattern}"),
        ProtectionTarget::Tag => format!("refs/tags/{pattern}"),
    }
}

fn github_int(value: u32) -> i32 {
    i32::try_from(value).unwrap_or_else(|_| panic!("Value {value} exceeds GitHub's Int range"))
}

pub fn construct_ruleset(
    branch_protection: &rust_team_data::v1::BranchProtection,
    bypass_actors: Vec<api::RulesetBypassActor>,
) -> api::Ruleset {
    use api::*;

    // Use a BTreeSet to ensure a consistent order. This avoids unnecessary diffs when the order of rules changes,
    // since GitHub does not guarantee any specific order for rules.
    let mut rules: BTreeSet<RulesetRule> = BTreeSet::new();

    // Add creation protection if requested
    if branch_protection.prevent_creation {
        rules.insert(RulesetRule::Creation);
    }

    // Add deletion protection if requested
    if branch_protection.prevent_deletion {
        rules.insert(RulesetRule::Deletion);
    }

    if branch_protection.prevent_update {
        rules.insert(RulesetRule::Update);
    }

    if branch_protection.require_linear_history {
        rules.insert(RulesetRule::RequiredLinearHistory);
    }

    // Add non-fast-forward protection if requested
    if branch_protection.prevent_force_push {
        rules.insert(RulesetRule::NonFastForward);
    }

    // Add pull request rule if PRs are required
    if let BranchProtectionMode::PrRequired {
        required_approvals, ..
    } = branch_protection.mode
    {
        rules.insert(RulesetRule::PullRequest {
            parameters: PullRequestParameters {
                dismiss_stale_reviews_on_push: branch_protection.dismiss_stale_review,
                require_code_owner_review: REQUIRE_CODE_OWNER_REVIEW_DEFAULT,
                require_last_push_approval: REQUIRE_LAST_PUSH_APPROVAL_DEFAULT,
                required_approving_review_count: github_int(required_approvals),
                required_review_thread_resolution: branch_protection
                    .require_conversation_resolution,
            },
        });
    }

    // Add required status checks if any
    if let BranchProtectionMode::PrRequired { ci_checks, .. } = &branch_protection.mode
        && !ci_checks.is_empty()
    {
        let mut checks = ci_checks.clone();
        checks.sort();
        rules.insert(RulesetRule::RequiredStatusChecks {
            parameters: RequiredStatusChecksParameters {
                do_not_enforce_on_create: Some(false),
                required_status_checks: checks
                    .iter()
                    .map(|context| RequiredStatusCheck {
                        context: context.clone(),
                        integration_id: Some(GITHUB_ACTIONS_INTEGRATION_ID),
                    })
                    .collect(),
                strict_required_status_checks_policy: branch_protection.require_up_to_date_branches,
            },
        });
    }

    if branch_protection.merge_queue {
        let merge_method = match branch_protection.merge_queue_method {
            rust_team_data::v1::MergeQueueMethod::Merge => MergeQueueMergeMethod::Merge,
            rust_team_data::v1::MergeQueueMethod::Squash => MergeQueueMergeMethod::Squash,
            rust_team_data::v1::MergeQueueMethod::Rebase => MergeQueueMergeMethod::Rebase,
        };
        let parameters = MergeQueueParameters {
            check_response_timeout_minutes: github_int(
                branch_protection.merge_queue_check_response_timeout_minutes,
            ),
            max_entries_to_build: github_int(branch_protection.merge_queue_max_entries_to_build),
            max_entries_to_merge: github_int(branch_protection.merge_queue_max_entries_to_merge),
            merge_method,
            min_entries_to_merge_wait_minutes: github_int(
                branch_protection.merge_queue_min_entries_to_merge_wait_minutes,
            ),
            ..MergeQueueParameters::default()
        };

        rules.insert(RulesetRule::MergeQueue { parameters });
    }

    api::Ruleset {
        id: None,
        name: branch_protection
            .name
            .clone()
            .unwrap_or_else(|| branch_protection.pattern.to_string()),
        target: match branch_protection.target {
            ProtectionTarget::Branch => RulesetTarget::Branch,
            ProtectionTarget::Tag => RulesetTarget::Tag,
        },
        source_type: RulesetSourceType::Repository,
        enforcement: RulesetEnforcement::Active,
        bypass_actors,
        conditions: RulesetConditions {
            ref_name: RulesetRefNameCondition {
                include: vec![convert_pattern_to_ref_pattern(
                    branch_protection.target,
                    &branch_protection.pattern,
                )],
                exclude: vec![],
            },
        },
        rules,
    }
}

/// The special bot teams
const BOTS_TEAMS: &[&str] = &["bors", "highfive", "rfcbot", "bots"];

/// A diff between the team repo and the state on GitHub
pub(crate) struct Diff {
    team_diffs: Vec<TeamDiff>,
    repo_diffs: Vec<RepoDiff>,
    org_membership_diffs: Vec<OrgMembershipDiff>,
}

impl Diff {
    /// Apply the diff to GitHub
    pub(crate) async fn apply(self, sync: &GitHubWrite) -> anyhow::Result<()> {
        for team_diff in self.team_diffs {
            team_diff.apply(sync).await?;
        }
        for repo_diff in self.repo_diffs {
            repo_diff.apply(sync).await?;
        }
        for org_diff in self.org_membership_diffs {
            org_diff.apply(sync).await?;
        }

        Ok(())
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.team_diffs.is_empty()
            && self.repo_diffs.is_empty()
            && self.org_membership_diffs.is_empty()
    }
}

impl std::fmt::Display for Diff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if !self.team_diffs.is_empty() {
            writeln!(f, "💻 Team Diffs:")?;
            for team_diff in &self.team_diffs {
                write!(f, "{team_diff}")?;
            }
        }

        if !&self.repo_diffs.is_empty() {
            writeln!(f, "💻 Repo Diffs:")?;
            for repo_diff in &self.repo_diffs {
                write!(f, "{repo_diff}")?;
            }
        }

        if !&self.org_membership_diffs.is_empty() {
            writeln!(f, "💻 Org membership Diffs:")?;
            for org_diff in &self.org_membership_diffs {
                write!(f, "{org_diff}")?;
            }
        }

        Ok(())
    }
}

#[derive(Debug)]
enum RepoDiff {
    Create(CreateRepoDiff),
    Update(UpdateRepoDiff),
}

impl RepoDiff {
    async fn apply(&self, sync: &GitHubWrite) -> anyhow::Result<()> {
        match self {
            RepoDiff::Create(c) => c.apply(sync).await,
            RepoDiff::Update(u) => u.apply(sync).await,
        }
    }

    fn noop(&self) -> bool {
        match self {
            RepoDiff::Create(_c) => false,
            RepoDiff::Update(u) => u.noop(),
        }
    }

    fn org_and_name(&self) -> (&str, &str) {
        match self {
            RepoDiff::Create(c) => (&c.org, &c.name),
            RepoDiff::Update(u) => (&u.org, &u.name),
        }
    }
}

impl std::fmt::Display for RepoDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Create(c) => write!(f, "{c}"),
            Self::Update(u) => write!(f, "{u}"),
        }
    }
}

#[derive(Debug)]
struct OrgMembershipDiff {
    org: OrgName,
    members_to_remove: Vec<String>,
}

impl OrgMembershipDiff {
    async fn apply(self, sync: &GitHubWrite) -> anyhow::Result<()> {
        for member in &self.members_to_remove {
            sync.remove_gh_member_from_org(&self.org, member).await?;
        }

        Ok(())
    }
}

impl std::fmt::Display for OrgMembershipDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if !self.members_to_remove.is_empty() {
            writeln!(f, "❌ Removing the following members from `{}`:", self.org)?;
            for member in &self.members_to_remove {
                writeln!(f, "  - {member}",)?;
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
struct CreateRepoDiff {
    org: String,
    name: String,
    settings: RepoSettings,
    permissions: Vec<RepoPermissionAssignmentDiff>,
    branch_protections: Vec<(String, api::BranchProtection)>,
    rulesets: Vec<api::Ruleset>,
    environments: Vec<(String, rust_team_data::v1::Environment)>,
    app_installations: Vec<AppInstallationDiff>,
}

impl CreateRepoDiff {
    async fn apply(&self, sync: &GitHubWrite) -> anyhow::Result<()> {
        let repo = sync
            .create_repo(&self.org, &self.name, &self.settings)
            .await?;

        for permission in &self.permissions {
            permission.apply(sync, &self.org, &self.name).await?;
        }

        // Apply branch protections
        for (branch, protection) in &self.branch_protections {
            BranchProtectionDiff {
                pattern: branch.clone(),
                operation: BranchProtectionDiffOperation::Create(protection.clone()),
            }
            .apply(sync, &self.org, &self.name, &repo.node_id)
            .await?;
        }

        // Apply rulesets (in addition to branch protections if configured)
        for ruleset in &self.rulesets {
            RulesetDiff {
                name: ruleset.name.clone(),
                operation: RulesetDiffOperation::Create(ruleset.clone()),
            }
            .apply(sync, &self.org, &self.name)
            .await?;
        }

        for (env_name, env) in &self.environments {
            sync.create_environment(&self.org, &self.name, env_name, &env.branches, &env.tags)
                .await?;
        }

        for installation in &self.app_installations {
            installation.apply(sync, repo.repo_id, &self.org).await?;
        }

        Ok(())
    }
}

impl std::fmt::Display for CreateRepoDiff {
    fn fmt(&self, mut f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let CreateRepoDiff {
            org,
            name,
            settings,
            permissions,
            branch_protections,
            rulesets,
            environments,
            app_installations,
        } = self;

        let RepoSettings {
            description,
            homepage,
            archived: _,
            auto_merge_enabled,
        } = &settings;

        writeln!(f, "➕ Creating repo:")?;
        writeln!(f, "  Org: {org}")?;
        writeln!(f, "  Name: {name}")?;
        writeln!(f, "  Description: {description}")?;
        writeln!(f, "  Homepage: {homepage:?}")?;
        writeln!(f, "  Auto-merge: {auto_merge_enabled}")?;
        writeln!(f, "  Permissions:")?;
        for diff in permissions {
            write!(f, "{diff}")?;
        }

        if !branch_protections.is_empty() {
            writeln!(f, "  Branch Protections:")?;
            for (branch_name, branch_protection) in branch_protections {
                writeln!(&mut f, "    {branch_name}")?;
                log_branch_protection(branch_protection, None, &mut f)?;
            }
        }

        if !rulesets.is_empty() {
            writeln!(f, "  Rulesets:")?;
            for ruleset in rulesets {
                writeln!(f, "    {}", ruleset.name)?;
                log_ruleset(ruleset, None, &mut f)?;
            }
        }

        if !environments.is_empty() {
            writeln!(f, "  Environments:")?;
            for (env_name, env) in environments {
                writeln!(f, "    - {env_name}")?;
                if !env.branches.is_empty() {
                    writeln!(f, "        Branches: {}", env.branches.join(", "))?;
                }
                if !env.tags.is_empty() {
                    writeln!(f, "        Tags: {}", env.tags.join(", "))?;
                }
            }
        }

        writeln!(f, "  App Installations:")?;
        for diff in app_installations {
            write!(f, "{diff}")?;
        }

        Ok(())
    }
}

#[derive(Debug)]
struct UpdateRepoDiff {
    org: String,
    name: String,
    repo_node_id: String,
    repo_id: u64,
    // old, new
    settings_diff: (RepoSettings, RepoSettings),
    permission_diffs: Vec<RepoPermissionAssignmentDiff>,
    branch_protection_diffs: Vec<BranchProtectionDiff>,
    ruleset_diffs: Vec<RulesetDiff>,
    environment_diffs: Vec<EnvironmentDiff>,
    app_installation_diffs: Vec<AppInstallationDiff>,
}

#[derive(Debug)]
enum EnvironmentDiff {
    Create(String, rust_team_data::v1::Environment),
    Update {
        name: String,
        add_branches: Vec<String>,
        remove_branches: Vec<String>,
        add_tags: Vec<String>,
        remove_tags: Vec<String>,
        new_branches: Vec<String>,
        new_tags: Vec<String>,
    },
    Delete(String),
}

impl UpdateRepoDiff {
    pub(crate) fn noop(&self) -> bool {
        if !self.can_be_modified() {
            return true;
        }

        let UpdateRepoDiff {
            org: _,
            name: _,
            repo_node_id: _,
            repo_id: _,
            settings_diff,
            permission_diffs,
            branch_protection_diffs,
            ruleset_diffs,
            environment_diffs,
            app_installation_diffs,
        } = self;

        settings_diff.0 == settings_diff.1
            && permission_diffs.is_empty()
            && branch_protection_diffs.is_empty()
            && ruleset_diffs.is_empty()
            && environment_diffs.is_empty()
            && app_installation_diffs.is_empty()
    }

    fn can_be_modified(&self) -> bool {
        // Archived repositories cannot be modified
        // If the repository should be archived, and we do not change its archival status,
        // we should not change any other properties of the repo.
        if self.settings_diff.1.archived && self.settings_diff.0.archived {
            return false;
        }
        true
    }

    async fn apply(&self, sync: &GitHubWrite) -> anyhow::Result<()> {
        if !self.can_be_modified() {
            return Ok(());
        }

        // If we're unarchiving, we have to unarchive first and *then* modify other properties
        // of the repository. On the other hand, if we're achiving, we need to perform
        // the archiving *last* (otherwise permissions and branch protections cannot be modified)
        // anymore. If we're not changing the archival status, the order doesn't really matter.
        let is_unarchive = self.settings_diff.0.archived && !self.settings_diff.1.archived;

        if is_unarchive {
            sync.edit_repo(&self.org, &self.name, &self.settings_diff.1)
                .await?;
        }

        for permission in &self.permission_diffs {
            permission.apply(sync, &self.org, &self.name).await?;
        }

        for branch_protection in &self.branch_protection_diffs {
            branch_protection
                .apply(sync, &self.org, &self.name, &self.repo_node_id)
                .await?;
        }

        for ruleset in &self.ruleset_diffs {
            ruleset.apply(sync, &self.org, &self.name).await?;
        }

        for env_diff in &self.environment_diffs {
            match env_diff {
                EnvironmentDiff::Create(name, env) => {
                    sync.create_environment(&self.org, &self.name, name, &env.branches, &env.tags)
                        .await?;
                }
                EnvironmentDiff::Update {
                    name,
                    new_branches,
                    new_tags,
                    ..
                } => {
                    sync.update_environment(&self.org, &self.name, name, new_branches, new_tags)
                        .await?;
                }
                EnvironmentDiff::Delete(name) => {
                    sync.delete_environment(&self.org, &self.name, name).await?;
                }
            }
        }

        if !is_unarchive && self.settings_diff.0 != self.settings_diff.1 {
            sync.edit_repo(&self.org, &self.name, &self.settings_diff.1)
                .await?;
        }

        for app_installation in &self.app_installation_diffs {
            app_installation
                .apply(sync, self.repo_id, &self.org)
                .await?;
        }

        Ok(())
    }
}

impl std::fmt::Display for UpdateRepoDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.noop() {
            return Ok(());
        }

        let UpdateRepoDiff {
            org,
            name,
            repo_node_id: _,
            repo_id: _,
            settings_diff,
            permission_diffs,
            branch_protection_diffs,
            ruleset_diffs,
            environment_diffs,
            app_installation_diffs,
        } = self;

        writeln!(f, "📝 Editing repo '{org}/{name}':")?;
        let (settings_old, settings_new) = &settings_diff;
        let RepoSettings {
            description,
            homepage,
            archived,
            auto_merge_enabled,
        } = settings_old;
        match (description.as_str(), settings_new.description.as_str()) {
            ("", "") => {}
            ("", new) => writeln!(f, "  Set description: '{new}'")?,
            (old, "") => writeln!(f, "  Remove description: '{old}'")?,
            (old, new) if old != new => writeln!(f, "  New description: '{old}' => '{new}'")?,
            _ => {}
        }
        match (homepage, &settings_new.homepage) {
            (None, Some(new)) => writeln!(f, "  Set homepage: '{new}'")?,
            (Some(old), None) => writeln!(f, "  Remove homepage: '{old}'")?,
            (Some(old), Some(new)) if old != new => {
                writeln!(f, "  New homepage: '{old}' => '{new}'")?
            }
            _ => {}
        }
        match (archived, &settings_new.archived) {
            (false, true) => writeln!(f, "  Archive")?,
            (true, false) => writeln!(f, "  Unarchive")?,
            _ => {}
        }
        match (auto_merge_enabled, &settings_new.auto_merge_enabled) {
            (false, true) => writeln!(f, "  Enable auto-merge")?,
            (true, false) => writeln!(f, "  Disable auto-merge")?,
            _ => {}
        }
        if !permission_diffs.is_empty() {
            writeln!(f, "  Permission Changes:")?;
            for permission_diff in permission_diffs {
                write!(f, "{permission_diff}")?;
            }
        }
        if !branch_protection_diffs.is_empty() {
            writeln!(f, "  Branch Protections:")?;
            for branch_protection_diff in branch_protection_diffs {
                write!(f, "{branch_protection_diff}")?;
            }
        }
        if !ruleset_diffs.is_empty() {
            writeln!(f, "  Rulesets:")?;
            for ruleset_diff in ruleset_diffs {
                write!(f, "{ruleset_diff}")?;
            }
        }
        if !environment_diffs.is_empty() {
            writeln!(f, "  Environments:")?;
            for env_diff in environment_diffs {
                match env_diff {
                    EnvironmentDiff::Create(name, env) => {
                        writeln!(f, "    ➕ Create: {name}")?;
                        if !env.branches.is_empty() {
                            writeln!(f, "        Branches: {}", env.branches.join(", "))?;
                        }
                        if !env.tags.is_empty() {
                            writeln!(f, "        Tags: {}", env.tags.join(", "))?;
                        }
                    }
                    EnvironmentDiff::Update {
                        name,
                        add_branches,
                        remove_branches,
                        add_tags,
                        remove_tags,
                        new_branches: _,
                        new_tags: _,
                    } => {
                        writeln!(f, "    🔄 Update: {name}")?;
                        if !add_branches.is_empty() {
                            writeln!(f, "        Adding branches: {}", add_branches.join(", "))?;
                        }
                        if !remove_branches.is_empty() {
                            writeln!(
                                f,
                                "        Removing branches: {}",
                                remove_branches.join(", ")
                            )?;
                        }
                        if !add_tags.is_empty() {
                            writeln!(f, "        Adding tags: {}", add_tags.join(", "))?;
                        }
                        if !remove_tags.is_empty() {
                            writeln!(f, "        Removing tags: {}", remove_tags.join(", "))?;
                        }
                        if add_branches.is_empty()
                            && remove_branches.is_empty()
                            && add_tags.is_empty()
                            && remove_tags.is_empty()
                        {
                            writeln!(f, "        No pattern changes")?;
                        }
                    }
                    EnvironmentDiff::Delete(name) => writeln!(f, "    ❌ Delete: {name}")?,
                }
            }
        }

        if !app_installation_diffs.is_empty() {
            writeln!(f, "  App installation changes:")?;

            for diff in app_installation_diffs {
                write!(f, "{diff}")?;
            }
        }

        Ok(())
    }
}

#[derive(Debug)]
struct RepoPermissionAssignmentDiff {
    collaborator: RepoCollaborator,
    diff: RepoPermissionDiff,
}

impl RepoPermissionAssignmentDiff {
    async fn apply(&self, sync: &GitHubWrite, org: &str, repo_name: &str) -> anyhow::Result<()> {
        match &self.diff {
            RepoPermissionDiff::Create(p) | RepoPermissionDiff::Update(_, p) => {
                match &self.collaborator {
                    RepoCollaborator::Team(team_name) => {
                        sync.update_team_repo_permissions(org, repo_name, team_name, p)
                            .await?
                    }
                    RepoCollaborator::User(user_name) => {
                        sync.update_user_repo_permissions(org, repo_name, user_name, p)
                            .await?
                    }
                }
            }
            RepoPermissionDiff::Delete(_) => match &self.collaborator {
                RepoCollaborator::Team(team_name) => {
                    sync.remove_team_from_repo(org, repo_name, team_name)
                        .await?
                }
                RepoCollaborator::User(user_name) => {
                    sync.remove_collaborator_from_repo(org, repo_name, user_name)
                        .await?
                }
            },
        }
        Ok(())
    }
}

impl std::fmt::Display for RepoPermissionAssignmentDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let RepoPermissionAssignmentDiff { collaborator, diff } = self;

        let name = match &collaborator {
            RepoCollaborator::Team(name) => format!("team '{name}'"),
            RepoCollaborator::User(name) => format!("user '{name}'"),
        };
        match &diff {
            RepoPermissionDiff::Create(p) => {
                writeln!(f, "    Giving {name} {p} permission")
            }
            RepoPermissionDiff::Update(old, new) => {
                writeln!(f, "    Changing {name}'s permission from {old} to {new}")
            }
            RepoPermissionDiff::Delete(p) => {
                writeln!(f, "    Removing {name}'s {p} permission ")
            }
        }
    }
}

#[derive(Debug)]
enum RepoPermissionDiff {
    Create(RepoPermission),
    Update(RepoPermission, RepoPermission),
    Delete(RepoPermission),
}

#[derive(Clone, Debug)]
enum RepoCollaborator {
    Team(String),
    User(String),
}

#[derive(Debug)]
struct BranchProtectionDiff {
    pattern: String,
    operation: BranchProtectionDiffOperation,
}

impl BranchProtectionDiff {
    async fn apply(
        &self,
        sync: &GitHubWrite,
        org: &str,
        repo_name: &str,
        repo_id: &str,
    ) -> anyhow::Result<()> {
        match &self.operation {
            BranchProtectionDiffOperation::Create(bp) => {
                sync.upsert_branch_protection(
                    BranchProtectionOp::CreateForRepo(repo_id.to_string()),
                    &self.pattern,
                    bp,
                    org,
                )
                .await?;
            }
            BranchProtectionDiffOperation::Update(id, _, bp) => {
                sync.upsert_branch_protection(
                    BranchProtectionOp::UpdateBranchProtection(id.clone()),
                    &self.pattern,
                    bp,
                    org,
                )
                .await?;
            }
            BranchProtectionDiffOperation::Delete(id) => {
                debug!(
                    "Deleting branch protection '{}' on '{}/{}' as \
                the protection is not in the team repo",
                    self.pattern, org, repo_name
                );
                sync.delete_branch_protection(org, repo_name, id).await?;
            }
        }

        Ok(())
    }
}

impl std::fmt::Display for BranchProtectionDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "      {}", self.pattern)?;
        match &self.operation {
            BranchProtectionDiffOperation::Create(bp) => log_branch_protection(bp, None, f),
            BranchProtectionDiffOperation::Update(_, old, new) => {
                log_branch_protection(old, Some(new), f)
            }
            BranchProtectionDiffOperation::Delete(_) => {
                writeln!(f, "        Deleting branch protection")
            }
        }
    }
}

/// Logs a field diff. When `new` is `Some`, only prints if the value changed.
/// When `new` is `None` (creation), always prints the current value.
fn log_field<T: PartialEq + std::fmt::Debug>(
    label: &str,
    old: &T,
    new: Option<&T>,
    result: &mut dyn Write,
) -> std::fmt::Result {
    match new {
        Some(new_val) => {
            if old != new_val {
                writeln!(result, "        {label}: {old:?} => {new_val:?}")?;
            }
        }
        None => {
            writeln!(result, "        {label}: {old:?}")?;
        }
    }
    Ok(())
}

fn log_field_if_not_default<T: PartialEq + std::fmt::Debug + Default>(
    label: &str,
    old: &T,
    new: Option<&T>,
    result: &mut dyn Write,
) -> std::fmt::Result {
    match new {
        Some(new_val) => log_field(label, old, Some(new_val), result)?,
        None => {
            if old != &T::default() {
                writeln!(result, "        {label}: {old:?}")?;
            }
        }
    }
    Ok(())
}

fn log_branch_protection(
    current: &api::BranchProtection,
    new: Option<&api::BranchProtection>,
    mut result: impl Write,
) -> std::fmt::Result {
    log_field(
        "Require branches to be up to date",
        &current.requires_strict_status_checks,
        new.map(|n| &n.requires_strict_status_checks),
        &mut result,
    )?;
    log_field(
        "Dismiss Stale Reviews",
        &current.dismisses_stale_reviews,
        new.map(|n| &n.dismisses_stale_reviews),
        &mut result,
    )?;
    log_field(
        "Require conversation resolution",
        &current.requires_conversation_resolution,
        new.map(|n| &n.requires_conversation_resolution),
        &mut result,
    )?;
    log_field(
        "Require linear history",
        &current.requires_linear_history,
        new.map(|n| &n.requires_linear_history),
        &mut result,
    )?;
    log_field(
        "Is admin enforced",
        &current.is_admin_enforced,
        new.map(|n| &n.is_admin_enforced),
        &mut result,
    )?;
    log_field(
        "Required Approving Review Count",
        &current.required_approving_review_count,
        new.map(|n| &n.required_approving_review_count),
        &mut result,
    )?;
    log_field(
        "Requires PR",
        &current.requires_approving_reviews,
        new.map(|n| &n.requires_approving_reviews),
        &mut result,
    )?;
    log_field(
        "Required Checks",
        &current.required_status_check_contexts,
        new.map(|n| &n.required_status_check_contexts),
        &mut result,
    )?;
    log_field(
        "Allowances",
        &current.push_allowances,
        new.map(|n| &n.push_allowances),
        &mut result,
    )?;
    Ok(())
}

fn log_ruleset(
    current: &api::Ruleset,
    new: Option<&api::Ruleset>,
    mut result: impl Write,
) -> std::fmt::Result {
    // Log basic ruleset properties
    log_field_if_not_default(
        "Target",
        &current.target,
        new.map(|n| &n.target),
        &mut result,
    )?;
    log_field_if_not_default(
        "Source Type",
        &current.source_type,
        new.map(|n| &n.source_type),
        &mut result,
    )?;
    log_field_if_not_default(
        "Enforcement",
        &current.enforcement,
        new.map(|n| &n.enforcement),
        &mut result,
    )?;

    let (include_label, exclude_label) = match current.target {
        api::RulesetTarget::Branch => ("Include Branches", "Exclude Branches"),
        api::RulesetTarget::Tag => ("Include Tags", "Exclude Tags"),
    };

    let ref_name = &current.conditions.ref_name;
    let new_ref_name = new.map(|n| &n.conditions.ref_name);
    log_field(
        include_label,
        &ref_name.include,
        new_ref_name.map(|r| &r.include),
        &mut result,
    )?;
    log_field_if_not_default(
        exclude_label,
        &ref_name.exclude,
        new_ref_name.map(|r| &r.exclude),
        &mut result,
    )?;

    log_field_if_not_default(
        "Bypass Actors",
        &current.bypass_actors,
        new.map(|n| &n.bypass_actors),
        &mut result,
    )?;

    #[derive(PartialEq, Eq)]
    enum RuleValue {
        Bool(bool),
        Number(i32),
        String(String),
    }

    /// Store the default value of a rule (if it has one) to suppress noisy
    /// logging when a full ruleset is created or a rule is added/removed.
    #[derive(PartialEq, Eq)]
    struct LoggedRule {
        value: RuleValue,
        default: Option<RuleValue>,
    }

    impl Display for LoggedRule {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match &self.value {
                RuleValue::Bool(val) => val.fmt(f),
                RuleValue::Number(val) => val.fmt(f),
                RuleValue::String(val) => val.fmt(f),
            }
        }
    }

    impl LoggedRule {
        fn bool_with_default(value: bool, default: bool) -> Self {
            Self {
                value: RuleValue::Bool(value),
                default: Some(RuleValue::Bool(default)),
            }
        }

        fn number(value: i32) -> Self {
            Self {
                value: RuleValue::Number(value),
                default: None,
            }
        }

        fn number_with_default(value: i32, default: i32) -> Self {
            Self {
                value: RuleValue::Number(value),
                default: Some(RuleValue::Number(default)),
            }
        }

        fn string(value: String) -> Self {
            Self {
                value: RuleValue::String(value),
                default: None,
            }
        }

        fn string_with_default(value: String, default: String) -> Self {
            Self {
                value: RuleValue::String(value),
                default: Some(RuleValue::String(default)),
            }
        }

        fn is_default(&self) -> bool {
            self.default.as_ref() == Some(&self.value)
        }
    }

    // The list representation of rules makes it a bit annoying to diff and print
    // So we normalize the rules to a set of key-value pairs, and then diff those
    fn record_rules(ruleset: &Ruleset) -> BTreeMap<&'static str, LoggedRule> {
        const FORBID_FORCE_PUSHES: &str = "Forbid force pushes";
        const REQUIRE_PULL_REQUESTS: &str = "Require pull requests";
        const RESTRICT_CREATIONS: &str = "Restrict creations";
        const RESTRICT_DELETIONS: &str = "Restrict deletions";

        // Some default-true toggles are represented by the presence of a rule,
        // so seed their disabled state to keep non-default `false` values visible.
        let mut rules = BTreeMap::from([
            (
                FORBID_FORCE_PUSHES,
                LoggedRule::bool_with_default(
                    false,
                    schema::branch_protection_default_prevent_force_push(),
                ),
            ),
            (
                REQUIRE_PULL_REQUESTS,
                LoggedRule::bool_with_default(false, REQUIRE_PULL_REQUESTS_DEFAULT),
            ),
            (
                RESTRICT_CREATIONS,
                LoggedRule::bool_with_default(
                    false,
                    schema::branch_protection_default_prevent_creation(),
                ),
            ),
            (
                RESTRICT_DELETIONS,
                LoggedRule::bool_with_default(
                    false,
                    schema::branch_protection_default_prevent_deletion(),
                ),
            ),
        ]);

        for rule in &ruleset.rules {
            match rule {
                api::RulesetRule::Creation => {
                    rules.insert(
                        RESTRICT_CREATIONS,
                        LoggedRule::bool_with_default(
                            true,
                            schema::branch_protection_default_prevent_creation(),
                        ),
                    );
                }
                api::RulesetRule::Update => {
                    rules.insert(
                        "Restrict updates",
                        LoggedRule::bool_with_default(
                            true,
                            schema::branch_protection_default_prevent_update(),
                        ),
                    );
                }
                api::RulesetRule::Deletion => {
                    rules.insert(
                        RESTRICT_DELETIONS,
                        LoggedRule::bool_with_default(
                            true,
                            schema::branch_protection_default_prevent_deletion(),
                        ),
                    );
                }
                api::RulesetRule::RequiredLinearHistory => {
                    rules.insert(
                        "Require linear history",
                        LoggedRule::bool_with_default(true, false),
                    );
                }
                api::RulesetRule::RequiredSignatures => {
                    rules.insert(
                        "Require signed commits",
                        LoggedRule::bool_with_default(true, false),
                    );
                }
                api::RulesetRule::NonFastForward => {
                    rules.insert(
                        FORBID_FORCE_PUSHES,
                        LoggedRule::bool_with_default(
                            true,
                            schema::branch_protection_default_prevent_force_push(),
                        ),
                    );
                }
                api::RulesetRule::MergeQueue { parameters } => {
                    let default_parameters = api::MergeQueueParameters::default();
                    rules.insert(
                        "Require merge queue",
                        LoggedRule::bool_with_default(true, false),
                    );
                    let api::MergeQueueParameters {
                        check_response_timeout_minutes,
                        grouping_strategy,
                        max_entries_to_build,
                        max_entries_to_merge,
                        merge_method,
                        min_entries_to_merge,
                        min_entries_to_merge_wait_minutes,
                    } = parameters;
                    rules.insert(
                        "Merge queue timeout",
                        LoggedRule::number_with_default(
                            *check_response_timeout_minutes,
                            default_parameters.check_response_timeout_minutes,
                        ),
                    );
                    rules.insert(
                        "Merge queue grouping strategy",
                        LoggedRule::string_with_default(
                            format!("{grouping_strategy:?}"),
                            format!("{:?}", default_parameters.grouping_strategy),
                        ),
                    );
                    rules.insert(
                        "Merge queue max entries to build",
                        LoggedRule::number_with_default(
                            *max_entries_to_build,
                            default_parameters.max_entries_to_build,
                        ),
                    );
                    rules.insert(
                        "Merge queue max entries to merge",
                        LoggedRule::number_with_default(
                            *max_entries_to_merge,
                            default_parameters.max_entries_to_merge,
                        ),
                    );
                    rules.insert(
                        "Merge queue min entries to merge",
                        LoggedRule::number_with_default(
                            *min_entries_to_merge,
                            default_parameters.min_entries_to_merge,
                        ),
                    );
                    rules.insert(
                        "Merge queue merge_method",
                        LoggedRule::string_with_default(
                            format!("{merge_method:?}"),
                            format!("{:?}", default_parameters.merge_method),
                        ),
                    );
                    rules.insert(
                        "Merge queue wait time for min group size",
                        LoggedRule::number_with_default(
                            *min_entries_to_merge_wait_minutes,
                            default_parameters.min_entries_to_merge_wait_minutes,
                        ),
                    );
                }
                api::RulesetRule::PullRequest { parameters } => {
                    rules.insert(
                        REQUIRE_PULL_REQUESTS,
                        LoggedRule::bool_with_default(true, REQUIRE_PULL_REQUESTS_DEFAULT),
                    );
                    rules.insert(
                        "Dismiss stale reviews on push",
                        LoggedRule::bool_with_default(
                            parameters.dismiss_stale_reviews_on_push,
                            false,
                        ),
                    );
                    rules.insert(
                        "Require code owner review",
                        LoggedRule::bool_with_default(
                            parameters.require_code_owner_review,
                            REQUIRE_CODE_OWNER_REVIEW_DEFAULT,
                        ),
                    );
                    rules.insert(
                        "Require last push approval",
                        LoggedRule::bool_with_default(
                            parameters.require_last_push_approval,
                            REQUIRE_LAST_PUSH_APPROVAL_DEFAULT,
                        ),
                    );
                    rules.insert(
                        "Required approvals",
                        LoggedRule::number(parameters.required_approving_review_count),
                    );
                    rules.insert(
                        "Require review thread resolution",
                        LoggedRule::bool_with_default(
                            parameters.required_review_thread_resolution,
                            REQUIRED_REVIEW_THREAD_RESOLUTION_DEFAULT,
                        ),
                    );
                }
                api::RulesetRule::RequiredStatusChecks { parameters } => {
                    rules.insert(
                        "Require branches to be up to date",
                        LoggedRule::bool_with_default(
                            parameters.strict_required_status_checks_policy,
                            STRICT_REQUIRED_STATUS_CHECKS_POLICY_DEFAULT,
                        ),
                    );
                    let mut checks: Vec<String> = parameters
                        .required_status_checks
                        .iter()
                        .map(|check| {
                            if let Some(integration_id) = check.integration_id {
                                format!("{} (integration_id: {integration_id})", check.context)
                            } else {
                                format!("{} (any integration)", check.context)
                            }
                        })
                        .collect();
                    checks.sort();
                    rules.insert(
                        "Required status checks",
                        LoggedRule::string(checks.join(", ")),
                    );
                }
                api::RulesetRule::RequiredDeployments { parameters } => {
                    let mut envs = parameters.required_deployment_environments.clone();
                    envs.sort();
                    rules.insert(
                        "Required deployment environments",
                        LoggedRule::string(envs.join(", ")),
                    );
                }
            }
        }
        rules
    }

    let old_rules = record_rules(current);
    let new_rules = new.map(record_rules);

    if let Some(new_rules) = new_rules {
        for (name, old_value) in &old_rules {
            if let Some(new_value) = new_rules.get(name) {
                // Updated rule
                if new_value.value != old_value.value {
                    writeln!(result, "        {name}: {old_value} => {new_value}")?;
                }
            } else {
                // The rule is not present anymore in the new ruleset, so it was deleted
                writeln!(
                    result,
                    "        {name}: {}",
                    match &old_value.value {
                        RuleValue::Bool(val) => format!("deleting `{val}`"),
                        RuleValue::Number(val) => format!("deleting `{val}`"),
                        RuleValue::String(val) => format!("deleting `{val}`"),
                    }
                )?;
            }
        }

        // Created rules
        for (name, new_value) in new_rules {
            if !old_rules.contains_key(name) {
                writeln!(result, "        {name}: {new_value}")?;
            }
        }
    } else {
        // The entire ruleset is new
        for (name, value) in old_rules {
            if value.is_default() {
                // Hide default-valued rules on creation as
                // they don't represent a meaningful change to the user.
                continue;
            }
            writeln!(result, "        {name}: {value}")?;
        }
    }

    Ok(())
}

#[derive(Debug)]
enum BranchProtectionDiffOperation {
    Create(api::BranchProtection),
    Update(String, api::BranchProtection, api::BranchProtection),
    Delete(String),
}

#[derive(Debug)]
struct RulesetDiff {
    name: String,
    operation: RulesetDiffOperation,
}

impl RulesetDiff {
    async fn apply(&self, sync: &GitHubWrite, org: &str, repo_name: &str) -> anyhow::Result<()> {
        use api::RulesetOp;
        match &self.operation {
            RulesetDiffOperation::Create(ruleset) => {
                sync.upsert_ruleset(RulesetOp::CreateForRepo, org, repo_name, ruleset)
                    .await?;
            }
            RulesetDiffOperation::Update(id, _, new_ruleset) => {
                sync.upsert_ruleset(RulesetOp::UpdateRuleset(*id), org, repo_name, new_ruleset)
                    .await?;
            }
            RulesetDiffOperation::Delete(id) => {
                debug!(
                    "Deleting ruleset '{}' (id: {}) on '{}/{}' as \
                the ruleset is not in the team repo",
                    self.name, id, org, repo_name
                );
                sync.delete_ruleset(org, repo_name, *id).await?;
            }
        }
        Ok(())
    }
}

impl std::fmt::Display for RulesetDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let action = match self.operation {
            RulesetDiffOperation::Create(_) => "Creating",
            RulesetDiffOperation::Update(_, _, _) => "Updating",
            RulesetDiffOperation::Delete(_) => "Deleting",
        };
        writeln!(f, "      {action} '{}'", self.name)?;
        match &self.operation {
            RulesetDiffOperation::Create(ruleset) => log_ruleset(ruleset, None, f),
            RulesetDiffOperation::Update(_, old, new) => log_ruleset(old, Some(new), f),
            RulesetDiffOperation::Delete(_) => Ok(()),
        }
    }
}

#[derive(Debug)]
enum RulesetDiffOperation {
    Create(api::Ruleset),
    Update(i64, api::Ruleset, api::Ruleset), // id, old, new
    Delete(i64),
}

#[derive(Debug)]
enum TeamDiff {
    Create(CreateTeamDiff),
    Edit(EditTeamDiff),
    Delete(DeleteTeamDiff),
}

impl TeamDiff {
    async fn apply(self, sync: &GitHubWrite) -> anyhow::Result<()> {
        match self {
            TeamDiff::Create(c) => c.apply(sync).await?,
            TeamDiff::Edit(e) => e.apply(sync).await?,
            TeamDiff::Delete(d) => d.apply(sync).await?,
        }

        Ok(())
    }

    fn noop(&self) -> bool {
        match self {
            TeamDiff::Create(_) | TeamDiff::Delete(_) => false,
            TeamDiff::Edit(e) => e.noop(),
        }
    }
}

impl std::fmt::Display for TeamDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TeamDiff::Create(c) => write!(f, "{c}"),
            TeamDiff::Edit(e) => write!(f, "{e}"),
            TeamDiff::Delete(d) => write!(f, "{d}"),
        }
    }
}

#[derive(Debug)]
struct CreateTeamDiff {
    org: String,
    name: String,
    description: String,
    privacy: TeamPrivacy,
    members: Vec<(String, TeamRole)>,
}

impl CreateTeamDiff {
    async fn apply(self, sync: &GitHubWrite) -> anyhow::Result<()> {
        sync.create_team(&self.org, &self.name, &self.description, self.privacy)
            .await?;
        for (member_name, role) in self.members {
            MemberDiff::Create(role)
                .apply(&self.org, &self.name, &member_name, sync)
                .await?;
        }

        Ok(())
    }
}

impl std::fmt::Display for CreateTeamDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let CreateTeamDiff {
            org,
            name,
            description,
            privacy,
            members,
        } = self;

        writeln!(f, "➕ Creating team:")?;
        writeln!(f, "  Org: {org}")?;
        writeln!(f, "  Name: {name}")?;
        writeln!(f, "  Description: {description}")?;
        writeln!(
            f,
            "  Privacy: {}",
            match privacy {
                TeamPrivacy::Secret => "secret",
                TeamPrivacy::Closed => "closed",
            }
        )?;
        writeln!(f, "  Members:")?;
        for (name, role) in members {
            writeln!(f, "    {name}: {role}")?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct EditTeamDiff {
    org: String,
    name: String,
    name_diff: Option<String>,
    description_diff: Option<(String, String)>,
    privacy_diff: Option<(TeamPrivacy, TeamPrivacy)>,
    member_diffs: Vec<(String, MemberDiff)>,
}

impl EditTeamDiff {
    async fn apply(self, sync: &GitHubWrite) -> anyhow::Result<()> {
        if self.name_diff.is_some()
            || self.description_diff.is_some()
            || self.privacy_diff.is_some()
        {
            sync.edit_team(
                &self.org,
                &self.name,
                self.name_diff.as_deref(),
                self.description_diff.as_ref().map(|(_, d)| d.as_str()),
                self.privacy_diff.map(|(_, p)| p),
            )
            .await?;
        }

        for (member_name, member_diff) in self.member_diffs {
            member_diff
                .apply(&self.org, &self.name, &member_name, sync)
                .await?;
        }

        Ok(())
    }

    fn noop(&self) -> bool {
        let EditTeamDiff {
            org: _,
            name: _,
            name_diff,
            description_diff,
            privacy_diff,
            member_diffs,
        } = self;

        name_diff.is_none()
            && description_diff.is_none()
            && privacy_diff.is_none()
            && member_diffs.iter().all(|(_, d)| d.is_noop())
    }
}

impl std::fmt::Display for EditTeamDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.noop() {
            return Ok(());
        }

        let EditTeamDiff {
            org,
            name,
            name_diff,
            description_diff,
            privacy_diff,
            member_diffs,
        } = self;

        writeln!(f, "📝 Editing team '{org}/{name}':")?;
        if let Some(n) = name_diff {
            writeln!(f, "  New name: {n}")?;
        }
        if let Some((old, new)) = &description_diff {
            writeln!(f, "  New description: '{old}' => '{new}'")?;
        }
        if let Some((old, new)) = &privacy_diff {
            let display = |privacy: &TeamPrivacy| match privacy {
                TeamPrivacy::Secret => "secret",
                TeamPrivacy::Closed => "closed",
            };
            writeln!(f, "  New privacy: '{}' => '{}'", display(old), display(new))?;
        }
        for (member, diff) in member_diffs {
            match diff {
                MemberDiff::Create(r) => {
                    writeln!(f, "  Adding member '{member}' with {r} role")?;
                }
                MemberDiff::ChangeRole((o, n)) => {
                    writeln!(f, "  Changing '{member}' role from {o} to {n}")?;
                }
                MemberDiff::Delete => {
                    writeln!(f, "  Deleting member '{member}'")?;
                }
                MemberDiff::Noop => {}
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
enum MemberDiff {
    Create(TeamRole),
    ChangeRole((TeamRole, TeamRole)),
    Delete,
    Noop,
}

impl MemberDiff {
    async fn apply(
        self,
        org: &str,
        team: &str,
        member: &str,
        sync: &GitHubWrite,
    ) -> anyhow::Result<()> {
        match self {
            MemberDiff::Create(role) | MemberDiff::ChangeRole((_, role)) => {
                sync.set_team_membership(org, team, member, role).await?;
            }
            MemberDiff::Delete => sync.remove_team_membership(org, team, member).await?,
            MemberDiff::Noop => {}
        }

        Ok(())
    }

    fn is_noop(&self) -> bool {
        matches!(self, Self::Noop)
    }
}

#[derive(Debug)]
struct DeleteTeamDiff {
    org: String,
    name: String,
    slug: String,
}

impl DeleteTeamDiff {
    async fn apply(self, sync: &GitHubWrite) -> anyhow::Result<()> {
        sync.delete_team(&self.org, &self.slug).await?;
        Ok(())
    }
}

impl std::fmt::Display for DeleteTeamDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "❌ Deleting team '{}/{}'", self.org, self.name)?;
        Ok(())
    }
}

#[derive(Debug)]
enum AppInstallationDiff {
    Add(AppInstallation),
    Remove(AppInstallation),
}

impl AppInstallationDiff {
    async fn apply(&self, sync: &GitHubWrite, repo_id: u64, org: &str) -> anyhow::Result<()> {
        match self {
            AppInstallationDiff::Add(app) => {
                sync.add_repo_to_app_installation(app.installation_id, repo_id, org)
                    .await?;
            }
            AppInstallationDiff::Remove(app) => {
                sync.remove_repo_from_app_installation(app.installation_id, repo_id, org)
                    .await?;
            }
        }
        Ok(())
    }
}

impl std::fmt::Display for AppInstallationDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppInstallationDiff::Add(app) => {
                writeln!(f, "    Install app {}", app.app)
            }
            AppInstallationDiff::Remove(app) => {
                writeln!(f, "    Remove app {}", app.app)
            }
        }
    }
}
