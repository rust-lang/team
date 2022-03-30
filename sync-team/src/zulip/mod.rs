use failure::Error;
use reqwest::Client;
use serde::Deserialize;

use crate::team_api::TeamApi;

pub(crate) fn run(token: String, team_api: &TeamApi, dry_run: bool) -> Result<(), Error> {
    let zulip = ZulipApi::new(token, dry_run);
    let teams = team_api.get_teams()?;
    let zulip_map = team_api.get_zulip_map()?;
    let members = zulip.get_members()?;

    // This maps from Zulip email to Zulip id
    let email_map = {
        let mut map = std::collections::BTreeMap::new();
        for member in &members {
            map.insert(member.email.clone(), member.user_id);
        }
        map
    };

    // This maps from Zulip name to Zulip id
    let name_map = {
        let mut map = std::collections::BTreeMap::new();
        for member in members {
            map.insert(member.name, member.user_id);
        }
        map
    };

    // The zulip map goes from zulip id to github id so we need to reverse it
    let zulip_map = {
        let mut map = std::collections::BTreeMap::new();
        for (zulip_id, github_id) in &zulip_map.users {
            map.insert(*github_id, *zulip_id);
        }
        map
    };
    for team in teams
        .iter()
        .filter(|t| matches!(t.kind, rust_team_data::v1::TeamKind::Team))
    {
        let name = format!("T-{}", team.name);
        let mut ids = vec![];
        for member in &team.members {
            let id = zulip_id_from_member(&name_map, &email_map, &zulip_map, member)?;
            match id {
                Some(id) => ids.push(id),
                None => log::error!(
                    "could not find id for {} {} {:?}",
                    member.name,
                    member.github,
                    member.email
                ),
            }
        }
        zulip.create_user_group(&name, &ids)?;

        // Add all team members to team
        //   TODO: decide what should happen if there's no GitHub to Zulip mapping
        //   TODO: decide if those in Zulip and not in team repo should be deleted
    }
    Ok(())
}

fn zulip_id_from_member(
    name_map: &std::collections::BTreeMap<String, usize>,
    email_map: &std::collections::BTreeMap<String, usize>,
    zulip_map: &std::collections::BTreeMap<usize, usize>,
    member: &rust_team_data::v1::TeamMember,
) -> Result<Option<usize>, Error> {
    if let Some(id) = zulip_map.get(&member.github_id) {
        return Ok(Some(*id));
    }
    if let Some(id) = name_map.get(&member.github) {
        return Ok(Some(*id));
    }
    if let Some(id) = name_map.get(&member.name) {
        return Ok(Some(*id));
    }

    let email = match &member.email {
        Some(e) => e,
        None => return Ok(None),
    };

    Ok(email_map.get(email).copied())
}

struct ZulipApi {
    client: Client,
    token: String,
    dry_run: bool,
}

const ZULIP_BASE_URL: &str = "https://rust-lang.zulipchat.com/api/v1";
const BOT_EMAIL: &str = "me@ryanlevick.com"; // TODO: Change

impl ZulipApi {
    fn new(token: String, dry_run: bool) -> Self {
        Self {
            client: Client::new(),
            token,
            dry_run,
        }
    }

    fn create_user_group(&self, name: &str, member_ids: &[usize]) -> Result<(), Error> {
        log::info!(
            "creating Zulip user group '{}' with member ids: {:?}",
            name,
            member_ids
        );
        if !self.dry_run {
            let mut form = std::collections::HashMap::new();
            form.insert("name", name.to_owned());
            form.insert("description", "DESCRIPTION".to_owned());
            form.insert(
                "members",
                format!(
                    "[{}]",
                    member_ids
                        .into_iter()
                        .map(|id| id.to_string())
                        .collect::<Vec<_>>()
                        .join(",")
                ),
            );

            let mut r = self
                .client
                .post(&format!("{}/user_groups/create", ZULIP_BASE_URL))
                .basic_auth(BOT_EMAIL, Some(&self.token))
                .form(&form)
                .send()?;
            if r.status() == 400 {
                let body = r.json::<serde_json::Value>()?;
                let error = body
                    .get("msg")
                    .ok_or_else(|| {
                        failure::format_err!("got 400 when creating user group {}: {}", name, body)
                    })?
                    .as_str()
                    .ok_or_else(|| {
                        failure::format_err!("got 400 when creating user group {}: {}", name, body)
                    })?;
                if error.contains("already exists") {
                    return Ok(());
                } else {
                    return Err(failure::format_err!(
                        "got 400 when creating user group {}: {}",
                        name,
                        body
                    ));
                }
            }

            r.error_for_status()?;
        }

        Ok(())
    }

    fn get_members(&self) -> Result<Vec<ZulipUser>, Error> {
        let response = self
            .client
            .get(&format!("{}/users", ZULIP_BASE_URL,))
            .basic_auth(BOT_EMAIL, Some(&self.token))
            .send()?;

        Ok(response.error_for_status()?.json::<ZulipUsers>()?.members)
    }
}

#[derive(Deserialize)]
struct ZulipUsers {
    members: Vec<ZulipUser>,
}

#[derive(Deserialize)]
struct ZulipUser {
    #[serde(rename = "full_name")]
    name: String,
    #[serde(rename = "delivery_email")]
    email: String,
    user_id: usize,
}
