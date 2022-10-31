mod api;

use self::api::{GitHub, TeamPrivacy, TeamRole};
use crate::{github::api::RepoPermission, TeamApi};
use anyhow::Error;
use log::{debug, info};
use rust_team_data::v1::Bot;
use std::collections::{HashMap, HashSet};

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
    pub(crate) fn new(token: String, team_api: &TeamApi, dry_run: bool) -> Result<Self, Error> {
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

        Ok(Diff { team_diffs })
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
                    // Remove the current team from the collection of unseen GitHub kteams
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

    pub(crate) fn synchronize_all(&self) -> Result<(), Error> {
        self.syncronize_teams()?;

        for repo in &self.repos {
            self.synchronize_repo(repo)?;
        }

        Ok(())
    }

    fn syncronize_teams(&self) -> Result<(), Error> {
        let mut unseen_github_teams = HashMap::new();
        for team in &self.teams {
            if let Some(gh) = &team.github {
                for github_team in &gh.teams {
                    // Get existing teams we haven't seen yet
                    let unseen_github_teams = match unseen_github_teams.get_mut(&github_team.org) {
                        Some(ts) => ts,
                        None => unseen_github_teams
                            .entry(github_team.org.clone())
                            .or_insert(self.github.org_teams(&github_team.org)?),
                    };
                    // Remove the current team from the collection of unseen GitHub teams
                    unseen_github_teams.remove(&github_team.name);

                    self.synchronize_team(github_team)?;
                }
            }
        }

        for (org, remaining_github_team) in unseen_github_teams
            .iter()
            .filter(|(org, _)| *org == "rust-lang") // Only delete unmanaged teams in `rust-lang` for now
            .flat_map(|(org, teams)| teams.iter().map(move |t| (org, t)))
            // Don't delete the special bot teams
            .filter(|(_, team)| !BOTS_TEAMS.contains(&team.as_str()))
        {
            info!("Deleting team {} from {}", remaining_github_team, org);
            self.github.delete_team(org, remaining_github_team)?;
        }

        Ok(())
    }

    fn synchronize_team(&self, github_team: &rust_team_data::v1::GitHubTeam) -> Result<(), Error> {
        let slug = format!("{}/{}", github_team.org, github_team.name);
        debug!("synchronizing team {}", slug);

        // Ensure the team exists and is consistent
        let team = match self.github.team(&github_team.org, &github_team.name)? {
            Some(team) => team,
            None => self.github.create_team(
                &github_team.org,
                &github_team.name,
                DEFAULT_DESCRIPTION,
                DEFAULT_PRIVACY,
            )?,
        };
        if team.name != github_team.name
            || team.description != DEFAULT_DESCRIPTION
            || team.privacy != DEFAULT_PRIVACY
        {
            self.github.edit_team(
                &github_team.org,
                &team.name,
                Some(&github_team.name),
                Some(DEFAULT_DESCRIPTION),
                Some(DEFAULT_PRIVACY),
            )?;
        }

        let mut current_members = self.github.team_memberships(&team)?;

        // Ensure all expected members are in the team
        for member in &github_team.members {
            let expected_role = self.expected_role(&github_team.org, *member);
            let username = &self.usernames_cache[member];
            if let Some(member) = current_members.remove(member) {
                if member.role != expected_role {
                    info!(
                        "{}: user {} has the role {} instead of {}, changing them...",
                        slug, username, member.role, expected_role
                    );
                    self.github.set_membership(
                        &github_team.org,
                        &github_team.name,
                        username,
                        expected_role,
                    )?;
                } else {
                    debug!("{}: user {} is in the correct state", slug, username);
                }
            } else {
                info!(
                    "{}: user {} is missing from team, adding them...",
                    slug, username
                );
                // If the user is not a member of the org and they *don't* have a pending
                // invitation this will send the invite email and add the membership in a "pending"
                // state.
                //
                // If the user didn't accept the invitation yet the next time the tool runs, the
                // method will be called again. Thankfully though in that case GitHub doesn't send
                // yet another invitation email to the user, but treats the API call as a noop, so
                // it's safe to do it multiple times.
                self.github.set_membership(
                    &github_team.org,
                    &github_team.name,
                    username,
                    expected_role,
                )?;
            }
        }

        // The previous cycle removed expected members from current_members, so it only contains
        // members to delete now.
        for member in current_members.values() {
            info!(
                "{}: user {} is not in the team anymore, removing them...",
                slug, member.username
            );
            self.github
                .remove_membership(&github_team.org, &github_team.name, &member.username)?;
        }

        Ok(())
    }

    fn synchronize_repo(&self, expected_repo: &rust_team_data::v1::Repo) -> Result<(), Error> {
        debug!(
            "synchronizing repo {}/{}",
            expected_repo.org, expected_repo.name
        );

        // Ensure the repo exists or create it.
        let (actual_repo, just_created) =
            match self.github.repo(&expected_repo.org, &expected_repo.name)? {
                Some(r) => {
                    debug!("repo already exists...");
                    (r, false)
                }
                None => {
                    let repo = self.github.create_repo(
                        &expected_repo.org,
                        &expected_repo.name,
                        &expected_repo.description,
                    )?;
                    (repo, true)
                }
            };

        // Ensure the repo is consistent between its expected state and current state
        if !just_created {
            if actual_repo.description != expected_repo.description {
                self.github
                    .edit_repo(&actual_repo, &expected_repo.description)?;
            } else {
                debug!("repo is in synced state");
            }
        }

        use rust_team_data::v1;
        let permission = |p: &v1::RepoPermission| match *p {
            v1::RepoPermission::Write => RepoPermission::Write,
            v1::RepoPermission::Admin => RepoPermission::Admin,
            v1::RepoPermission::Maintain => RepoPermission::Maintain,
            v1::RepoPermission::Triage => RepoPermission::Triage,
        };
        let mut actual_teams = self.github.teams(&expected_repo.org, &expected_repo.name)?;
        let mut actual_collaborators = self
            .github
            .collaborators(&expected_repo.org, &expected_repo.name)?;

        // Sync team and bot permissions
        for expected_team in &expected_repo.teams {
            let permission = permission(&expected_team.permission);
            actual_teams.remove(&expected_team.name);
            self.github.update_team_repo_permissions(
                &expected_repo.org,
                &expected_repo.name,
                &expected_team.name,
                &permission,
            )?;
        }

        for bot in &expected_repo.bots {
            let bot_name = match bot {
                Bot::Bors => "bors",
                Bot::Highfive => "rust-highfive",
                Bot::RustTimer => "rust-timer",
                Bot::Rustbot => "rustbot",
            };
            actual_teams.remove(bot_name);
            actual_collaborators.remove(bot_name);
            self.github.update_user_repo_permissions(
                &expected_repo.org,
                &expected_repo.name,
                bot_name,
                &RepoPermission::Write,
            )?;
        }

        for member in &expected_repo.members {
            actual_collaborators.remove(&member.name);
            self.github.update_user_repo_permissions(
                &expected_repo.org,
                &expected_repo.name,
                &member.name,
                &permission(&member.permission),
            )?;
        }

        // `actual_teams` now contains the teams that were not expected
        // but are still on GitHub. We now remove them.
        for team in &actual_teams {
            self.github
                .remove_team_from_repo(&expected_repo.org, &expected_repo.name, team)?;
        }
        // `actual_collaborators` now contains the collaborators that were not expected
        // but are still on GitHub. We now remove them.
        for collaborator in &actual_collaborators {
            self.github.remove_collaborator_from_repo(
                &expected_repo.org,
                &expected_repo.name,
                collaborator,
            )?;
        }

        let mut main_branch_commit = None;
        let mut actual_branch_protections = self.github.protected_branches(&actual_repo)?;

        for branch in &expected_repo.branches {
            actual_branch_protections.remove(&branch.name);

            // if the branch does not already exist, create it
            if self.github.branch(&actual_repo, &branch.name)?.is_none() {
                // First, we need the sha of the head of the main branch
                let main_branch_commit = match main_branch_commit.as_ref() {
                    Some(s) => s,
                    None => {
                        let head = self
                            .github
                            .branch(&actual_repo, &actual_repo.default_branch)?;
                        // cache the main branch head so we only need to get it once
                        main_branch_commit.get_or_insert(head)
                    }
                };

                // If there is a main branch commit, create the new branch
                if let Some(main_branch_commit) = main_branch_commit {
                    self.github
                        .create_branch(&actual_repo, &branch.name, main_branch_commit)?;
                }
            }

            // Update the protection of the branch
            let protection_result = self.github.update_branch_protection(
                &actual_repo,
                &branch.name,
                api::BranchProtection {
                    required_approving_review_count: if expected_repo.bots.contains(&Bot::Bors) {
                        0
                    } else {
                        1
                    },
                    dismiss_stale_reviews: branch.dismiss_stale_review,
                    required_checks: branch.ci_checks.clone(),
                    allowed_users: expected_repo
                        .bots
                        .contains(&Bot::Bors)
                        .then(|| vec!["bors".to_owned()])
                        .unwrap_or_default(),
                },
            )?;
            if !protection_result {
                debug!(
                    "Did not update branch protection for \
                    '{}' on '{}/{}' as the branch does not exist.",
                    branch.name, actual_repo.org, actual_repo.name
                );
            }
        }

        // `actual_branch_protections` now contains the branch protections that were not expected
        // but are still on GitHub. We now remove them.
        for branch_protection in actual_branch_protections {
            debug!(
                "Deleting branch protection for '{}' on '{}/{}' as \
                the protection is not in the team repo",
                branch_protection, actual_repo.org, actual_repo.name
            );
            self.github
                .delete_branch_protection(&actual_repo, &branch_protection)?;
        }
        Ok(())
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

/// The special bot teams
const BOTS_TEAMS: &[&str] = &["bors", "highfive", "rfcbot", "bots"];

/// A diff between the team repo and the state on GitHub
pub(crate) struct Diff {
    team_diffs: Vec<TeamDiff>,
}

impl Diff {
    /// Apply the diff to GitHub
    pub(crate) fn apply(self, sync: &SyncGitHub) -> anyhow::Result<()> {
        for team_diff in self.team_diffs {
            team_diff.apply(sync)?;
        }

        Ok(())
    }

    /// Print out the diff to the logs
    pub(crate) fn log(&self) {
        for team_diff in &self.team_diffs {
            team_diff.log()
        }
    }
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

    fn log(&self) {
        match self {
            TeamDiff::Create(c) => c.log(),
            TeamDiff::Edit(e) => e.log(),
            TeamDiff::Delete(d) => d.log(),
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

    fn log(&self) {
        info!("➕ Creating team:");
        info!("  Org: {}", self.org);
        info!("  Name: {}", self.name);
        info!("  Description: {}", self.description);
        info!(
            "  Privacy: {}",
            match self.privacy {
                TeamPrivacy::Secret => "secret",
                TeamPrivacy::Closed => "closed",
            }
        );
        info!("  Members:");
        for (name, role) in &self.members {
            info!("    {}: {}", name, role);
        }
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
        sync.github.edit_team(
            &self.org,
            &self.name,
            self.name_diff.as_deref(),
            self.description_diff.as_ref().map(|(_, d)| d.as_str()),
            self.privacy_diff.map(|(_, p)| p),
        )?;

        for (member_name, member_diff) in self.member_diffs {
            member_diff.apply(&self.org, &self.name, &member_name, sync)?;
        }

        Ok(())
    }

    fn log(&self) {
        if self.noop() {
            debug!("✅ Team '{}' stays the same...", self.name);
            return;
        }
        info!("📝 Editing team '{}':", self.name);
        if let Some(n) = &self.name_diff {
            info!("  New name: {}", n);
        }
        if let Some((old, new)) = &self.description_diff {
            info!("  New description: '{}' => '{}'", old, new);
        }
        if let Some((old, new)) = &self.privacy_diff {
            let display = |privacy: &TeamPrivacy| match privacy {
                TeamPrivacy::Secret => "secret",
                TeamPrivacy::Closed => "closed",
            };
            info!("  New privacy: '{}' => '{}'", display(old), display(new));
        }
        for (member, diff) in &self.member_diffs {
            match diff {
                MemberDiff::Create(r) => info!("  Adding member '{member}' with {r} role"),
                MemberDiff::ChangeRole((o, n)) => {
                    info!("  Changing '{member}' role from {o} to {n}")
                }
                MemberDiff::Delete => info!("  Deleting member '{member}'"),
                MemberDiff::Noop => debug!("  Member '{member}' stays the same"),
            }
        }
    }

    fn noop(&self) -> bool {
        self.name_diff.is_none()
            && self.description_diff.is_none()
            && self.privacy_diff.is_none()
            && self.member_diffs.iter().all(|(_, d)| d.is_noop())
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
                sync.github.set_membership(org, team, member, role)?;
            }
            MemberDiff::Delete => sync.github.remove_membership(org, team, member)?,
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

    fn log(&self) {
        info!("❌ Deleting team:");
        info!("  Org: {}", self.org);
        info!("  Name: {}", self.name);
    }
}
