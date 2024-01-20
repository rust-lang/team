use std::collections::{HashMap, HashSet};

use derive_builder::Builder;
use rust_team_data::v1::{GitHubTeam, Person, TeamGitHub, TeamKind};

use crate::github::api::{
    BranchProtection, GithubRead, Repo, RepoTeam, RepoUser, Team, TeamMember, TeamPrivacy, TeamRole,
};
use crate::github::{api, SyncGitHub, TeamDiff};

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

        GithubMock {
            users,
            owners: Default::default(),
            teams,
            team_memberships,
            team_invitations: Default::default(),
        }
    }

    pub fn diff_teams(&self, github: GithubMock) -> Vec<TeamDiff> {
        let teams = self.teams.iter().map(|r| r.to_data()).collect();
        let repos = vec![];

        let read = Box::new(github);
        let sync = SyncGitHub::new(read, teams, repos).expect("Cannot create SyncGitHub");
        sync.diff_teams().expect("Cannot diff teams")
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

    fn to_data(&self) -> rust_team_data::v1::Team {
        let TeamData {
            name,
            kind,
            gh_teams,
        } = self.clone();
        rust_team_data::v1::Team {
            name: name.clone(),
            kind,
            subteam_of: None,
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

    fn team_memberships(&self, team: &Team) -> anyhow::Result<HashMap<UserId, TeamMember>> {
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

    fn repo(&self, _org: &str, _repo: &str) -> anyhow::Result<Option<Repo>> {
        todo!()
    }

    fn repo_teams(&self, _org: &str, _repo: &str) -> anyhow::Result<Vec<RepoTeam>> {
        todo!()
    }

    fn repo_collaborators(&self, _org: &str, _repo: &str) -> anyhow::Result<Vec<RepoUser>> {
        todo!()
    }

    fn branch_protections(
        &self,
        _org: &str,
        _repo: &str,
    ) -> anyhow::Result<HashMap<String, (String, BranchProtection)>> {
        todo!()
    }
}
