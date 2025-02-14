use std::collections::{HashMap, HashSet};

use derive_builder::Builder;
use rust_team_data::v1;
use rust_team_data::v1::{
    Bot, BranchProtectionMode, GitHubTeam, MergeBot, Person, RepoPermission, TeamGitHub, TeamKind,
};

use crate::github::api::{
    BranchProtection, GithubRead, OrgAppInstallation, Repo, RepoAppInstallation, RepoTeam,
    RepoUser, Team, TeamMember, TeamPrivacy, TeamRole,
};
use crate::github::{
    api, construct_branch_protection, convert_permission, RepoDiff, SyncGitHub, TeamDiff,
};

const DEFAULT_ORG: &str = "rust-lang";

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

        let mut team_memberships: HashMap<String, HashMap<UserId, TeamMember>> = HashMap::default();
        let mut teams = vec![];
        for team in &self.teams {
            for gh_team in &team.gh_teams {
                let res = team_memberships.insert(
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

                teams.push(api::Team {
                    id: Some(teams.len() as u64),
                    name: gh_team.name.clone(),
                    description: Some("Managed by the rust-lang/team repository.".to_string()),
                    privacy: TeamPrivacy::Closed,
                    slug: gh_team.name.clone(),
                })
            }
        }

        let mut repos = HashMap::default();
        let mut repo_members: HashMap<String, RepoMembers> = HashMap::default();
        let mut branch_protections = HashMap::new();

        for repo in &self.repos {
            repos.insert(
                repo.name.clone(),
                Repo {
                    repo_id: repos.len() as u64,
                    node_id: repos.len().to_string(),
                    name: repo.name.clone(),
                    org: DEFAULT_ORG.to_string(),
                    description: Some(repo.description.clone()),
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
            repo_members.insert(repo.name.clone(), RepoMembers { teams, members });

            let repo_v1: v1::Repo = repo.clone().into();
            let mut protections = vec![];
            for protection in &repo.branch_protections {
                protections.push((
                    format!("{}", protections.len()),
                    construct_branch_protection(&repo_v1, protection),
                ));
            }
            branch_protections.insert(repo.name.clone(), protections);
        }

        GithubMock {
            users,
            owners: Default::default(),
            teams,
            team_memberships,
            team_invitations: Default::default(),
            repos,
            repo_members,
            branch_protections,
        }
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
    pub fn gh_team(mut self, name: &str, members: &[UserId]) -> Self {
        let mut gh_teams = self.gh_teams.unwrap_or_default();
        gh_teams.push(GitHubTeam {
            org: DEFAULT_ORG.to_string(),
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
            org: DEFAULT_ORG.to_string(),
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
    // org name -> user ID
    owners: HashMap<String, Vec<UserId>>,
    teams: Vec<Team>,
    // Team name -> members
    team_memberships: HashMap<String, HashMap<UserId, TeamMember>>,
    // Team name -> list of invited users
    team_invitations: HashMap<String, Vec<String>>,
    // Repo name -> repo data
    repos: HashMap<String, Repo>,
    // Repo name -> (teams, members)
    repo_members: HashMap<String, RepoMembers>,
    // Repo name -> Vec<(protection ID, branch protection)>
    branch_protections: HashMap<String, Vec<(String, BranchProtection)>>,
}

impl GithubMock {
    pub fn add_invitation(&mut self, repo: &str, user: &str) {
        self.team_invitations
            .entry(repo.to_string())
            .or_default()
            .push(user.to_string());
    }
}

impl GithubRead for GithubMock {
    fn usernames(&self, ids: &[UserId]) -> anyhow::Result<HashMap<UserId, String>> {
        Ok(self
            .users
            .iter()
            .filter(|(k, _)| ids.contains(k))
            .map(|(k, v)| (*k, v.clone()))
            .collect())
    }

    fn org_owners(&self, org: &str) -> anyhow::Result<HashSet<UserId>> {
        Ok(self
            .owners
            .get(org)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect())
    }

    fn org_app_installations(&self, _org: &str) -> anyhow::Result<Vec<OrgAppInstallation>> {
        Ok(vec![])
    }

    fn app_installation_repos(
        &self,
        _installation_id: u64,
        _org: &str,
    ) -> anyhow::Result<Vec<RepoAppInstallation>> {
        Ok(vec![])
    }

    fn org_teams(&self, org: &str) -> anyhow::Result<Vec<(String, String)>> {
        assert_eq!(org, DEFAULT_ORG);
        Ok(self
            .teams
            .iter()
            .map(|team| (team.name.clone(), team.slug.clone()))
            .collect())
    }

    fn team(&self, _org: &str, team: &str) -> anyhow::Result<Option<Team>> {
        Ok(self.teams.iter().find(|t| t.name == team).cloned())
    }

    fn team_memberships(
        &self,
        team: &Team,
        _org: &str,
    ) -> anyhow::Result<HashMap<UserId, TeamMember>> {
        let memberships = self
            .team_memberships
            .get(&team.name)
            .cloned()
            .unwrap_or_default();
        Ok(memberships)
    }

    fn team_membership_invitations(
        &self,
        org: &str,
        team: &str,
    ) -> anyhow::Result<HashSet<String>> {
        assert_eq!(org, DEFAULT_ORG);
        Ok(self
            .team_invitations
            .get(team)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect())
    }

    fn repo(&self, org: &str, repo: &str) -> anyhow::Result<Option<Repo>> {
        assert_eq!(org, DEFAULT_ORG);
        Ok(self.repos.get(repo).cloned())
    }

    fn repo_teams(&self, org: &str, repo: &str) -> anyhow::Result<Vec<RepoTeam>> {
        assert_eq!(org, DEFAULT_ORG);
        Ok(self
            .repo_members
            .get(repo)
            .cloned()
            .map(|members| members.teams)
            .unwrap_or_default())
    }

    fn repo_collaborators(&self, org: &str, repo: &str) -> anyhow::Result<Vec<RepoUser>> {
        // The mock currently only supports mocking one GitHub organization.
        assert_eq!(org, DEFAULT_ORG);
        Ok(self
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
        assert_eq!(org, DEFAULT_ORG);

        let Some(protections) = self.branch_protections.get(repo) else {
            return Ok(Default::default());
        };
        let mut result = HashMap::default();
        for (id, protection) in protections {
            result.insert(protection.pattern.clone(), (id.clone(), protection.clone()));
        }

        Ok(result)
    }
}

#[derive(Clone)]
pub struct RepoMembers {
    teams: Vec<RepoTeam>,
    members: Vec<RepoUser>,
}
