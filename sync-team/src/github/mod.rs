mod api;
#[cfg(test)]
mod tests;

use self::api::{BranchProtectionOp, TeamPrivacy, TeamRole};
use crate::github::api::{GithubRead, Login, PushAllowanceActor, RepoPermission, RepoSettings};
use log::debug;
use rust_team_data::v1::{Bot, BranchProtectionMode};
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter, Write};

pub(crate) use self::api::{GitHubApiRead, GitHubWrite, HttpClient};

static DEFAULT_DESCRIPTION: &str = "Managed by the rust-lang/team repository.";
static DEFAULT_PRIVACY: TeamPrivacy = TeamPrivacy::Closed;

pub(crate) fn create_diff(
    github: Box<dyn GithubRead>,
    teams: Vec<rust_team_data::v1::Team>,
    repos: Vec<rust_team_data::v1::Repo>,
) -> anyhow::Result<Diff> {
    let github = SyncGitHub::new(github, teams, repos)?;
    github.diff_all()
}

type OrgName = String;
type RepoName = String;

#[derive(Copy, Clone, Debug, PartialEq)]
enum GithubApp {
    RenovateBot,
}

impl GithubApp {
    fn from_id(app_id: u64) -> Option<Self> {
        match app_id {
            2740 => Some(GithubApp::RenovateBot),
            _ => None,
        }
    }
}

impl Display for GithubApp {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            GithubApp::RenovateBot => f.write_str("RenovateBot"),
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
    usernames_cache: HashMap<u64, String>,
    org_owners: HashMap<OrgName, HashSet<u64>>,
    org_apps: HashMap<OrgName, Vec<OrgAppInstallation>>,
}

