use async_trait::async_trait;
use indexmap::IndexMap;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::vec;

use derive_builder::Builder;
use rust_team_data::v1::{
    self, Bot, BranchProtectionMode, Environment, GitHubTeam, MergeBot, MergeQueueMethod, Person,
    ProtectionTarget, RepoPermission, TeamGitHub, TeamKind,
};

use crate::schema;
use crate::sync::Config;
use crate::sync::github::api::{
    BranchPolicy, BranchProtection, GithubRead, OrgAppInstallation, Repo, RepoAppInstallation,
    RepoTeam, RepoUser, Ruleset, Team, TeamMember, TeamPrivacy, TeamRole,
};
use crate::sync::github::{
    OrgMembershipDiff, RepoDiff, SyncGitHub, TeamDiff, api, construct_ruleset, convert_permission,
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
    config: Config,
}

impl DataModel {
    pub fn create_user(&mut self, name: &str) -> UserId {
        let github_id = self.people.len() as UserId;
        self.people.push(Person {
            name: name.to_string(),
            email: Some(format!("{name}@rust.com")),
            github_id,
            github_sponsors: false,
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

    pub fn add_allowed_org_member(&mut self, member: &str) {
        self.config.special_org_members.insert(member.to_string());
    }

    pub fn add_independent_github_org(&mut self, org: &str) {
        self.config.independent_github_orgs.insert(org.to_string());
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

                org.members.extend(
                    gh_team
                        .members
                        .iter()
                        .copied()
                        .map(|user_id| (user_id, users[&user_id].clone())),
                );
            }
        }

        for repo in &self.repos {
            let org = orgs.entry(repo.org.clone()).or_default();
            org.repos.insert(
                repo.name.clone(),
                Repo {
                    node_id: org.repos.len().to_string(),
                    repo_id: org.repos.len() as u64,
                    name: repo.name.clone(),
                    org: repo.org.clone(),
                    description: repo.description.clone(),
                    homepage: repo.homepage.clone(),
                    archived: false,
                    private: false,
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

            // Branch protections are deprecated so we don't test them anymore
            org.branch_protections.insert(repo.name.clone(), Vec::new());

            let protections = repo
                .branch_protections
                .iter()
                .enumerate()
                .map(|(idx, protection)| Ruleset {
                    id: Some(idx as i64),
                    ..construct_ruleset(protection, vec![])
                })
                .collect();
            org.rulesets.insert(repo.name.clone(), protections);

            let environments: HashMap<String, Environment> =
                repo.environments.clone().into_iter().collect();
            org.repo_environments
                .insert(repo.name.clone(), environments);
        }

        if orgs.is_empty() {
            orgs.insert(DEFAULT_ORG.to_string(), GithubOrg::default());
        }

        GithubMock { users, orgs }
    }

    pub async fn diff_org_membership(&self, github: GithubMock) -> Vec<OrgMembershipDiff> {
        self.create_sync(github)
            .await
            .diff_org_memberships()
            .await
            .expect("Cannot diff org membership")
    }

    pub async fn diff_teams(&self, github: GithubMock) -> Vec<TeamDiff> {
        self.create_sync(github)
            .await
            .diff_teams()
            .await
            .expect("Cannot diff teams")
    }

    pub async fn diff_repos(&self, github: GithubMock) -> Vec<RepoDiff> {
        self.create_sync(github)
            .await
            .diff_repos()
            .await
            .expect("Cannot diff repos")
    }

    async fn create_sync(&self, github: GithubMock) -> SyncGitHub {
        let teams = self.teams.iter().cloned().map(|t| t.into()).collect();
        let repos = self.repos.iter().cloned().map(|r| r.into()).collect();
        let config = self.config.clone();

        SyncGitHub::new(Box::new(github), teams, repos, config)
            .await
            .expect("Cannot create SyncGitHub")
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
    #[builder(default)]
    pub environments: IndexMap<String, v1::Environment>,
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
            environments,
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
            crates: vec![],
            environments,
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

    pub fn environment(mut self, name: &str) -> Self {
        let mut environments = self.environments.clone().unwrap_or_default();
        environments.insert(
            name.to_string(),
            v1::Environment {
                branches: Vec::new(),
                tags: Vec::new(),
            },
        );
        self.environments = Some(environments);
        self
    }

    pub fn environment_with_branches(mut self, name: &str, branches: &[&str]) -> Self {
        let mut environments = self.environments.clone().unwrap_or_default();
        environments.insert(
            name.to_string(),
            v1::Environment {
                branches: branches.iter().map(|s| s.to_string()).collect(),
                tags: Vec::new(),
            },
        );
        self.environments = Some(environments);
        self
    }
}

#[derive(Clone)]
pub struct BranchProtectionBuilder {
    pub name: Option<String>,
    pub pattern: String,
    pub target: ProtectionTarget,
    pub dismiss_stale_review: bool,
    pub require_conversation_resolution: bool,
    pub require_linear_history: bool,
    pub mode: BranchProtectionMode,
    pub allowed_merge_teams: Vec<String>,
    pub allowed_merge_apps: Vec<MergeBot>,
    pub require_up_to_date_branches: bool,
    pub merge_queue: bool,
    pub merge_queue_method: MergeQueueMethod,
    pub merge_queue_max_entries_to_build: u32,
    pub merge_queue_min_entries_to_merge_wait_minutes: u32,
    pub merge_queue_max_entries_to_merge: u32,
    pub merge_queue_check_response_timeout_minutes: u32,
    pub prevent_creation: bool,
    pub prevent_update: bool,
    pub prevent_deletion: bool,
    pub prevent_force_push: bool,
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
            name,
            pattern,
            target,
            dismiss_stale_review,
            require_conversation_resolution,
            require_linear_history,
            mode,
            allowed_merge_teams,
            allowed_merge_apps,
            require_up_to_date_branches,
            merge_queue,
            merge_queue_method,
            merge_queue_max_entries_to_build,
            merge_queue_min_entries_to_merge_wait_minutes,
            merge_queue_max_entries_to_merge,
            merge_queue_check_response_timeout_minutes,
            prevent_creation,
            prevent_update,
            prevent_deletion,
            prevent_force_push,
        } = self;
        v1::BranchProtection {
            name,
            pattern,
            target,
            dismiss_stale_review,
            require_conversation_resolution,
            require_linear_history,
            mode,
            allowed_merge_teams,
            allowed_merge_apps,
            require_up_to_date_branches,
            merge_queue,
            merge_queue_method,
            merge_queue_max_entries_to_build,
            merge_queue_min_entries_to_merge_wait_minutes,
            merge_queue_max_entries_to_merge,
            merge_queue_check_response_timeout_minutes,
            prevent_creation,
            prevent_update,
            prevent_deletion,
            prevent_force_push,
            // Maintain compatibility with triagebot
            merge_bots: vec![],
        }
    }

    fn create(pattern: &str, mode: BranchProtectionMode) -> Self {
        let merge_queue_defaults = schema::MergeQueue::default();

        Self {
            name: None,
            pattern: pattern.to_string(),
            target: ProtectionTarget::default(),
            mode,
            dismiss_stale_review: false,
            require_conversation_resolution: false,
            require_linear_history: false,
            allowed_merge_teams: vec![],
            allowed_merge_apps: vec![],
            require_up_to_date_branches: false,
            merge_queue: merge_queue_defaults.enabled,
            merge_queue_method: merge_queue_defaults.method.into(),
            merge_queue_max_entries_to_build: merge_queue_defaults.max_entries_to_build,
            merge_queue_min_entries_to_merge_wait_minutes: merge_queue_defaults
                .min_entries_to_merge_wait_minutes,
            merge_queue_max_entries_to_merge: merge_queue_defaults.max_entries_to_merge,
            merge_queue_check_response_timeout_minutes: merge_queue_defaults
                .check_response_timeout_minutes,
            prevent_creation: schema::branch_protection_default_prevent_creation(),
            prevent_update: schema::branch_protection_default_prevent_update(),
            prevent_deletion: schema::branch_protection_default_prevent_deletion(),
            prevent_force_push: schema::branch_protection_default_prevent_force_push(),
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
    pub fn add_member(&mut self, org: &str, username: &str) {
        let user_id = self.users.len() as UserId;
        self.users.insert(user_id, username.to_string());
        self.orgs
            .entry(org.to_string())
            .or_default()
            .members
            .insert((user_id, username.to_string()));
    }

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

#[async_trait]
impl GithubRead for GithubMock {
    fn uses_pat(&self) -> bool {
        true
    }

    async fn usernames(&self, ids: &[UserId]) -> anyhow::Result<HashMap<UserId, String>> {
        Ok(self
            .users
            .iter()
            .filter(|(k, _)| ids.contains(k))
            .map(|(k, v)| (*k, v.clone()))
            .collect())
    }

    async fn org_owners(&self, org: &str) -> anyhow::Result<HashSet<UserId>> {
        Ok(self.get_org(org).owners.iter().copied().collect())
    }

    async fn org_members(&self, org: &str) -> anyhow::Result<HashMap<u64, String>> {
        Ok(self.get_org(org).members.iter().cloned().collect())
    }

    async fn org_app_installations(&self, _org: &str) -> anyhow::Result<Vec<OrgAppInstallation>> {
        Ok(vec![])
    }

    async fn app_installation_repos(
        &self,
        _installation_id: u64,
        _org: &str,
    ) -> anyhow::Result<Vec<RepoAppInstallation>> {
        Ok(vec![])
    }

    async fn org_teams(&self, org: &str) -> anyhow::Result<Vec<(String, String)>> {
        Ok(self
            .get_org(org)
            .teams
            .iter()
            .map(|team| (team.name.clone(), team.slug.clone()))
            .collect())
    }

    async fn team(&self, org: &str, team: &str) -> anyhow::Result<Option<Team>> {
        Ok(self
            .get_org(org)
            .teams
            .iter()
            .find(|t| t.name == team)
            .cloned())
    }

    async fn team_memberships(
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

    async fn team_membership_invitations(
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

    async fn repo(&self, org: &str, repo: &str) -> anyhow::Result<Option<Repo>> {
        Ok(self
            .orgs
            .get(org)
            .and_then(|org| org.repos.get(repo).cloned()))
    }

    async fn repo_teams(&self, org: &str, repo: &str) -> anyhow::Result<Vec<RepoTeam>> {
        Ok(self
            .get_org(org)
            .repo_members
            .get(repo)
            .cloned()
            .map(|members| members.teams)
            .unwrap_or_default())
    }

    async fn repo_collaborators(&self, org: &str, repo: &str) -> anyhow::Result<Vec<RepoUser>> {
        Ok(self
            .get_org(org)
            .repo_members
            .get(repo)
            .cloned()
            .map(|members| members.members)
            .unwrap_or_default())
    }

    async fn branch_protections(
        &self,
        org: &str,
        repo: &str,
    ) -> anyhow::Result<BTreeMap<String, (String, BranchProtection)>> {
        let Some(protections) = self.get_org(org).branch_protections.get(repo) else {
            return Ok(Default::default());
        };
        let mut result = BTreeMap::default();
        for (id, protection) in protections {
            result.insert(protection.pattern.clone(), (id.clone(), protection.clone()));
        }

        Ok(result)
    }

    async fn repo_rulesets(&self, org: &str, repo: &str) -> anyhow::Result<Vec<Ruleset>> {
        Ok(self
            .get_org(org)
            .rulesets
            .get(repo)
            .cloned()
            .unwrap_or_default())
    }

    async fn repo_environments(
        &self,
        org: &str,
        repo: &str,
    ) -> anyhow::Result<HashMap<String, Environment>> {
        Ok(self
            .get_org(org)
            .repo_environments
            .get(repo)
            .cloned()
            .unwrap_or_default())
    }

    async fn environment_branch_policies(
        &self,
        _org: &str,
        _repo: &str,
        _environment: &str,
    ) -> anyhow::Result<Vec<BranchPolicy>> {
        unimplemented!(
            "call the function repo_environments instead, and read branch policies from there"
        )
    }
}

#[derive(Default)]
struct GithubOrg {
    members: BTreeSet<(UserId, String)>,
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
    // Repo name -> Vec<ruleset>
    rulesets: HashMap<String, Vec<Ruleset>>,
    // Repo name -> HashMap<env name, environment>
    repo_environments: HashMap<String, HashMap<String, Environment>>,
}

#[derive(Clone)]
pub struct RepoMembers {
    teams: Vec<RepoTeam>,
    members: Vec<RepoUser>,
}
