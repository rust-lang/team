use crate::data::Data;
use crate::schema::Permissions;
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
                        is_lead: leads.contains(github_name),
                    });
                }
            }
            members.sort_by_key(|member| member.github.to_lowercase());
            members.sort_by_key(|member| !member.is_lead);

            let team_data = v1::Team {
                name: team.name().into(),
                kind: if team.is_wg() {
                    v1::TeamKind::WorkingGroup
                } else {
                    v1::TeamKind::Team
                },
                subteam_of: team.subteam_of().map(|st| st.into()),
                members,
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
                    weight: ws.weight(),
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
        for perm in Permissions::AVAILABLE {
            let mut github_users = crate::permissions::allowed_github_users(&self.data, perm)?
                .into_iter()
                .collect::<Vec<_>>();
            github_users.sort();
            self.add(
                &format!("v1/permissions/{}.json", perm),
                &v1::Permission { github_users },
            )?;
        }
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
