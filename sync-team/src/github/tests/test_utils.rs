use std::collections::{BTreeSet, HashMap, HashSet};

use derive_builder::Builder;
use rust_team_data::v1;
use rust_team_data::v1::{
    Bot, BranchProtectionMode, GitHubTeam, MergeBot, Person, RepoPermission, TeamGitHub, TeamKind,
};

use crate::github::api::{
    BranchProtection, GithubRead, Repo, RepoTeam, RepoUser, Team, TeamMember, TeamPrivacy, TeamRole,
};
use crate::github::{
    RepoDiff, SyncGitHub, TeamDiff, api, construct_branch_protection, convert_permission,
};

pub const DEFAULT_ORG: &str = "rust-lang";

type UserId = u64;

/// Represents the contents of rust_team_data state.
/// In tests, you should fill the model with repos, teams, people etc.,
/// and then call `gh_model` to construct a corresponding GitHubModel.
/// After that, you can modify the data model further, then generate a diff
/// and assert that it has the expected value.
#[derive(Default, Clone)]
pub struct DataModel {
    people: Vec<Person>,
    teams: Vec<TeamData>,
    repos: Vec<RepoData>,
}

impl DataModel {
    pub fn create_user(&mut self, name: &str) -> UserId {
        let github_id = self.people.len() as UserId;
        self.people.push(Person {
            name: name.to_string(),
            email: Some(format!("{name}@rust.com")),
            github_id,
        });
        github_id
    }

    pub fn create_team(&mut self, team: TeamDataBuilder) {
        let team = team.build().expect("Cannot build team");
        self.teams.push(team);
    }

    pub fn get_team(&mut self, name: &str) -> &mut TeamData {
        self.teams
            .iter_mut()
            .find(|t| t.name == name)
            .expect("Team not found")
    }

    pub fn create_repo(&mut self, repo: RepoDataBuilder) {
        let repo = repo.build().expect("Cannot build repo");
        self.repos.push(repo);
    }

    pub fn get_repo(&mut self, name: &str) -> &mut RepoData {
        self.repos
            .iter_mut()
            .find(|r| r.name == name)
            .expect("Repo not found")
    }

    /// Creates a GitHub model from the current team data mock.
    /// Note that all users should have been created before calling this method, so that
    /// GitHub knows about the users' existence.
    pub fn gh_model(&self) -> GithubMock {
        let users: HashMap<UserId, String> = self
            .people
            .iter()
            .map(|user| (user.github_id, user.name.clone()))
            .collect();

        let mut orgs: HashMap<String, GithubOrg> = HashMap::default();

        for team in &self.teams {
            for gh_team in &team.gh_teams {
                let org = orgs.entry(gh_team.org.clone()).or_default();
                let res = org.team_memberships.insert(
                    gh_team.name.clone(),
                    gh_team
                        .members
                        .iter()
                        .map(|member| {
                            (
                                *member,
                                TeamMember {
                                    username: users.get(member).expect("User not found").clone(),
                                    role: TeamRole::Member,
                                },
                            )
                        })
                        .collect(),
                );
                assert!(res.is_none());

                org.teams.push(api::Team {
                    id: Some(org.teams.len() as u64),
                    name: gh_team.name.clone(),
                    description: Some("Managed by the rust-lang/team repository.".to_string()),
                    privacy: TeamPrivacy::Closed,
                    slug: gh_team.name.clone(),
                });

                org.members.extend(gh_team.members.iter().copied());
            }
        }

        for repo in &self.repos {
            let org = orgs.entry(repo.org.clone()).or_default();
            org.repos.insert(
                repo.name.clone(),
                Repo {
                    node_id: org.repos.len().to_string(),
                    name: repo.name.clone(),
                    org: repo.org.clone(),
                    description: repo.description.clone(),
                    homepage: repo.homepage.clone(),
                    archived: false,
                    allow_auto_merge: None,
                },
            );
            let teams = repo
                .teams
                .clone()
                .into_iter()
                .map(|t| api::RepoTeam {
                    name: t.name,
                    permission: match t.permission {
                        RepoPermission::Write => api::RepoPermission::Write,
                        RepoPermission::Admin => api::RepoPermission::Admin,
                        RepoPermission::Maintain => api::RepoPermission::Maintain,
                        RepoPermission::Triage => api::RepoPermission::Triage,
                    },
                })
                .collect();
            let members = repo
                .members
                .clone()
                .into_iter()
                .map(|m| api::RepoUser {
                    name: m.name,
                    permission: convert_permission(&m.permission),
                })
                .collect();
            org.repo_members
                .insert(repo.name.clone(), RepoMembers { teams, members });

            let repo_v1: v1::Repo = repo.clone().into();
            let mut protections = vec![];
            for protection in &repo.branch_protections {
                protections.push((
                    format!("{}", protections.len()),
                    construct_branch_protection(&repo_v1, protection),
                ));
            }
            org.branch_protections
                .insert(repo.name.clone(), protections);
        }

        if orgs.is_empty() {
            orgs.insert(DEFAULT_ORG.to_string(), GithubOrg::default());
        }

        GithubMock { users, orgs }
    }

