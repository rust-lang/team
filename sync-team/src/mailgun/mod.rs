mod api;
mod http;

use std::collections::{HashMap, HashSet};
use std::str;

use self::api::Empty;
use curl::easy::Form;
use failure::{bail, Error, ResultExt};
use rust_team_data::v1 as team_data;

const DESCRIPTION: &str = "managed by an automatic script on github";

// Limit (in bytes) of the size of a Mailgun rule's actions list.
const ACTIONS_SIZE_LIMIT_BYTES: usize = 4000;

#[derive(Debug, Clone)]
struct List {
    address: String,
    members: Vec<String>,
    priority: i32,
}

fn mangle_lists(lists: team_data::Lists) -> Result<Vec<List>, Error> {
    let mut result = Vec::new();

    for (_key, list) in lists.lists.into_iter() {
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
        for member in list.members {
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

fn mangle_address(addr: &str) -> Result<String, Error> {
    // Escape dots since they have a special meaning in Python regexes
    let mangled = addr.replace(".", "\\.");

    // Inject (?:\+.+)? before the '@' in the address to support '+' aliases like
    // infra+botname@rust-lang.org
    if let Some(at_pos) = mangled.find('@') {
        let (user, domain) = mangled.split_at(at_pos);
        Ok(format!("^{}(?:\\+.+)?{}$", user, domain))
    } else {
        bail!("the address `{}` doesn't have any '@'", addr);
    }
}

pub(crate) fn run() -> Result<(), Error> {
    let api_url = if let Ok(url) = std::env::var("TEAM_DATA_BASE_URL") {
        format!("{}/lists.json", url)
    } else {
        format!("{}/lists.json", team_data::BASE_URL)
    };
    let mailmap = http::get::<team_data::Lists>(&api_url)?;

    // Mangle all the mailing lists
    let lists = mangle_lists(mailmap)?;

    let mut routes = Vec::new();
    let mut response = http::get::<api::RoutesResponse>("/routes")?;
    let mut cur = 0;
    while response.items.len() > 0 {
        cur += response.items.len();
        routes.extend(response.items);
        if cur >= response.total_count {
            break;
        }
        let url = format!("/routes?skip={}", cur);
        response = http::get::<api::RoutesResponse>(&url)?;
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
            Some(new_list) => {
                sync(&route, &new_list).with_context(|_| format!("failed to sync {}", address))?
            }
            None => del(&route).with_context(|_| format!("failed to delete {}", address))?,
        }
    }

    for (_, list) in addr2list.iter() {
        create(list).with_context(|_| format!("failed to create {}", list.address))?;
    }

    Ok(())
}

fn build_route_action(member: &str) -> String {
    format!("forward(\"{}\")", member)
}

fn build_route_actions(list: &List) -> impl Iterator<Item = String> + '_ {
    list.members.iter().map(|member| build_route_action(member))
}

fn create(new: &List) -> Result<(), Error> {
    let mut form = Form::new();
    form.part("priority")
        .contents(new.priority.to_string().as_bytes())
        .add()?;
    form.part("description")
        .contents(DESCRIPTION.as_bytes())
        .add()?;
    let expr = format!("match_recipient(\"{}\")", new.address);
    form.part("expression").contents(expr.as_bytes()).add()?;
    for action in build_route_actions(new) {
        form.part("action").contents(action.as_bytes()).add()?;
    }
    http::post::<Empty>("/routes", form)?;

    Ok(())
}

fn sync(route: &api::Route, list: &List) -> Result<(), Error> {
    let before = route
        .actions
        .iter()
        .map(|action| extract(action, "forward(\"", "\")"))
        .collect::<HashSet<_>>();
    let after = list.members.iter().map(|s| &s[..]).collect::<HashSet<_>>();
    if before == after {
        return Ok(());
    }

    let mut form = Form::new();
    form.part("priority")
        .contents(list.priority.to_string().as_bytes())
        .add()?;
    for action in build_route_actions(list) {
        form.part("action").contents(action.as_bytes()).add()?;
    }
    http::put::<Empty>(&format!("/routes/{}", route.id), form)?;

    Ok(())
}

fn del(route: &api::Route) -> Result<(), Error> {
    http::delete::<Empty>(&format!("/routes/{}", route.id))?;
    Ok(())
}

fn extract<'a>(s: &'a str, prefix: &str, suffix: &str) -> &'a str {
    assert!(
        s.starts_with(prefix),
        "`{}` didn't start with `{}`",
        s,
        prefix
    );
    assert!(s.ends_with(suffix), "`{}` didn't end with `{}`", s, suffix);
    &s[prefix.len()..s.len() - suffix.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let original = rust_team_data::v1::Lists {
            lists: indexmap::indexmap![
                "small@example.com".to_string() => rust_team_data::v1::List {
                    address: "small@example.com".into(),
                    members: vec![
                        "foo@example.com".into(),
                        "bar@example.com".into(),
                    ],
                },
                "big@example.com".into() => rust_team_data::v1::List {
                    address: "big@example.com".into(),
                    // Generate 300 members automatically to simulate a big list, and test whether the
                    // partitioning mechanism works.
                    members: (0..300).map(|i| format!("foo{:03}@example.com", i)).collect(),
                },
            ],
        };

        let mangled = mangle_lists(original).unwrap();
        assert_eq!(4, mangled.len());

        let small = &mangled[0];
        assert_eq!(small.address, mangle_address("small@example.com").unwrap());
        assert_eq!(small.priority, 0);
        assert_eq!(small.members, vec!["foo@example.com", "bar@example.com",]);

        // With ACTIONS_SIZE_LIMIT_BYTES = 4000, each list can contain at most 137 users named
        // `fooNNN@example.com`. If the limit is changed the numbers will need to be updated.

        let big_part1 = &mangled[1];
        assert_eq!(
            big_part1.address,
            mangle_address("big@example.com").unwrap()
        );
        assert_eq!(big_part1.priority, 0);
        assert_eq!(
            big_part1.members,
            (0..137)
                .map(|i| format!("foo{:03}@example.com", i))
                .collect::<Vec<_>>()
        );

        let big_part2 = &mangled[2];
        assert_eq!(
            big_part2.address,
            mangle_address("big@example.com").unwrap()
        );
        assert_eq!(big_part2.priority, 1);
        assert_eq!(
            big_part2.members,
            (137..274)
                .map(|i| format!("foo{:03}@example.com", i))
                .collect::<Vec<_>>()
        );

        let big_part3 = &mangled[3];
        assert_eq!(
            big_part3.address,
            mangle_address("big@example.com").unwrap()
        );
        assert_eq!(big_part3.priority, 2);
        assert_eq!(
            big_part3.members,
            (274..300)
                .map(|i| format!("foo{:03}@example.com", i))
                .collect::<Vec<_>>()
        );
    }
}
