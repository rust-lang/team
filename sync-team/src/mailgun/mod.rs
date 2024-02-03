mod api;

use std::collections::{HashMap, HashSet};
use std::str;

use self::api::Mailgun;
use crate::TeamApi;
use anyhow::{bail, Context};
use log::info;
use rust_team_data::{email_encryption, v1 as team_data};

const DESCRIPTION: &str = "managed by an automatic script on github";

// Limit (in bytes) of the size of a Mailgun rule's actions list.
const ACTIONS_SIZE_LIMIT_BYTES: usize = 4000;

#[derive(Debug, Clone, PartialEq, Eq)]
struct List {
    address: String,
    members: Vec<String>,
    priority: i32,
}

fn mangle_lists(email_encryption_key: &str, lists: team_data::Lists) -> anyhow::Result<Vec<List>> {
    let mut result = Vec::new();

    for (_key, mut list) in lists.lists.into_iter() {
        // Handle encrypted list addresses.
        list.address = email_encryption::try_decrypt(email_encryption_key, &list.address)?;

        let base_list = List {
            address: mangle_address(&list.address)?,
            members: Vec::new(),
            priority: 0,
        };

        // Mailgun only supports at most 4000 bytes of "actions" for each rule, and some of our
        // lists have so many members we're going over that limit.
        //
        // The official workaround for this, as explained in the docs [1], is to create multiple
        // rules, all with the same filter but each with a different set of actions. This snippet
        // of code implements that.
        //
        // Since all the lists have the same address, to differentiate them during the sync this
        // also sets the priority of the rule to the partition number.
        //
        // [1] https://documentation.mailgun.com/en/latest/user_manual.html#routes
        let mut current_list = base_list.clone();
        let mut current_actions_len = 0;
        let mut partitions_count = 0;
        for mut member in list.members {
            // Handle encrypted member email addresses.
            member = email_encryption::try_decrypt(email_encryption_key, &member)?;

            let action = build_route_action(&member);
            if current_actions_len + action.len() > ACTIONS_SIZE_LIMIT_BYTES {
                partitions_count += 1;
                result.push(current_list);

                current_list = base_list.clone();
                current_list.priority = partitions_count;
                current_actions_len = 0;
            }

            current_actions_len += action.len();
            current_list.members.push(member);
        }

        result.push(current_list);
    }

    Ok(result)
}

fn mangle_address(addr: &str) -> anyhow::Result<String> {
    // Escape dots since they have a special meaning in Python regexes
    let mangled = addr.replace('.', "\\.");

    // Inject (?:\+.+)? before the '@' in the address to support '+' aliases like
    // infra+botname@rust-lang.org
    if let Some(at_pos) = mangled.find('@') {
        let (user, domain) = mangled.split_at(at_pos);
        Ok(format!("^{user}(?:\\+.+)?{domain}$"))
    } else {
        bail!("the address `{}` doesn't have any '@'", addr);
    }
}

pub(crate) fn run(
    token: &str,
    email_encryption_key: &str,
    team_api: &TeamApi,
    dry_run: bool,
) -> anyhow::Result<()> {
    let mailgun = Mailgun::new(token, dry_run);
    let mailmap = team_api.get_lists()?;

    // Mangle all the mailing lists
    let lists = mangle_lists(email_encryption_key, mailmap)?;

    let mut routes = Vec::new();
    let mut response = mailgun.get_routes(None)?;
    let mut cur = 0u64;
    while !response.items.is_empty() {
        cur += response.items.len() as u64;
        routes.extend(response.items);
        if cur >= response.total_count {
            break;
        }
        response = mailgun.get_routes(Some(cur))?;
    }

    let mut addr2list = HashMap::new();
    for list in &lists {
        if addr2list
            .insert((list.address.clone(), list.priority), list)
            .is_some()
        {
            bail!(
                "duplicate address: {} (with priority {})",
                list.address,
                list.priority
            );
        }
    }

    for route in routes {
        if route.description != DESCRIPTION {
            continue;
        }
        let address = extract(&route.expression, "match_recipient(\"", "\")");
        let key = (address.to_string(), route.priority);
        match addr2list.remove(&key) {
            Some(new_list) => sync(&mailgun, &route, new_list)
                .with_context(|| format!("failed to sync {address}"))?,
            None => mailgun
                .delete_route(&route.id)
                .with_context(|| format!("failed to delete {address}"))?,
        }
    }

    for (_, list) in addr2list.iter() {
        create(&mailgun, list).with_context(|| format!("failed to create {}", list.address))?;
    }

    Ok(())
}

fn build_route_action(member: &str) -> String {
    format!("forward(\"{member}\")")
}

fn build_route_actions(list: &List) -> impl Iterator<Item = String> + '_ {
    list.members.iter().map(|member| build_route_action(member))
}