    pub fn diff_teams(&self, github: GithubMock) -> Vec<TeamDiff> {
        self.create_sync(github)
            .diff_teams()
            .expect("Cannot diff teams")
    }

    pub fn diff_repos(&self, github: GithubMock) -> Vec<RepoDiff> {
        self.create_sync(github)
            .diff_repos()
            .expect("Cannot diff repos")
    }

    fn create_sync(&self, github: GithubMock) -> SyncGitHub {
        let teams = self.teams.iter().cloned().map(|t| t.into()).collect();
        let repos = self.repos.iter().cloned().map(|r| r.into()).collect();

        SyncGitHub::new(Box::new(github), teams, repos).expect("Cannot create SyncGitHub")
    }
}

#[derive(Clone, Builder)]
#[builder(pattern = "owned")]
pub struct TeamData {
    #[builder(default = "TeamKind::Team")]
    kind: TeamKind,
    name: String,
    #[builder(default)]
    gh_teams: Vec<GitHubTeam>,
}

impl TeamData {
    #[allow(clippy::new_ret_no_self)]
    pub fn new(name: &str) -> TeamDataBuilder {
        TeamDataBuilder::default().name(name.to_string())
    }

    pub fn add_gh_member(&mut self, team: &str, member: UserId) {
        self.github_team(team).members.push(member);
    }

    pub fn remove_gh_member(&mut self, team: &str, user: UserId) {
        self.github_team(team).members.retain(|u| *u != user);
    }

    pub fn remove_gh_team(&mut self, name: &str) {
        self.gh_teams.retain(|t| t.name != name);
    }

    fn github_team(&mut self, name: &str) -> &mut GitHubTeam {
        self.gh_teams
            .iter_mut()
            .find(|t| t.name == name)
            .expect("GitHub team not found")
    }
}

impl From<TeamData> for v1::Team {
    fn from(value: TeamData) -> Self {
        let TeamData {
            name,
            kind,
            gh_teams,
        } = value;
        v1::Team {
            name: name.clone(),
            kind,
            subteam_of: None,
            top_level: None,
            members: vec![],
            alumni: vec![],
            github: (!gh_teams.is_empty()).then_some(TeamGitHub { teams: gh_teams }),
            website_data: None,
            roles: vec![],
            discord: vec![],
        }
    }
}

impl TeamDataBuilder {
    pub fn gh_team(mut self, org: &str, name: &str, members: &[UserId]) -> Self {
        let mut gh_teams = self.gh_teams.unwrap_or_default();
        gh_teams.push(GitHubTeam {
            org: org.to_string(),
            name: name.to_string(),
            members: members.to_vec(),
        });
        self.gh_teams = Some(gh_teams);
        self
    }
}

