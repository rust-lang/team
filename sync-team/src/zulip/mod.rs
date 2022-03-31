use std::collections::{BTreeMap, HashMap};

use failure::Error;
use reqwest::Client;
use serde::Deserialize;

use crate::team_api::TeamApi;

pub(crate) fn run(token: String, team_api: &TeamApi, dry_run: bool) -> Result<(), Error> {
    let zulip_api = ZulipApi::new(token, dry_run);
    let mut cache = ZulipCache::new(team_api, zulip_api, dry_run)?;

    for team in team_api
        .get_teams()?
        .iter()
        .filter(|t| matches!(t.kind, rust_team_data::v1::TeamKind::Team))
        .filter(|t| t.name == "test")
    {
        let mut member_zulip_ids = vec![];
        for member in &team.members {
            let member_zulip_id = cache.zulip_id_from_member(member)?;
            match member_zulip_id {
                Some(id) => member_zulip_ids.push(id),
                None => log::warn!(
                    "could not find id for {} ({} {:?})",
                    member.name,
                    member.github,
                    member.email
                ),
            }
        }

        // Sort for better diagnostics
        member_zulip_ids.sort();

        cache.create_or_update_user_group(&team.name, &member_zulip_ids)?;
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
    /// User group name to user group id
    user_groups: BTreeMap<String, ZulipUserGroup>,
    /// The Zulip API
    zulip_api: ZulipApi,
    /// Whether this is a dry run or not
    dry_run: bool,
}

impl ZulipCache {
    fn new(team_api: &TeamApi, zulip_api: ZulipApi, dry_run: bool) -> Result<Self, Error> {
        let zulip_map = team_api.get_zulip_map()?;
        let members = zulip_api.get_users()?;
        let user_groups = zulip_api.get_user_groups()?;

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

        let user_groups = user_groups
            .into_iter()
            .map(|mut ug| {
                // sort for better diagnostics
                ug.members.sort();
                (ug.name.clone(), ug)
            })
            .collect();

        Ok(Self {
            names,
            emails,
            github_ids,
            user_groups,
            zulip_api,
            dry_run,
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

    fn user_group_id_from_name(&self, name: &str) -> Option<usize> {
        self.user_groups.get(name).map(|u| u.id)
    }

    fn create_user_group(
        &mut self,
        user_group_name: &str,
        description: &str,
        member_ids: &[usize],
    ) -> Result<usize, Error> {
        self.zulip_api
            .create_user_group(user_group_name, description, member_ids)?;

        // Update the user group cache so it has the user group that was just created
        let user_groups = self.zulip_api.get_user_groups()?;
        let user_groups = user_groups
            .into_iter()
            .map(|ug| (ug.name.clone(), ug))
            .collect();
        self.user_groups = user_groups;

        // If this is a dry run, we insert a record since it won't be on the actual API
        if self.dry_run {
            self.user_groups.insert(
                user_group_name.to_owned(),
                ZulipUserGroup {
                    id: 0,
                    name: user_group_name.to_owned(),
                    members: member_ids.into(),
                },
            );
        }

        Ok(self
            .user_group_id_from_name(&user_group_name)
            .expect("user group id not found even thoough it was just created"))
    }

    fn create_or_update_user_group(
        &mut self,
        team_name: &str,
        member_zulip_ids: &[usize],
    ) -> Result<(), Error> {
        let user_group_name = format!("T-{}", team_name);
        let id = self.user_group_id_from_name(&user_group_name);
        let user_group_id = match id {
            Some(id) => {
                log::info!(
                    "'{}' user group ({}) already exists on Zulip",
                    user_group_name,
                    id
                );
                id
            }
            None => {
                log::info!(
                    "no '{}' user group found on Zulip. Creating one...",
                    user_group_name
                );
                self.create_user_group(
                    &user_group_name,
                    &format!("The {} team", team_name),
                    &member_zulip_ids,
                )?
            }
        };

        let existing_members = self.user_group_members_from_name(&user_group_name).unwrap();
        log::info!(
            "'{}' user group ({}) has members on Zulip {:?} and needs to have {:?}",
            user_group_name,
            user_group_id,
            existing_members,
            member_zulip_ids
        );
        let add_ids = member_zulip_ids
            .iter()
            .filter(|i| !existing_members.contains(i))
            .copied()
            .collect::<Vec<_>>();
        let remove_ids = existing_members
            .iter()
            .filter(|i| !member_zulip_ids.contains(i))
            .copied()
            .collect::<Vec<_>>();

        // We don't currently update the members field of the cached user group because it's
        // not necessary, but for correctness sake we should consider doing so
        self.zulip_api
            .update_user_group_members(user_group_id, &add_ids, &remove_ids)
    }

    fn user_group_members_from_name(&self, user_group_name: &str) -> Option<&[usize]> {
        self.user_groups
            .get(user_group_name)
            .map(|u| &u.members[..])
    }
}

/// Access to the Zulip API
#[derive(Clone)]
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
    fn create_user_group(
        &self,
        user_group_name: &str,
        description: &str,
        member_ids: &[usize],
    ) -> Result<(), Error> {
        log::info!(
            "creating Zulip user group '{}' with description '{}' and member ids: {:?}",
            user_group_name,
            description,
            member_ids
        );
        if self.dry_run {
            return Ok(());
        }

        let member_ids = format!(
            "[{}]",
            member_ids
                .into_iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );
        let mut form = HashMap::new();
        form.insert("name", user_group_name);
        form.insert("description", description);
        form.insert("members", &member_ids);

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

        Ok(())
    }

    /// Get all user groups of the Rust Zulip instance
    fn get_user_groups(&self) -> Result<Vec<ZulipUserGroup>, Error> {
        let response = self
            .req(reqwest::Method::GET, "/user_groups", None)?
            .error_for_status()?
            .json::<ZulipUserGroups>()?
            .user_groups;

        Ok(response)
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

    fn update_user_group_members(
        &self,
        user_group_id: usize,
        add_ids: &[usize],
        remove_ids: &[usize],
    ) -> Result<(), Error> {
        if add_ids.is_empty() && remove_ids.is_empty() {
            log::info!(
                "user group {} does not need to have its group members updated",
                user_group_id
            );
            return Ok(());
        }

        log::info!(
            "updating user group {} by adding {:?} and removing {:?}",
            user_group_id,
            add_ids,
            remove_ids
        );

        if self.dry_run {
            return Ok(());
        }

        let add_ids = format!(
            "[{}]",
            add_ids
                .into_iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );
        let remove_ids = format!(
            "[{}]",
            remove_ids
                .into_iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );
        let mut form = HashMap::new();
        form.insert("add", add_ids.as_str());
        form.insert("delete", remove_ids.as_str());

        let path = format!("/user_groups/{}/members", user_group_id);
        let _ = self.req(reqwest::Method::POST, &path, Some(form))?;
        Ok(())
    }

    /// Perform a request against the Zulip API
    fn req(
        &self,
        method: reqwest::Method,
        path: &str,
        form: Option<HashMap<&str, &str>>,
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

/// A collection of Zulip user groups
#[derive(Deserialize)]
struct ZulipUserGroups {
    user_groups: Vec<ZulipUserGroup>,
}

/// A single Zulip user group
#[derive(Deserialize)]
struct ZulipUserGroup {
    id: usize,
    name: String,
    members: Vec<usize>,
}
