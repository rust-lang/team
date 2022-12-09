mod api;

use self::api::{GitHub, TeamPrivacy, TeamRole};
use crate::{github::api::RepoPermission, TeamApi};
use log::debug;
use rust_team_data::v1::Bot;
use std::collections::{HashMap, HashSet};
use std::fmt::Write;

static DEFAULT_DESCRIPTION: &str = "Managed by the rust-lang/team repository.";
static DEFAULT_PRIVACY: TeamPrivacy = TeamPrivacy::Closed;

pub(crate) struct SyncGitHub {
    github: GitHub,
    teams: Vec<rust_team_data::v1::Team>,
    repos: Vec<rust_team_data::v1::Repo>,
    usernames_cache: HashMap<usize, String>,
    org_owners: HashMap<String, HashSet<usize>>,
}

impl SyncGitHub {
    pub(crate) fn new(token: String, team_api: &TeamApi, dry_run: bool) -> anyhow::Result<Self> {
        let github = GitHub::new(token, dry_run);
        let teams = team_api.get_teams()?;
        let repos = team_api.get_repos()?;

        debug!("caching mapping between user ids and usernames");
        let users = teams
            .iter()
            .filter_map(|t| t.github.as_ref().map(|gh| &gh.teams))
            .flatten()
            .flat_map(|team| &team.members)
            .copied()
            .collect::<HashSet<_>>();
        let usernames_cache = github.usernames(&users.into_iter().collect::<Vec<_>>())?;

        debug!("caching organization owners");
        let orgs = teams
            .iter()
            .filter_map(|t| t.github.as_ref())
            .flat_map(|gh| &gh.teams)
            .map(|gh_team| &gh_team.org)
            .collect::<HashSet<_>>();
        let mut org_owners = HashMap::new();
        for org in &orgs {
            org_owners.insert((*org).to_string(), github.org_owners(org)?);
        }

        Ok(SyncGitHub {
            github,
            teams,
            repos,
            usernames_cache,
            org_owners,
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
                            let ts = self.github.org_teams(&github_team.org)?;
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
            .filter(|(org, _)| org == "rust-lang") // Only delete unmanaged teams in `rust-lang` for now
            .flat_map(|(org, remaining_github_teams)| {
                remaining_github_teams
                    .into_iter()
                    .map(move |t| (org.clone(), t))
            })
            // Don't delete the special bot teams
            .filter(|(_, remaining_github_team)| {
                !BOTS_TEAMS.contains(&remaining_github_team.as_str())
            })
            .map(|(org, remaining_github_team)| {
                TeamDiff::Delete(DeleteTeamDiff {
                    org,
                    name: remaining_github_team,
                })
            });

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
        if team.description != DEFAULT_DESCRIPTION {
            description_diff = Some((team.description.clone(), DEFAULT_DESCRIPTION.to_owned()));
        }
        let mut privacy_diff = None;
        if team.privacy != DEFAULT_PRIVACY {
            privacy_diff = Some((team.privacy, DEFAULT_PRIVACY))
        }

        let mut member_diffs = Vec::new();

        let mut current_members = self.github.team_memberships(&team)?;

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
                member_diffs.push((username.clone(), MemberDiff::Create(expected_role)));
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
                for branch in &expected_repo.branches {
                    branch_protections.push((
                        branch.name.clone(),
                        branch_protection(expected_repo, branch),
                    ));
                }

                return Ok(RepoDiff::Create(CreateRepoDiff {
                    org: expected_repo.org.clone(),
                    name: expected_repo.name.clone(),
                    description: expected_repo.description.clone(),
                    permissions,
                    branch_protections,
                }));
            }
        };

        let permission_diffs = self.diff_permissions(expected_repo)?;
        let branch_protection_diffs = self.diff_branch_protections(&actual_repo, expected_repo)?;
        let description_diff =
            (actual_repo.description.as_ref() != Some(&expected_repo.description)).then(|| {
                (
                    actual_repo.description.clone(),
                    expected_repo.description.clone(),
                )
            });
        Ok(RepoDiff::Update(UpdateRepoDiff {
            org: expected_repo.org.clone(),
            name: actual_repo.name,
            description_diff,
            permission_diffs,
            branch_protection_diffs,
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
        let mut actual_protected_branches = self.github.protected_branches(actual_repo)?;
        let mut main_branch_commit = None;

        for branch in &expected_repo.branches {
            actual_protected_branches.remove(&branch.name);
            let expected_branch_protection = branch_protection(expected_repo, branch);
            let actual_branch =
                self.github
                    .branch(&expected_repo.org, &expected_repo.name, &branch.name)?;
            let operation = if actual_branch.is_none() {
                // Branch does not yet exist, get the main branch's HEAD commit which is needed to create the branch
                let main_branch_commit = match main_branch_commit.as_ref() {
                    Some(s) => s,
                    None => {
                        // HEAD commit isn't in cache yet - fill the cache
                        let head = self
                            .github
                            .branch(
                                &actual_repo.org,
                                &actual_repo.name,
                                &actual_repo.default_branch,
                            )?
                            .unwrap(); // TODO: main branch doesn't exist yet?

                        // cache the main branch head so we only need to get it once
                        main_branch_commit.get_or_insert(head)
                    }
                };
                BranchProtectionDiffOperation::CreateWithBranch(
                    expected_branch_protection,
                    main_branch_commit.clone(),
                )
            } else {
                let actual_branch_protection = self.github.branch_protection(
                    &actual_repo.org,
                    &actual_repo.name,
                    &branch.name,
                )?;
                match actual_branch_protection {
                    Some(bp) if bp != expected_branch_protection => {
                        BranchProtectionDiffOperation::Update(bp, expected_branch_protection)
                    }
                    None => BranchProtectionDiffOperation::Create(expected_branch_protection),
                    // The branch protection doesn't need to change
                    Some(_) => continue,
                }
            };
            branch_protection_diffs.push(BranchProtectionDiff {
                name: branch.name.clone(),
                operation,
            })
        }

        // `actual_branch_protections` now contains the branch protections that were not expected
        // but are still on GitHub. We want to delete them.
        branch_protection_diffs.extend(actual_protected_branches.into_iter().map(|name| {
            BranchProtectionDiff {
                name,
                operation: BranchProtectionDiffOperation::Delete,
            }
        }));

        Ok(branch_protection_diffs)
    }

    fn expected_role(&self, org: &str, user: usize) -> TeamRole {
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
    let bots = expected_repo.bots.iter().map(|b| {
        let bot_name = bot_name(b);
        actual_teams.remove(bot_name);
        (bot_name, RepoPermission::Write)
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

fn bot_name(bot: &Bot) -> &str {
    match bot {
        Bot::Bors => "bors",
        Bot::Highfive => "rust-highfive",
        Bot::RustTimer => "rust-timer",
        Bot::Rustbot => "rustbot",
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

fn branch_protection(
    expected_repo: &rust_team_data::v1::Repo,
    branch: &rust_team_data::v1::Branch,
) -> api::BranchProtection {
    let required_approving_review_count = if expected_repo.bots.contains(&Bot::Bors) {
        0
    } else {
        1
    };
    let allowed_users = expected_repo
        .bots
        .contains(&Bot::Bors)
        .then(|| {
            vec![api::branch_protection::UserRestriction::Name(
                "bors".to_owned(),
            )]
        })
        .unwrap_or_default();
    api::BranchProtection {
        required_status_checks: api::branch_protection::RequiredStatusChecks {
            strict: false,
            checks: branch
                .ci_checks
                .clone()
                .into_iter()
                .map(|c| api::branch_protection::Check { context: c })
                .collect(),
        },
        enforce_admins: api::branch_protection::EnforceAdmins::Bool(true),
        required_pull_request_reviews: api::branch_protection::PullRequestReviews {
            dismissal_restrictions: HashMap::new(),
            dismiss_stale_reviews: branch.dismiss_stale_review,
            required_approving_review_count,
        },
        restrictions: api::branch_protection::Restrictions {
            users: allowed_users,
            teams: Vec::new(),
        },
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
    pub(crate) fn apply(self, sync: &SyncGitHub) -> anyhow::Result<()> {
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
            write!(f, "{}", team_diff)?;
        }
        writeln!(f, "💻 Repo Diffs:")?;
        for repo_diff in &self.repo_diffs {
            write!(f, "{}", repo_diff)?;
        }
        Ok(())
    }
}

enum RepoDiff {
    Create(CreateRepoDiff),
    Update(UpdateRepoDiff),
}

impl RepoDiff {
    fn apply(&self, sync: &SyncGitHub) -> anyhow::Result<()> {
        match self {
            RepoDiff::Create(c) => c.apply(sync),
            RepoDiff::Update(u) => u.apply(sync),
        }
    }
}

impl std::fmt::Display for RepoDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Create(c) => write!(f, "{}", c),
            Self::Update(u) => write!(f, "{}", u),
        }
    }
}

struct CreateRepoDiff {
    org: String,
    name: String,
    description: String,
    permissions: Vec<RepoPermissionAssignmentDiff>,
    branch_protections: Vec<(String, api::BranchProtection)>,
}

impl CreateRepoDiff {
    fn apply(&self, sync: &SyncGitHub) -> anyhow::Result<()> {
        let repo = sync
            .github
            .create_repo(&self.org, &self.name, &self.description)?;

        for permission in &self.permissions {
            permission.apply(sync, &self.org, &self.name)?;
        }

        let main_branch_commit = sync
            .github
            .branch(&self.org, &self.name, &repo.default_branch)?
            .unwrap();
        for (branch, protection) in &self.branch_protections {
            BranchProtectionDiff {
                name: branch.clone(),
                operation: BranchProtectionDiffOperation::CreateWithBranch(
                    protection.clone(),
                    main_branch_commit.clone(),
                ),
            }
            .apply(sync, &self.org, &self.name)?;
        }
        Ok(())
    }
}

impl std::fmt::Display for CreateRepoDiff {
    fn fmt(&self, mut f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "➕ Creating repo:")?;
        writeln!(f, "  Org: {}", self.org)?;
        writeln!(f, "  Name: {}", self.name)?;
        writeln!(f, "  Description: {}", self.description)?;
        writeln!(f, "  Permissions:")?;
        for diff in &self.permissions {
            write!(f, "{}", diff)?;
        }
        writeln!(f, "  Branch Protections:")?;
        for (branch_name, branch_protection) in &self.branch_protections {
            writeln!(&mut f, "    {}", branch_name)?;
            log_branch_protection(branch_protection, None, &mut f)?;
        }
        Ok(())
    }
}

struct UpdateRepoDiff {
    org: String,
    name: String,
    description_diff: Option<(Option<String>, String)>,
    permission_diffs: Vec<RepoPermissionAssignmentDiff>,
    branch_protection_diffs: Vec<BranchProtectionDiff>,
}

impl UpdateRepoDiff {
    pub(crate) fn noop(&self) -> bool {
        self.description_diff.is_none()
            && self.permission_diffs.is_empty()
            && self.branch_protection_diffs.is_empty()
    }

    fn apply(&self, sync: &SyncGitHub) -> anyhow::Result<()> {
        if let Some((_, description)) = &self.description_diff {
            sync.github.edit_repo(&self.org, &self.name, description)?;
        }
        for permission in &self.permission_diffs {
            permission.apply(sync, &self.org, &self.name)?;
        }
        for branch_protection in &self.branch_protection_diffs {
            branch_protection.apply(sync, &self.org, &self.name)?;
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
        if let Some((old, new)) = &self.description_diff {
            if let Some(old) = old {
                writeln!(f, "  New description: '{}' => '{}'", old, new)?;
            } else {
                writeln!(f, "  Set description: '{}'", new)?;
            }
        }
        if !self.permission_diffs.is_empty() {
            writeln!(f, "  Permission Changes:")?;
        }
        for permission_diff in &self.permission_diffs {
            write!(f, "{}", permission_diff)?;
        }
        writeln!(f, "  Branch Protections:")?;
        for branch_protection_diff in &self.branch_protection_diffs {
            write!(f, "{}", branch_protection_diff)?;
        }

        Ok(())
    }
}

struct RepoPermissionAssignmentDiff {
    collaborator: RepoCollaborator,
    diff: RepoPermissionDiff,
}

impl RepoPermissionAssignmentDiff {
    fn apply(&self, sync: &SyncGitHub, org: &str, repo_name: &str) -> anyhow::Result<()> {
        match &self.diff {
            RepoPermissionDiff::Create(p) | RepoPermissionDiff::Update(_, p) => {
                match &self.collaborator {
                    RepoCollaborator::Team(team_name) => sync
                        .github
                        .update_team_repo_permissions(org, repo_name, team_name, p)?,
                    RepoCollaborator::User(user_name) => sync
                        .github
                        .update_user_repo_permissions(org, repo_name, user_name, p)?,
                }
            }
            RepoPermissionDiff::Delete(_) => match &self.collaborator {
                RepoCollaborator::Team(team_name) => sync
                    .github
                    .remove_team_from_repo(org, repo_name, team_name)?,
                RepoCollaborator::User(user_name) => sync
                    .github
                    .remove_collaborator_from_repo(org, repo_name, user_name)?,
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

enum RepoPermissionDiff {
    Create(RepoPermission),
    Update(RepoPermission, RepoPermission),
    Delete(RepoPermission),
}

#[derive(Clone)]
enum RepoCollaborator {
    Team(String),
    User(String),
}

struct BranchProtectionDiff {
    name: String,
    operation: BranchProtectionDiffOperation,
}

impl BranchProtectionDiff {
    fn apply(&self, sync: &SyncGitHub, org: &str, repo_name: &str) -> anyhow::Result<()> {
        if let BranchProtectionDiffOperation::CreateWithBranch(_, main_branch_commit) =
            &self.operation
        {
            sync.github
                .create_branch(org, repo_name, &self.name, main_branch_commit)?;
        }
        match &self.operation {
            BranchProtectionDiffOperation::CreateWithBranch(bp, _)
            | BranchProtectionDiffOperation::Create(bp)
            | BranchProtectionDiffOperation::Update(_, bp) => {
                // Update the protection of the branch
                let protection_result = sync
                    .github
                    .update_branch_protection(org, repo_name, &self.name, bp)?;
                if !protection_result {
                    debug!(
                        "Did not update branch protection for \
                    '{}' on '{}/{}' as the branch does not exist.",
                        self.name, org, repo_name
                    );
                }
            }
            BranchProtectionDiffOperation::Delete => {
                debug!(
                    "Deleting branch protection for '{}' on '{}/{}' as \
                the protection is not in the team repo",
                    self.name, org, repo_name
                );
                sync.github
                    .delete_branch_protection(org, repo_name, &self.name)?;
            }
        }

        Ok(())
    }
}

impl std::fmt::Display for BranchProtectionDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "      {}", self.name)?;
        match &self.operation {
            BranchProtectionDiffOperation::CreateWithBranch(bp, _) => {
                let _ = writeln!(f, "        Creating branch");
                log_branch_protection(bp, None, f)
            }
            BranchProtectionDiffOperation::Create(bp) => log_branch_protection(bp, None, f),
            BranchProtectionDiffOperation::Update(old, new) => {
                log_branch_protection(old, Some(new), f)
            }
            BranchProtectionDiffOperation::Delete => {
                writeln!(f, "        Deleting branch protection")
            }
        }
    }
}

fn log_branch_protection(
    branch_protection: &api::BranchProtection,
    other: Option<&api::BranchProtection>,
    mut result: impl Write,
) -> std::fmt::Result {
    macro_rules! log {
        ($str:literal, $($method:ident).+) => {
            let new = other.map(|n| &n.$($method).*);
            let old = &branch_protection.$($method).*;
            if Some(old) != new {
                if let Some(n) = new.as_ref() {
                    writeln!(result, "        {}: {:?} => {:?}", $str, old, n)?;
                } else {
                    writeln!(result, "        {}: {:?}", $str, old)?;
                };
            }
        };
    }

    log!(
        "Dismiss Stale Reviews",
        required_pull_request_reviews.dismiss_stale_reviews
    );
    log!(
        "Required Approving Review Count",
        required_pull_request_reviews.required_approving_review_count
    );
    log!("Checks", required_status_checks.checks);
    log!("User Overrides", restrictions.users);
    log!("Team Overrides", restrictions.teams);
    Ok(())
}

enum BranchProtectionDiffOperation {
    CreateWithBranch(
        api::BranchProtection,
        String, /* main branch HEAD commit */
    ),
    Create(api::BranchProtection),
    Update(api::BranchProtection, api::BranchProtection),
    Delete,
}

enum TeamDiff {
    Create(CreateTeamDiff),
    Edit(EditTeamDiff),
    Delete(DeleteTeamDiff),
}

impl TeamDiff {
    fn apply(self, sync: &SyncGitHub) -> anyhow::Result<()> {
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
            TeamDiff::Create(c) => write!(f, "{}", c),
            TeamDiff::Edit(e) => write!(f, "{}", e),
            TeamDiff::Delete(d) => write!(f, "{}", d),
        }
    }
}

struct CreateTeamDiff {
    org: String,
    name: String,
    description: String,
    privacy: TeamPrivacy,
    members: Vec<(String, TeamRole)>,
}

impl CreateTeamDiff {
    fn apply(self, sync: &SyncGitHub) -> anyhow::Result<()> {
        sync.github
            .create_team(&self.org, &self.name, &self.description, self.privacy)?;
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
            writeln!(f, "    {}: {}", name, role)?;
        }
        Ok(())
    }
}

struct EditTeamDiff {
    org: String,
    name: String,
    name_diff: Option<String>,
    description_diff: Option<(String, String)>,
    privacy_diff: Option<(TeamPrivacy, TeamPrivacy)>,
    member_diffs: Vec<(String, MemberDiff)>,
}

impl EditTeamDiff {
    fn apply(self, sync: &SyncGitHub) -> anyhow::Result<()> {
        if self.name_diff.is_some()
            || self.description_diff.is_some()
            || self.privacy_diff.is_some()
        {
            sync.github.edit_team(
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
        writeln!(f, "📝 Editing team '{}':", self.name)?;
        if let Some(n) = &self.name_diff {
            writeln!(f, "  New name: {}", n)?;
        }
        if let Some((old, new)) = &self.description_diff {
            writeln!(f, "  New description: '{}' => '{}'", old, new)?;
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

enum MemberDiff {
    Create(TeamRole),
    ChangeRole((TeamRole, TeamRole)),
    Delete,
    Noop,
}

impl MemberDiff {
    fn apply(self, org: &str, team: &str, member: &str, sync: &SyncGitHub) -> anyhow::Result<()> {
        match self {
            MemberDiff::Create(role) | MemberDiff::ChangeRole((_, role)) => {
                sync.github.set_team_membership(org, team, member, role)?;
            }
            MemberDiff::Delete => sync.github.remove_team_membership(org, team, member)?,
            MemberDiff::Noop => {}
        }

        Ok(())
    }

    fn is_noop(&self) -> bool {
        matches!(self, Self::Noop)
    }
}

struct DeleteTeamDiff {
    org: String,
    name: String,
}

impl DeleteTeamDiff {
    fn apply(self, sync: &SyncGitHub) -> anyhow::Result<()> {
        sync.github.delete_team(&self.org, &self.name)?;
        Ok(())
    }
}

impl std::fmt::Display for DeleteTeamDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "❌ Deleting team:")?;
        writeln!(f, "  Org: {}", self.org)?;
        writeln!(f, "  Name: {}", self.name)?;
        Ok(())
    }
}