#[derive(Clone, Builder)]
#[builder(pattern = "owned")]
pub struct RepoData {
    name: String,
    #[builder(default = DEFAULT_ORG.to_string())]
    org: String,
    #[builder(default)]
    pub description: String,
    #[builder(default)]
    pub homepage: Option<String>,
    #[builder(default)]
    bots: Vec<Bot>,
    #[builder(default)]
    pub teams: Vec<v1::RepoTeam>,
    #[builder(default)]
    pub members: Vec<v1::RepoMember>,
    #[builder(default)]
    pub archived: bool,
    #[builder(default)]
    pub allow_auto_merge: bool,
    #[builder(default)]
    pub branch_protections: Vec<v1::BranchProtection>,
}

impl RepoData {
    #[allow(clippy::new_ret_no_self)]
    pub fn new(name: &str) -> RepoDataBuilder {
        RepoDataBuilder::default().name(name.to_string())
    }

    pub fn add_member(&mut self, name: &str, permission: RepoPermission) {
        self.members.push(v1::RepoMember {
            name: name.to_string(),
            permission,
        });
    }

    pub fn add_team(&mut self, name: &str, permission: RepoPermission) {
        self.teams.push(v1::RepoTeam {
            name: name.to_string(),
            permission,
        });
    }
}

impl From<RepoData> for v1::Repo {
    fn from(value: RepoData) -> Self {
        let RepoData {
            name,
            org,
            description,
            homepage,
            bots,
            teams,
            members,
            archived,
            allow_auto_merge,
            branch_protections,
        } = value;
        Self {
            org,
            name: name.clone(),
            description,
            homepage,
            bots,
            teams: teams.clone(),
            members: members.clone(),
            branch_protections,
            archived,
            private: false,
            auto_merge_enabled: allow_auto_merge,
        }
    }
}

impl RepoDataBuilder {
    pub fn team(mut self, name: &str, permission: RepoPermission) -> Self {
        let mut teams = self.teams.clone().unwrap_or_default();
        teams.push(v1::RepoTeam {
            name: name.to_string(),
            permission,
        });
        self.teams = Some(teams);
        self
    }

    pub fn member(mut self, name: &str, permission: RepoPermission) -> Self {
        let mut members = self.members.clone().unwrap_or_default();
        members.push(v1::RepoMember {
            name: name.to_string(),
            permission,
        });
        self.members = Some(members);
        self
    }
}

#[derive(Clone)]
pub struct BranchProtectionBuilder {
    pub pattern: String,
    pub dismiss_stale_review: bool,
    pub mode: BranchProtectionMode,
    pub allowed_merge_teams: Vec<String>,
    pub merge_bots: Vec<MergeBot>,
}

impl BranchProtectionBuilder {
    pub fn pr_required(pattern: &str, ci_checks: &[&str], required_approvals: u32) -> Self {
        Self::create(
            pattern,
            BranchProtectionMode::PrRequired {
                ci_checks: ci_checks.iter().map(|s| s.to_string()).collect(),
                required_approvals,
            },
        )
    }

    pub fn pr_not_required(pattern: &str) -> Self {
        Self::create(pattern, BranchProtectionMode::PrNotRequired)
    }

    pub fn build(self) -> v1::BranchProtection {
        let BranchProtectionBuilder {
            pattern,
            dismiss_stale_review,
            mode,
            allowed_merge_teams,
            merge_bots,
        } = self;
        v1::BranchProtection {
            pattern,
            dismiss_stale_review,
            mode,
            allowed_merge_teams,
            merge_bots,
        }
    }

    fn create(pattern: &str, mode: BranchProtectionMode) -> Self {
        Self {
            pattern: pattern.to_string(),
            mode,
            dismiss_stale_review: false,
            allowed_merge_teams: vec![],
            merge_bots: vec![],
        }
    }
}

/// Represents the state of GitHub repositories, teams and users.
#[derive(Default)]
pub struct GithubMock {
    // user ID -> login
    users: HashMap<UserId, String>,
    // org name -> organization data
    orgs: HashMap<String, GithubOrg>,
}

impl GithubMock {
    pub fn add_invitation(&mut self, org: &str, repo: &str, user: &str) {
        self.get_org_mut(org)
            .team_invitations
            .entry(repo.to_string())
            .or_default()
            .push(user.to_string());
    }