fn create(mailgun: &Mailgun, list: &List) -> anyhow::Result<()> {
    info!("creating list {}", list.address);

    let expr = format!("match_recipient(\"{}\")", list.address);
    let actions = build_route_actions(list).collect::<Vec<_>>();
    mailgun.create_route(list.priority, DESCRIPTION, &expr, &actions)?;
    Ok(())
}

fn sync(mailgun: &Mailgun, route: &api::Route, list: &List) -> anyhow::Result<()> {
    let before = route
        .actions
        .iter()
        .map(|action| extract(action, "forward(\"", "\")"))
        .collect::<HashSet<_>>();
    let after = list.members.iter().map(|s| &s[..]).collect::<HashSet<_>>();
    if before == after {
        return Ok(());
    }

    info!("updating list {}", list.address);
    let actions = build_route_actions(list).collect::<Vec<_>>();
    mailgun.update_route(&route.id, list.priority, &actions)?;
    Ok(())
}

fn extract<'a>(s: &'a str, prefix: &str, suffix: &str) -> &'a str {
    assert!(s.starts_with(prefix), "`{s}` didn't start with `{prefix}`");
    assert!(s.ends_with(suffix), "`{s}` didn't end with `{suffix}`");
    &s[prefix.len()..s.len() - suffix.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_team_data::email_encryption;

    #[test]
    fn test_build_route_actions() {
        let list = List {
            address: "list@example.com".into(),
            members: vec![
                "foo@example.com".into(),
                "bar@example.com".into(),
                "baz@example.net".into(),
            ],
            priority: 0,
        };

        assert_eq!(
            vec![
                "forward(\"foo@example.com\")",
                "forward(\"bar@example.com\")",
                "forward(\"baz@example.net\")",
            ],
            build_route_actions(&list).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_mangle_address() {
        assert_eq!(
            r"^list-name(?:\+.+)?@example\.com$",
            mangle_address("list-name@example.com").unwrap()
        );
        assert!(mangle_address("list-name.example.com").is_err());
    }

    #[test]
    fn test_mangle_lists() {
        const ENCRYPTION_KEY: &str = "mGDTk1eIx8P2gTerzKXwvun67d41iUid";

        let secret_list = email_encryption::encrypt(ENCRYPTION_KEY, "secret-list@example.com")
            .expect("failed to encrypt list");
        let secret_member = email_encryption::encrypt(ENCRYPTION_KEY, "secret-member@example.com")
            .expect("failed to encrypt member");

        let original = rust_team_data::v1::Lists {
            lists: indexmap::indexmap![
                "small@example.com".to_string() => rust_team_data::v1::List {
                    address: "small@example.com".into(),
                    members: vec![
                        "foo@example.com".into(),
                        "bar@example.com".into(),
                        secret_member.clone(),
                    ],
                },
                secret_list.clone() => rust_team_data::v1::List {
                    address: secret_list,
                    members: vec![secret_member, "baz@example.com".into()]
                },
                "big@example.com".into() => rust_team_data::v1::List {
                    address: "big@example.com".into(),
                    // Generate 300 members automatically to simulate a big list, and test whether the
                    // partitioning mechanism works.
                    members: (0..300).map(|i| format!("foo{i:03}@example.com")).collect(),
                },
            ],
        };

        let mangled = mangle_lists(ENCRYPTION_KEY, original).unwrap();
        let expected = vec![
            List {
                address: mangle_address("small@example.com").unwrap(),
                priority: 0,
                members: vec![
                    "foo@example.com".into(),
                    "bar@example.com".into(),
                    "secret-member@example.com".into(),
                ],
            },
            List {
                address: mangle_address("secret-list@example.com").unwrap(),
                priority: 0,
                members: vec!["secret-member@example.com".into(), "baz@example.com".into()],
            },
            // With ACTIONS_SIZE_LIMIT_BYTES = 4000, each list can contain at most 137 users named
            // `fooNNN@example.com`. If the limit is changed the numbers will need to be updated.
            List {
                address: mangle_address("big@example.com").unwrap(),
                priority: 0,
                members: (0..137)
                    .map(|i| format!("foo{i:03}@example.com"))
                    .collect::<Vec<_>>(),
            },
            List {
                address: mangle_address("big@example.com").unwrap(),
                priority: 1,
                members: (137..274)
                    .map(|i| format!("foo{i:03}@example.com"))
                    .collect::<Vec<_>>(),
            },
            List {
                address: mangle_address("big@example.com").unwrap(),
                priority: 2,
                members: (274..300)
                    .map(|i| format!("foo{i:03}@example.com"))
                    .collect::<Vec<_>>(),
            },
        ];
        assert_eq!(expected, mangled);
    }
}