impl SyncGitHub {
    pub(crate) fn new(
        github: Box<dyn GithubRead>,
        teams: Vec<rust_team_data::v1::Team>,
        repos: Vec<rust_team_data::v1::Repo>,
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
        let usernames_cache = github.usernames(&users)?;

        debug!("caching organization owners");
        let orgs = teams
            .iter()
            .filter_map(|t| t.github.as_ref())
            .flat_map(|gh| &gh.teams)
            .map(|gh_team| &gh_team.org)
            .collect::<HashSet<_>>();

        let mut org_owners = HashMap::new();
        let mut org_apps = HashMap::new();

        for org in &orgs {
            org_owners.insert((*org).to_string(), github.org_owners(org)?);

            let mut installations: Vec<OrgAppInstallation> = vec![];

            for installation in github.org_app_installations(org)? {
                if let Some(app) = GithubApp::from_id(installation.app_id) {
                    let mut repositories = HashSet::new();
                    for repo_installation in
                        github.app_installation_repos(installation.installation_id)?
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
            usernames_cache,
            org_owners,
            org_apps,
        })
    }

    pub(crate) fn diff_all(&self) -> anyhow::Result<Diff> {
        let team_diffs = self.diff_teams()?;
        let repo_diffs = self.diff_repos()?;

        Ok(Diff {
            team_diffs,
            repo_diffs,
        })
    }

    fn diff_teams(&self) -> anyhow::Result<Vec<TeamDiff>> {
        let mut diffs = Vec::new();
        let mut unseen_github_teams = HashMap::new();
        for team in &self.teams {
            if let Some(gh) = &team.github {
                for github_team in &gh.teams {
                    // Get existing teams we haven't seen yet
                    let unseen_github_teams = match unseen_github_teams.get_mut(&github_team.org) {
                        Some(ts) => ts,
                        None => {
                            let ts: HashMap<_, _> = self
                                .github
                                .org_teams(&github_team.org)?
                                .into_iter()
                                .collect();
                            unseen_github_teams
                                .entry(github_team.org.clone())
                                .or_insert(ts)
                        }
                    };
                    // Remove the current team from the collection of unseen GitHub teams
                    unseen_github_teams.remove(&github_team.name);

                    diffs.push(self.diff_team(github_team)?);
                }
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

    fn diff_team(&self, github_team: &rust_team_data::v1::GitHubTeam) -> anyhow::Result<TeamDiff> {
        // Ensure the team exists and is consistent
        let team = match self.github.team(&github_team.org, &github_team.name)? {
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

        let mut current_members = self.github.team_memberships(&team)?;
        let invites = self
            .github
            .team_membership_invitations(&github_team.org, &github_team.name)?;

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

    fn diff_repos(&self) -> anyhow::Result<Vec<RepoDiff>> {
        let mut diffs = Vec::new();
        for repo in &self.repos {
            diffs.push(self.diff_repo(repo)?);
        }
        Ok(diffs)
    }

    fn diff_repo(&self, expected_repo: &rust_team_data::v1::Repo) -> anyhow::Result<RepoDiff> {
        let actual_repo = match self.github.repo(&expected_repo.org, &expected_repo.name)? {
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

                return Ok(RepoDiff::Create(CreateRepoDiff {
                    org: expected_repo.org.clone(),
                    name: expected_repo.name.clone(),
                    settings: RepoSettings {
                        description: Some(expected_repo.description.clone()),
                        homepage: expected_repo.homepage.clone(),
                        archived: false,
                        auto_merge_enabled: expected_repo.auto_merge_enabled,
                    },
                    permissions,
                    branch_protections,
                    app_installations: self.diff_app_installations(expected_repo, &[])?,
                }));
            }
        };

        let permission_diffs = self.diff_permissions(expected_repo)?;
        let branch_protection_diffs = self.diff_branch_protections(&actual_repo, expected_repo)?;
        let old_settings = RepoSettings {
            description: actual_repo.description.clone(),
            homepage: actual_repo.homepage.clone(),
            archived: actual_repo.archived,
            auto_merge_enabled: actual_repo.allow_auto_merge.unwrap_or(false),
        };
        let new_settings = RepoSettings {
            description: Some(expected_repo.description.clone()),
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
                        // Only load installations from apps that we know about, to avoid removing
                        // unknown installations.
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
            app_installation_diffs,
        }))
    }

    fn diff_permissions(
        &self,
        expected_repo: &rust_team_data::v1::Repo,
    ) -> anyhow::Result<Vec<RepoPermissionAssignmentDiff>> {
        let actual_teams: HashMap<_, _> = self
            .github
            .repo_teams(&expected_repo.org, &expected_repo.name)?
            .into_iter()
            .map(|t| (t.name.clone(), t))
            .collect();
        let actual_collaborators: HashMap<_, _> = self
            .github
            .repo_collaborators(&expected_repo.org, &expected_repo.name)?
            .into_iter()
            .map(|u| (u.name.clone(), u))
            .collect();

        calculate_permission_diffs(expected_repo, actual_teams, actual_collaborators)
    }

    fn diff_branch_protections(
        &self,
        actual_repo: &api::Repo,
        expected_repo: &rust_team_data::v1::Repo,
    ) -> anyhow::Result<Vec<BranchProtectionDiff>> {
        let mut branch_protection_diffs = Vec::new();
        let mut actual_protections = self
            .github
            .branch_protections(&actual_repo.org, &actual_repo.name)?;
        for branch_protection in &expected_repo.branch_protections {
            let actual_branch_protection = actual_protections.remove(&branch_protection.pattern);
            let expected_branch_protection =
                construct_branch_protection(expected_repo, branch_protection);
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
            })
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
            _ => None,
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
                log::warn!("Application {app} should be enabled for repository {}/{}, but it is not installed on GitHub", expected_repo.org, expected_repo.name);
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
    let bots = expected_repo.bots.iter().filter_map(|b| {
        let bot_user_name = bot_user_name(b)?;
        actual_teams.remove(bot_user_name);
        Some((bot_user_name, RepoPermission::Write))
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

/// Returns `None` if the bot is not an actual bot user, but rather a GitHub app.
fn bot_user_name(bot: &Bot) -> Option<&str> {
    match bot {
        Bot::Bors => Some("bors"),
        Bot::Highfive => Some("rust-highfive"),
        Bot::RustTimer => Some("rust-timer"),
        Bot::Rustbot => Some("rustbot"),
        Bot::Rfcbot => Some("rfcbot"),
        Bot::Renovate => None,
    }
}

fn convert_permission(p: &rust_team_data::v1::RepoPermission) -> RepoPermission {
    use rust_team_data::v1;
    match *p {
        v1::RepoPermission::Write => RepoPermission::Write,
        v1::RepoPermission::Admin => RepoPermission::Admin,
        v1::RepoPermission::Maintain => RepoPermission::Maintain,
        v1::RepoPermission::Triage => RepoPermission::Triage,
    }
}

fn construct_branch_protection(
    expected_repo: &rust_team_data::v1::Repo,
    branch_protection: &rust_team_data::v1::BranchProtection,
) -> api::BranchProtection {
    let uses_bors = expected_repo.bots.contains(&Bot::Bors);
    let required_approving_review_count: u8 = if uses_bors {
        0
    } else {
        match branch_protection.mode {
            BranchProtectionMode::PrRequired {
                required_approvals, ..
            } => required_approvals
                .try_into()
                .expect("Too large required approval count"),
            BranchProtectionMode::PrNotRequired => 0,
        }
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

    if uses_bors {
        push_allowances.push(PushAllowanceActor::User(api::UserPushAllowanceActor {
            login: "bors".to_owned(),
        }));
    }
    api::BranchProtection {
        pattern: branch_protection.pattern.clone(),
        is_admin_enforced: true,
        dismisses_stale_reviews: branch_protection.dismiss_stale_review,
        required_approving_review_count,
        required_status_check_contexts: match &branch_protection.mode {
            BranchProtectionMode::PrRequired { ci_checks, .. } => ci_checks.clone(),
            BranchProtectionMode::PrNotRequired => {
                vec![]
            }
        },
        push_allowances,
        requires_approving_reviews: matches!(
            branch_protection.mode,
            BranchProtectionMode::PrRequired { .. }
        ),
    }
}

/// The special bot teams
const BOTS_TEAMS: &[&str] = &["bors", "highfive", "rfcbot", "bots"];

/// A diff between the team repo and the state on GitHub
pub(crate) struct Diff {
    team_diffs: Vec<TeamDiff>,
    repo_diffs: Vec<RepoDiff>,
}

impl Diff {
    /// Apply the diff to GitHub
    pub(crate) fn apply(self, sync: &GitHubWrite) -> anyhow::Result<()> {
        for team_diff in self.team_diffs {
            team_diff.apply(sync)?;
        }
        for repo_diff in self.repo_diffs {
            repo_diff.apply(sync)?;
        }

        Ok(())
    }
}

impl std::fmt::Display for Diff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "💻 Team Diffs:")?;
        for team_diff in &self.team_diffs {
            write!(f, "{team_diff}")?;
        }
        writeln!(f, "💻 Repo Diffs:")?;
        for repo_diff in &self.repo_diffs {
            write!(f, "{repo_diff}")?;
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
    fn apply(&self, sync: &GitHubWrite) -> anyhow::Result<()> {
        match self {
            RepoDiff::Create(c) => c.apply(sync),
            RepoDiff::Update(u) => u.apply(sync),
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
struct CreateRepoDiff {
    org: String,
    name: String,
    settings: RepoSettings,
    permissions: Vec<RepoPermissionAssignmentDiff>,
    branch_protections: Vec<(String, api::BranchProtection)>,
    app_installations: Vec<AppInstallationDiff>,
}

impl CreateRepoDiff {
    fn apply(&self, sync: &GitHubWrite) -> anyhow::Result<()> {
        let repo = sync.create_repo(&self.org, &self.name, &self.settings)?;

        for permission in &self.permissions {
            permission.apply(sync, &self.org, &self.name)?;
        }

        for (branch, protection) in &self.branch_protections {
            BranchProtectionDiff {
                pattern: branch.clone(),
                operation: BranchProtectionDiffOperation::Create(protection.clone()),
            }
            .apply(sync, &self.org, &self.name, &repo.node_id)?;
        }

        for installation in &self.app_installations {
            installation.apply(sync, repo.repo_id)?;
        }

        Ok(())
    }
}

impl std::fmt::Display for CreateRepoDiff {
    fn fmt(&self, mut f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let RepoSettings {
            description,
            homepage,
            archived: _,
            auto_merge_enabled,
        } = &self.settings;

        writeln!(f, "➕ Creating repo:")?;
        writeln!(f, "  Org: {}", self.org)?;
        writeln!(f, "  Name: {}", self.name)?;
        writeln!(f, "  Description: {:?}", description)?;
        writeln!(f, "  Homepage: {:?}", homepage)?;
        writeln!(f, "  Auto-merge: {}", auto_merge_enabled)?;
        writeln!(f, "  Permissions:")?;
        for diff in &self.permissions {
            write!(f, "{diff}")?;
        }
        writeln!(f, "  Branch Protections:")?;
        for (branch_name, branch_protection) in &self.branch_protections {
            writeln!(&mut f, "    {branch_name}")?;
            log_branch_protection(branch_protection, None, &mut f)?;
        }
        writeln!(f, "  App Installations:")?;
        for diff in &self.app_installations {
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
    app_installation_diffs: Vec<AppInstallationDiff>,
}

impl UpdateRepoDiff {
    pub(crate) fn noop(&self) -> bool {
        if !self.can_be_modified() {
            return true;
        }

        self.settings_diff.0 == self.settings_diff.1
            && self.permission_diffs.is_empty()
            && self.branch_protection_diffs.is_empty()
            && self.app_installation_diffs.is_empty()
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

    fn apply(&self, sync: &GitHubWrite) -> anyhow::Result<()> {
        if !self.can_be_modified() {
            return Ok(());
        }

        if self.settings_diff.0 != self.settings_diff.1 {
            sync.edit_repo(&self.org, &self.name, &self.settings_diff.1)?;
        }
        for permission in &self.permission_diffs {
            permission.apply(sync, &self.org, &self.name)?;
        }

        for branch_protection in &self.branch_protection_diffs {
            branch_protection.apply(sync, &self.org, &self.name, &self.repo_node_id)?;
        }

        for app_installation in &self.app_installation_diffs {
            app_installation.apply(sync, self.repo_id)?;
        }
        Ok(())
    }
}

impl std::fmt::Display for UpdateRepoDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.noop() {
            return Ok(());
        }
        writeln!(f, "📝 Editing repo '{}/{}':", self.org, self.name)?;
        let (settings_old, settings_new) = &self.settings_diff;
        let RepoSettings {
            description,
            homepage,
            archived,
            auto_merge_enabled,
        } = settings_old;
        match (description, &settings_new.description) {
            (None, Some(new)) => writeln!(f, "  Set description: '{new}'")?,
            (Some(old), None) => writeln!(f, "  Remove description: '{old}'")?,
            (Some(old), Some(new)) if old != new => {
                writeln!(f, "  New description: '{old}' => '{new}'")?
            }
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
        if !self.permission_diffs.is_empty() {
            writeln!(f, "  Permission Changes:")?;
        }
        for permission_diff in &self.permission_diffs {
            write!(f, "{permission_diff}")?;
        }
        if !self.branch_protection_diffs.is_empty() {
            writeln!(f, "  Branch Protections:")?;
        }
        for branch_protection_diff in &self.branch_protection_diffs {
            write!(f, "{branch_protection_diff}")?;
        }
        if !self.app_installation_diffs.is_empty() {
            writeln!(f, "  App installation changes:")?;
        }
        for diff in &self.app_installation_diffs {
            write!(f, "{diff}")?;
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
    fn apply(&self, sync: &GitHubWrite, org: &str, repo_name: &str) -> anyhow::Result<()> {
        match &self.diff {
            RepoPermissionDiff::Create(p) | RepoPermissionDiff::Update(_, p) => {
                match &self.collaborator {
                    RepoCollaborator::Team(team_name) => {
                        sync.update_team_repo_permissions(org, repo_name, team_name, p)?
                    }
                    RepoCollaborator::User(user_name) => {
                        sync.update_user_repo_permissions(org, repo_name, user_name, p)?
                    }
                }
            }
            RepoPermissionDiff::Delete(_) => match &self.collaborator {
                RepoCollaborator::Team(team_name) => {
                    sync.remove_team_from_repo(org, repo_name, team_name)?
                }
                RepoCollaborator::User(user_name) => {
                    sync.remove_collaborator_from_repo(org, repo_name, user_name)?
                }
            },
        }
        Ok(())
    }
}

impl std::fmt::Display for RepoPermissionAssignmentDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match &self.collaborator {
            RepoCollaborator::Team(name) => format!("team '{name}'"),
            RepoCollaborator::User(name) => format!("user '{name}'"),
        };
        match &self.diff {
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
    fn apply(
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
                )?;
            }
            BranchProtectionDiffOperation::Update(id, _, bp) => {
                sync.upsert_branch_protection(
                    BranchProtectionOp::UpdateBranchProtection(id.clone()),
                    &self.pattern,
                    bp,
                )?;
            }
            BranchProtectionDiffOperation::Delete(id) => {
                debug!(
                    "Deleting branch protection '{}' on '{}/{}' as \
                the protection is not in the team repo",
                    self.pattern, org, repo_name
                );
                sync.delete_branch_protection(org, repo_name, id)?;
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

fn log_branch_protection(
    current: &api::BranchProtection,
    new: Option<&api::BranchProtection>,
    mut result: impl Write,
) -> std::fmt::Result {
    macro_rules! log {
        ($str:literal, $field1:ident) => {
            let old = &current.$field1;
            let new = new.map(|n| &n.$field1);
            log!($str, old, new);
        };
        ($str:literal, $old:expr, $new:expr) => {
            if Some($old) != $new {
                if let Some(n) = $new.as_ref() {
                    writeln!(result, "        {}: {:?} => {:?}", $str, $old, n)?;
                } else {
                    writeln!(result, "        {}: {:?}", $str, $old)?;
                };
            }
        };
    }

    log!("Dismiss Stale Reviews", dismisses_stale_reviews);
    log!(
        "Required Approving Review Count",
        required_approving_review_count
    );
    log!("Required Checks", required_status_check_contexts);
    log!("Allowances", push_allowances);
    Ok(())
}

#[derive(Debug)]
enum BranchProtectionDiffOperation {
    Create(api::BranchProtection),
    Update(String, api::BranchProtection, api::BranchProtection),
    Delete(String),
}

#[derive(Debug)]
enum AppInstallationDiff {
    Add(AppInstallation),
    Remove(AppInstallation),
}

impl AppInstallationDiff {
    fn apply(&self, sync: &GitHubWrite, repo_id: u64) -> anyhow::Result<()> {
        match self {
            AppInstallationDiff::Add(app) => {
                sync.add_repo_to_app_installation(app.installation_id, repo_id)?;
            }
            AppInstallationDiff::Remove(app) => {
                sync.remove_repo_from_app_installation(app.installation_id, repo_id)?;
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

#[derive(Debug)]
enum TeamDiff {
    Create(CreateTeamDiff),
    Edit(EditTeamDiff),
    Delete(DeleteTeamDiff),
}

impl TeamDiff {
    fn apply(self, sync: &GitHubWrite) -> anyhow::Result<()> {
        match self {
            TeamDiff::Create(c) => c.apply(sync)?,
            TeamDiff::Edit(e) => e.apply(sync)?,
            TeamDiff::Delete(d) => d.apply(sync)?,
        }

        Ok(())
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
    fn apply(self, sync: &GitHubWrite) -> anyhow::Result<()> {
        sync.create_team(&self.org, &self.name, &self.description, self.privacy)?;
        for (member_name, role) in self.members {
            MemberDiff::Create(role).apply(&self.org, &self.name, &member_name, sync)?;
        }

        Ok(())
    }
}

impl std::fmt::Display for CreateTeamDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "➕ Creating team:")?;
        writeln!(f, "  Org: {}", self.org)?;
        writeln!(f, "  Name: {}", self.name)?;
        writeln!(f, "  Description: {}", self.description)?;
        writeln!(
            f,
            "  Privacy: {}",
            match self.privacy {
                TeamPrivacy::Secret => "secret",
                TeamPrivacy::Closed => "closed",
            }
        )?;
        writeln!(f, "  Members:")?;
        for (name, role) in &self.members {
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
    fn apply(self, sync: &GitHubWrite) -> anyhow::Result<()> {
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
            )?;
        }

        for (member_name, member_diff) in self.member_diffs {
            member_diff.apply(&self.org, &self.name, &member_name, sync)?;
        }

        Ok(())
    }

    fn noop(&self) -> bool {
        self.name_diff.is_none()
            && self.description_diff.is_none()
            && self.privacy_diff.is_none()
            && self.member_diffs.iter().all(|(_, d)| d.is_noop())
    }
}

impl std::fmt::Display for EditTeamDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.noop() {
            return Ok(());
        }
        writeln!(f, "📝 Editing team '{}/{}':", self.org, self.name)?;
        if let Some(n) = &self.name_diff {
            writeln!(f, "  New name: {n}")?;
        }
        if let Some((old, new)) = &self.description_diff {
            writeln!(f, "  New description: '{old}' => '{new}'")?;
        }
        if let Some((old, new)) = &self.privacy_diff {
            let display = |privacy: &TeamPrivacy| match privacy {
                TeamPrivacy::Secret => "secret",
                TeamPrivacy::Closed => "closed",
            };
            writeln!(f, "  New privacy: '{}' => '{}'", display(old), display(new))?;
        }
        for (member, diff) in &self.member_diffs {
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
    fn apply(self, org: &str, team: &str, member: &str, sync: &GitHubWrite) -> anyhow::Result<()> {
        match self {
            MemberDiff::Create(role) | MemberDiff::ChangeRole((_, role)) => {
                sync.set_team_membership(org, team, member, role)?;
            }
            MemberDiff::Delete => sync.remove_team_membership(org, team, member)?,
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
    fn apply(self, sync: &GitHubWrite) -> anyhow::Result<()> {
        sync.delete_team(&self.org, &self.slug)?;
        Ok(())
    }
}

impl std::fmt::Display for DeleteTeamDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "❌ Deleting team '{}/{}'", self.org, self.name)?;
        Ok(())
    }
}
