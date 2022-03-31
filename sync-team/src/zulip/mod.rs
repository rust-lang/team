use std::collections::{BTreeMap, HashMap};

use failure::Error;
use reqwest::Client;
use serde::Deserialize;

use crate::team_api::TeamApi;

pub(crate) fn run(token: String, team_api: &TeamApi, dry_run: bool) -> Result<(), Error> {
    let zulip_api = ZulipApi::new(token, dry_run);
    let cache = ZulipCache::new(team_api, &zulip_api)?;
    for team in cache
        .teams()
        .iter()
        .filter(|t| matches!(t.kind, rust_team_data::v1::TeamKind::Team))
    {
        let mut ids = vec![];
        for member in &team.members {
            let id = cache.zulip_id_from_member(member)?;
            match id {
                Some(id) => ids.push(id),
                None => log::warn!(
                    "could not find id for {} ({} {:?})",
                    member.name,
                    member.github,
                    member.email
                ),
            }
        }
        zulip_api.create_user_group(&team.name, &ids)?;
    }
    Ok(())
}

/// Caches data about teams and Zulip for easy and efficient lookup
struct ZulipCache {
    /// Map of Zulip user names to Zulip user ids
    names: BTreeMap<String, usize>,
    /// Map of Zulip emails to Zulip user ids
    emails: BTreeMap<String, usize>,
    /// Map of GitHub ids to Zulip user ids
    github_ids: BTreeMap<usize, usize>,
    /// Teams
    teams: Vec<rust_team_data::v1::Team>,
}

impl ZulipCache {
    fn new(team_api: &TeamApi, zulip_api: &ZulipApi) -> Result<Self, Error> {
        let teams = team_api.get_teams()?;
        let zulip_map = team_api.get_zulip_map()?;
        let members = zulip_api.get_users()?;

        let (names, emails) = {
            let mut names = BTreeMap::new();
            let mut emails = BTreeMap::new();
            for member in members {
                names.insert(member.name, member.user_id);
                emails.insert(member.email, member.user_id);
            }
            (names, emails)
        };

        let github_ids = zulip_map
            .users
            .iter()
            .map(|(zulip_id, github_id)| (*github_id, *zulip_id))
            .collect();

        Ok(Self {
            teams,
            names,
            emails,
            github_ids,
        })
    }

    /// Get a Zulip user id for a Team member
    fn zulip_id_from_member(
        &self,
        member: &rust_team_data::v1::TeamMember,
    ) -> Result<Option<usize>, Error> {
        if let Some(id) = self.github_ids.get(&member.github_id) {
            return Ok(Some(*id));
        }
        if let Some(id) = self.names.get(&member.github) {
            return Ok(Some(*id));
        }
        if let Some(id) = self.names.get(&member.name) {
            return Ok(Some(*id));
        }

        let email = match &member.email {
            Some(e) => e,
            None => return Ok(None),
        };

        Ok(self.emails.get(email).copied())
    }

    fn teams(&self) -> &[rust_team_data::v1::Team] {
        &self.teams[..]
    }
}

/// Access to the Zulip API
struct ZulipApi {
    client: Client,
    token: String,
    dry_run: bool,
}

const ZULIP_BASE_URL: &str = "https://rust-lang.zulipchat.com/api/v1";
const BOT_EMAIL: &str = "me@ryanlevick.com"; // TODO: Change

impl ZulipApi {
    /// Create a new `ZulipApi` instance
    fn new(token: String, dry_run: bool) -> Self {
        Self {
            client: Client::new(),
            token,
            dry_run,
        }
    }

    /// Creates a Zulip user group with the supplied name and members
    ///
    /// The user group's name will be of the form T-$name. This is a
    /// noop if the user group already exists.
    fn create_user_group(&self, name: &str, member_ids: &[usize]) -> Result<(), Error> {
        let user_group_name = format!("T-{}", name);
        log::info!(
            "creating Zulip user group '{}' with member ids: {:?}",
            user_group_name,
            member_ids
        );
        if !self.dry_run {
            let member_ids = format!(
                "[{}]",
                member_ids
                    .into_iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            );
            let mut form = HashMap::new();
            form.insert("name", user_group_name.clone());
            form.insert("description", format!("The {} team", name));
            form.insert("members", member_ids);

            let mut r = self.req(reqwest::Method::POST, "/user_groups/create", Some(form))?;
            if r.status() == 400 {
                let body = r.json::<serde_json::Value>()?;
                let err = || {
                    failure::format_err!(
                        "got 400 when creating user group {}: {}",
                        user_group_name,
                        body
                    )
                };
                let error = body.get("msg").ok_or_else(err)?.as_str().ok_or_else(err)?;
                if error.contains("already exists") {
                    return Ok(());
                } else {
                    return Err(err());
                }
            }

            r.error_for_status()?;
        }

        Ok(())
    }

    /// Get all users of the Rust Zulip instance
    fn get_users(&self) -> Result<Vec<ZulipUser>, Error> {
        let response = self
            .req(reqwest::Method::GET, "/users", None)?
            .error_for_status()?
            .json::<ZulipUsers>()?
            .members;

        Ok(response)
    }

    /// Perform a request against the Zulip API
    fn req(
        &self,
        method: reqwest::Method,
        path: &str,
        form: Option<HashMap<&str, String>>,
    ) -> Result<reqwest::Response, Error> {
        let mut req = self
            .client
            .request(method, &format!("{}{}", ZULIP_BASE_URL, path))
            .basic_auth(BOT_EMAIL, Some(&self.token));
        if let Some(form) = form {
            req = req.form(&form);
        }

        Ok(req.send()?)
    }
}

/// A collection of Zulip users
#[derive(Deserialize)]
struct ZulipUsers {
    members: Vec<ZulipUser>,
}

/// A single Zulip user
#[derive(Deserialize)]
struct ZulipUser {
    #[serde(rename = "full_name")]
    name: String,
    #[serde(rename = "delivery_email")]
    email: String,
    user_id: usize,
}
