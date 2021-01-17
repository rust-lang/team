use crate::data::Data;
use crate::schema::{Permissions, TeamKind};
use failure::Error;
use indexmap::IndexMap;
use log::info;
use rust_team_data::v1;
use std::path::Path;

pub(crate) struct Generator<'a> {
    dest: &'a Path,
    data: &'a Data,
}

impl<'a> Generator<'a> {
    pub(crate) fn new(dest: &'a Path, data: &'a Data) -> Result<Generator<'a>, Error> {
        if dest.is_dir() {
            std::fs::remove_dir_all(&dest)?;
        }
        std::fs::create_dir_all(&dest)?;

        Ok(Generator { dest, data })
    }

    pub(crate) fn generate(&self) -> Result<(), Error> {
        self.generate_teams()?;
        self.generate_lists()?;
        self.generate_permissions()?;
        self.generate_rfcbot()?;
        self.generate_zulip_map()?;
        Ok(())
    }

    fn generate_teams(&self) -> Result<(), Error> {
        let mut teams = IndexMap::new();

        for team in self.data.teams() {
            let leads = team.leads();
            let mut members = Vec::new();
            for github_name in &team.members(&self.data)? {
                if let Some(person) = self.data.person(github_name) {
                    members.push(v1::TeamMember {
                        name: person.name().into(),
                        github: (*github_name).into(),
                        github_id: person.github_id(),
                        is_lead: leads.contains(github_name),
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
                    });
                }
            }
            alumni.sort_by_key(|member| member.github.to_lowercase());

            let mut github_teams = team.github_teams(&self.data)?;
            github_teams.sort();

            let member_discord_ids = team.discord_ids(&self.data)?;

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
                            members: team.members,
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
                    weight: ws.weight(),
                }),
                discord: team.discord_role().map(|role| {
                    v1::TeamDiscord {
                        name: role.name().into(),
                        role_id: role.role_id(),
                        members: member_discord_ids,
                    }
                }),
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

    fn generate_permissions(&self) -> Result<(), Error> {
        for perm in &Permissions::available(self.data.config()) {
            let allowed = crate::permissions::allowed_people(&self.data, perm)?;
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
            self.add(
                &format!("v1/permissions/{}.json", perm.replace('-', "_")),
                &v1::Permission {
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
                    .members(&self.data)?
                    .into_iter()
                    .map(|s| s.to_string())
                    .filter(|member| !rfcbot.exclude_members.contains(&member))
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

    fn add<T: serde::Serialize>(&self, path: &str, obj: &T) -> Result<(), Error> {
        info!("writing API object {}...", path);
        let dest = self.dest.join(path);
        if let Some(parent) = dest.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let json = serde_json::to_string_pretty(obj)?;
        std::fs::write(&dest, json.as_bytes())?;
        Ok(())
    }
}
