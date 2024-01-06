use crate::data::Data;
use crate::schema::{Bot, Email, Permissions, RepoPermission, TeamKind, ZulipGroupMember};
use anyhow::{ensure, Context as _, Error};
use indexmap::IndexMap;
use log::info;
use rust_team_data::v1;
use std::collections::HashMap;
use std::path::Path;

pub(crate) struct Generator<'a> {
    dest: &'a Path,
    data: &'a Data,
}

impl<'a> Generator<'a> {
    pub(crate) fn new(dest: &'a Path, data: &'a Data) -> Result<Generator<'a>, Error> {
        if dest.is_dir() {
            std::fs::remove_dir_all(dest)?;
        }
        std::fs::create_dir_all(dest)?;

        Ok(Generator { dest, data })
    }

    pub(crate) fn generate(&self) -> Result<(), Error> {
        self.generate_teams()?;
        self.generate_repos()?;
        self.generate_lists()?;
        self.generate_zulip_groups()?;
        self.generate_permissions()?;
        self.generate_rfcbot()?;
        self.generate_zulip_map()?;
        self.generate_people()?;
        Ok(())
    }

    fn generate_repos(&self) -> Result<(), Error> {
        let mut repos: IndexMap<String, Vec<v1::Repo>> = IndexMap::new();
        let repo_iter = self
            .data
            .repos()
            .map(|repo| (repo, false))
            .chain(self.data.archived_repos().map(|repo| (repo, true)));

        for (r, archived) in repo_iter {
            let branch_protections: Vec<_> = r
                .branch_protections
                .iter()
                .map(|b| v1::BranchProtection {
                    pattern: b.pattern.clone(),
                    ci_checks: b.ci_checks.clone(),
                    dismiss_stale_review: b.dismiss_stale_review,
                    required_approvals: b.required_approvals.unwrap_or(1),
                    allowed_merge_teams: b.allowed_merge_teams.clone(),
                })
                .collect();
            let managed_by_bors = r.bots.contains(&Bot::Bors);
            let repo = v1::Repo {
                org: r.org.clone(),
                name: r.name.clone(),
                description: r.description.clone(),
                homepage: r.homepage.clone(),
                private: r.private_non_synced.unwrap_or(false),
                bots: r
                    .bots
                    .iter()
                    .map(|b| match b {
                        Bot::Bors => v1::Bot::Bors,
                        Bot::Highfive => v1::Bot::Highfive,
                        Bot::RustTimer => v1::Bot::RustTimer,
                        Bot::Rustbot => v1::Bot::Rustbot,
                        Bot::Rfcbot => v1::Bot::Rfcbot,
                    })
                    .collect(),
                teams: r
                    .access
                    .teams
                    .iter()
                    .map(|(name, permission)| {
                        let permission = match permission {
                            RepoPermission::Admin => v1::RepoPermission::Admin,
                            RepoPermission::Write => v1::RepoPermission::Write,
                            RepoPermission::Maintain => v1::RepoPermission::Maintain,
                            RepoPermission::Triage => v1::RepoPermission::Triage,
                        };
                        v1::RepoTeam {
                            name: name.clone(),
                            permission,
                        }
                    })
                    .collect(),
                members: r
                    .access
                    .individuals
                    .iter()
                    .map(|(name, permission)| {
                        let permission = match permission {
                            RepoPermission::Admin => v1::RepoPermission::Admin,
                            RepoPermission::Write => v1::RepoPermission::Write,
                            RepoPermission::Maintain => v1::RepoPermission::Maintain,
                            RepoPermission::Triage => v1::RepoPermission::Triage,
                        };
                        v1::RepoMember {
                            name: name.clone(),
                            permission,
                        }
                    })
                    .collect(),
                branch_protections,
                archived,
                auto_merge_enabled: !managed_by_bors,
            };

            self.add(&format!("v1/repos/{}.json", r.name), &repo)?;
            repos.entry(r.org.clone()).or_default().push(repo);
        }
        repos
            .values_mut()
            .for_each(|r| r.sort_by(|r1, r2| r1.name.cmp(&r2.name)));

        self.add("v1/repos.json", &v1::Repos { repos })?;
        Ok(())
    }

    fn generate_teams(&self) -> Result<(), Error> {
        let mut teams = IndexMap::new();

        for team in self.data.teams() {
            let mut website_roles = HashMap::new();
            for member in team.explicit_members().iter().cloned() {
                website_roles.insert(member.github, member.roles);
            }

            let leads = team.leads();
            let mut members = Vec::new();
            for github_name in &team.members(self.data)? {
                if let Some(person) = self.data.person(github_name) {
                    members.push(v1::TeamMember {
                        name: person.name().into(),
                        github: (*github_name).into(),
                        github_id: person.github_id(),
                        is_lead: leads.contains(github_name),
                        roles: website_roles.get(*github_name).cloned().unwrap_or_default(),
                    });
                }
            }
            members.sort_by_key(|member| member.github.to_lowercase());
            members.sort_by_key(|member| !member.is_lead);

            let mut alumni = Vec::new();
            for github_name in team.alumni() {
                if let Some(person) = self.data.person(github_name) {
                    alumni.push(v1::TeamMember {
                        name: person.name().into(),
                        github: github_name.to_string(),
                        github_id: person.github_id(),
                        is_lead: false,
                        roles: Vec::new(),
                    });
                }
            }
            alumni.sort_by_key(|member| member.github.to_lowercase());

            let mut github_teams = team.github_teams(self.data)?;
            github_teams.sort();

            let member_discord_ids = team.discord_ids(self.data)?;

            let team_data = v1::Team {
                name: team.name().into(),
                kind: match team.kind() {
                    TeamKind::Team => v1::TeamKind::Team,
                    TeamKind::WorkingGroup => v1::TeamKind::WorkingGroup,
                    TeamKind::ProjectGroup => v1::TeamKind::ProjectGroup,
                    TeamKind::MarkerTeam => v1::TeamKind::MarkerTeam,
                },
                subteam_of: team.subteam_of().map(|st| st.into()),
                members,
                alumni,
                github: Some(v1::TeamGitHub {
                    teams: github_teams
                        .into_iter()
                        .map(|team| v1::GitHubTeam {
                            org: team.org.to_string(),
                            name: team.name.to_string(),
                            members: team.members.into_iter().map(|(_, id)| id).collect(),
                        })
                        .collect::<Vec<_>>(),
                })
                .filter(|gh| !gh.teams.is_empty()),
                website_data: team.website_data().map(|ws| v1::TeamWebsite {
                    name: ws.name().into(),
                    description: ws.description().into(),
                    page: ws.page().unwrap_or_else(|| team.name()).into(),
                    email: ws.email().map(|e| e.into()),
                    repo: ws.repo().map(|e| e.into()),
                    discord: ws.discord().map(|i| v1::DiscordInvite {
                        channel: i.channel.into(),
                        url: i.url.into(),
                    }),
                    zulip_stream: ws.zulip_stream().map(|s| s.into()),
                    matrix_room: ws.matrix_room().map(|s| s.into()),
                    weight: ws.weight(),
                }),
                roles: team
                    .roles()
                    .iter()
                    .map(|role| v1::MemberRole {
                        id: role.id.clone(),
                        description: role.description.clone(),
                    })
                    .collect(),
                discord: team
                    .discord_roles()
                    .map(|roles| {
                        roles
                            .iter()
                            .map(|role| v1::TeamDiscord {
                                name: role.name().into(),
                                color: role.color().map(String::from),
                                members: member_discord_ids.clone(),
                            })
                            .collect()
                    })
                    .unwrap_or_else(Vec::new),
            };

            self.add(&format!("v1/teams/{}.json", team.name()), &team_data)?;
            teams.insert(team.name().into(), team_data);
        }

        teams.sort_keys();
        self.add("v1/teams.json", &v1::Teams { teams })?;
        Ok(())
    }

    fn generate_lists(&self) -> Result<(), Error> {
        let mut lists = IndexMap::new();

        for list in self.data.lists()?.values() {
            let mut members = list.emails().to_vec();
            members.sort();
            lists.insert(
                list.address().to_string(),
                v1::List {
                    address: list.address().to_string(),
                    members,
                },
            );
        }

        lists.sort_keys();
        self.add("v1/lists.json", &v1::Lists { lists })?;
        Ok(())
    }

    fn generate_zulip_groups(&self) -> Result<(), Error> {
        let mut groups = IndexMap::new();

        for group in self.data.zulip_groups()?.values() {
            let mut members = group.members().to_vec();
            members.sort();
            groups.insert(
                group.name().to_string(),
                v1::ZulipGroup {
                    name: group.name().to_string(),
                    members: members
                        .into_iter()
                        .filter_map(|m| match m {
                            ZulipGroupMember::MemberWithId { zulip_id, .. } => {
                                Some(v1::ZulipGroupMember::Id(zulip_id))
                            }
                            ZulipGroupMember::JustId(zulip_id) => {
                                Some(v1::ZulipGroupMember::Id(zulip_id))
                            }
                            ZulipGroupMember::MemberWithoutId { .. } => None,
                        })
                        .collect(),
                },
            );
        }

        groups.sort_keys();
        self.add("v1/zulip-groups.json", &v1::ZulipGroups { groups })?;
        Ok(())
    }

    fn generate_permissions(&self) -> Result<(), Error> {
        for perm in &Permissions::available(self.data.config()) {
            let allowed = crate::permissions::allowed_people(self.data, perm)?;
            let mut github_users = allowed
                .iter()
                .map(|p| p.github().to_string())
                .collect::<Vec<_>>();
            let mut github_ids = allowed.iter().map(|p| p.github_id()).collect::<Vec<_>>();

            let mut discord_ids = allowed
                .iter()
                .filter_map(|p| p.discord_id())
                .collect::<Vec<_>>();

            github_users.sort();
            github_ids.sort_unstable();
            discord_ids.sort_unstable();

            let mut people = allowed
                .iter()
                .map(|p| v1::PermissionPerson {
                    name: p.name().into(),
                    github: p.github().into(),
                    github_id: p.github_id(),
                })
                .collect::<Vec<_>>();

            // The sort operation here is necessary to ensure a stable output for the snapshot tests.
            people.sort();

            self.add(
                &format!("v1/permissions/{}.json", perm.replace('-', "_")),
                &v1::Permission {
                    people,
                    github_users,
                    github_ids,
                    discord_ids,
                },
            )?;
        }
        Ok(())
    }

    fn generate_rfcbot(&self) -> Result<(), Error> {
        let mut teams = IndexMap::new();

        for team in self.data.teams() {
            if let Some(rfcbot) = team.rfcbot_data() {
                let mut members = team
                    .members(self.data)?
                    .into_iter()
                    .map(|s| s.to_string())
                    .filter(|member| !rfcbot.exclude_members.contains(member))
                    .collect::<Vec<_>>();
                members.sort();
                teams.insert(
                    rfcbot.label.clone(),
                    v1::RfcbotTeam {
                        name: rfcbot.name.clone(),
                        ping: rfcbot.ping.clone(),
                        members,
                    },
                );
            }
        }

        teams.sort_keys();
        self.add("v1/rfcbot.json", &v1::Rfcbot { teams })?;
        Ok(())
    }

    fn generate_zulip_map(&self) -> Result<(), Error> {
        let mut zulip_people = IndexMap::new();

        for person in self.data.people() {
            if let Some(zulip_id) = person.zulip_id() {
                zulip_people.insert(zulip_id, person.github_id());
            }
        }

        zulip_people.sort_keys();
        self.add(
            "v1/zulip-map.json",
            &v1::ZulipMapping {
                users: zulip_people,
            },
        )?;
        Ok(())
    }

    fn generate_people(&self) -> Result<(), Error> {
        let mut people = IndexMap::new();

        for person in self.data.people() {
            people.insert(
                person.github().into(),
                v1::Person {
                    name: person.name().into(),
                    email: match person.email() {
                        Email::Missing | Email::Disabled => None,
                        Email::Present(s) => Some(s.into()),
                    },
                    github_id: person.github_id(),
                },
            );
        }

        people.sort_keys();

        self.add("v1/people.json", &v1::People { people })?;

        Ok(())
    }

    fn add<T>(&self, path: &str, obj: &T) -> Result<(), Error>
    where
        T: serde::Serialize + serde::de::DeserializeOwned + PartialEq,
    {
        info!("writing API object {}...", path);
        let dest = self.dest.join(path);
        if let Some(parent) = dest.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let json = serde_json::to_string_pretty(obj)?;
        std::fs::write(&dest, json.as_bytes())?;

        let obj2: T =
            serde_json::from_str(&json).with_context(|| format!("failed to deserialize {path}"))?;
        ensure!(
            *obj == obj2,
            "deserializing {path} produced a different result than what was serialized",
        );

        Ok(())
    }
}