    fn get_org(&self, org: &str) -> &GithubOrg {
        self.orgs
            .get(org)
            .unwrap_or_else(|| panic!("Org {org} not found"))
    }

    fn get_org_mut(&mut self, org: &str) -> &mut GithubOrg {
        self.orgs
            .get_mut(org)
            .unwrap_or_else(|| panic!("Org {org} not found"))
    }
}

impl GithubRead for GithubMock {
    fn uses_pat(&self) -> bool {
        true
    }

    fn usernames(&self, ids: &[UserId]) -> anyhow::Result<HashMap<UserId, String>> {
        Ok(self
            .users
            .iter()
            .filter(|(k, _)| ids.contains(k))
            .map(|(k, v)| (*k, v.clone()))
            .collect())
    }

    fn org_owners(&self, org: &str) -> anyhow::Result<HashSet<UserId>> {
        Ok(self.get_org(org).owners.iter().copied().collect())
    }

    fn org_teams(&self, org: &str) -> anyhow::Result<Vec<(String, String)>> {
        Ok(self
            .get_org(org)
            .teams
            .iter()
            .map(|team| (team.name.clone(), team.slug.clone()))
            .collect())
    }

    fn team(&self, org: &str, team: &str) -> anyhow::Result<Option<Team>> {
        Ok(self
            .get_org(org)
            .teams
            .iter()
            .find(|t| t.name == team)
            .cloned())
    }

    fn team_memberships(
        &self,
        team: &Team,
        org: &str,
    ) -> anyhow::Result<HashMap<UserId, TeamMember>> {
        Ok(self
            .get_org(org)
            .team_memberships
            .get(&team.name)
            .cloned()
            .unwrap_or_default())
    }

    fn team_membership_invitations(
        &self,
        org: &str,
        team: &str,
    ) -> anyhow::Result<HashSet<String>> {
        Ok(self
            .get_org(org)
            .team_invitations
            .get(team)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect())
    }

    fn repo(&self, org: &str, repo: &str) -> anyhow::Result<Option<Repo>> {
        Ok(self
            .orgs
            .get(org)
            .and_then(|org| org.repos.get(repo).cloned()))
    }

    fn repo_teams(&self, org: &str, repo: &str) -> anyhow::Result<Vec<RepoTeam>> {
        Ok(self
            .get_org(org)
            .repo_members
            .get(repo)
            .cloned()
            .map(|members| members.teams)
            .unwrap_or_default())
    }

    fn repo_collaborators(&self, org: &str, repo: &str) -> anyhow::Result<Vec<RepoUser>> {
        Ok(self
            .get_org(org)
            .repo_members
            .get(repo)
            .cloned()
            .map(|members| members.members)
            .unwrap_or_default())
    }

    fn branch_protections(
        &self,
        org: &str,
        repo: &str,
    ) -> anyhow::Result<HashMap<String, (String, BranchProtection)>> {
        let Some(protections) = self.get_org(org).branch_protections.get(repo) else {
            return Ok(Default::default());
        };
        let mut result = HashMap::default();
        for (id, protection) in protections {
            result.insert(protection.pattern.clone(), (id.clone(), protection.clone()));
        }

        Ok(result)
    }
}

#[derive(Default)]
struct GithubOrg {
    members: BTreeSet<UserId>,
    owners: BTreeSet<UserId>,
    teams: Vec<Team>,
    // Team name -> list of invited users
    team_invitations: HashMap<String, Vec<String>>,
    // Team name -> members
    team_memberships: HashMap<String, HashMap<UserId, TeamMember>>,
    // Repo name -> repo data
    repos: HashMap<String, Repo>,
    // Repo name -> (teams, members)
    repo_members: HashMap<String, RepoMembers>,
    // Repo name -> Vec<(protection ID, branch protection)>
    branch_protections: HashMap<String, Vec<(String, BranchProtection)>>,
}

#[derive(Clone)]
pub struct RepoMembers {
    teams: Vec<RepoTeam>,
    members: Vec<RepoUser>,
}
