use crate::data::Data;
use crate::schema::Team;
use anyhow::{Context, bail, format_err};
use indexmap::IndexSet;
use log::info;
use std::path::Path;
use toml_edit::{Array, Item, Value};

fn get_access_teams(doc: &mut toml_edit::DocumentMut) -> Option<&mut toml_edit::Table> {
    doc.get_mut("access")?.get_mut("teams")?.as_table_mut()
}

fn archive_toml_file<F>(
    src: &Path,
    dest_dir: &Path,
    dest: &Path,
    entity: &str,
    transform: F,
) -> anyhow::Result<()>
where
    F: FnOnce(&mut toml_edit::DocumentMut),
{
    if !src.is_file() {
        bail!("{entity} file not found: {}", src.display());
    }
    if dest.is_file() {
        bail!("{entity} is already archived: {}", dest.display());
    }

    let mut doc = read_toml_mut(src)?;

    transform(&mut doc);

    std::fs::create_dir_all(dest_dir)
        .with_context(|| format!("failed to create directory {dest_dir:?}"))?;
    std::fs::write(dest, doc.to_string()).with_context(|| format!("failed to write {dest:?}"))?;
    std::fs::remove_file(src).with_context(|| format!("failed to remove {src:?}"))?;

    info!("archived {entity} {src:?} -> {dest:?}");
    Ok(())
}

fn read_toml_mut(src: &Path) -> anyhow::Result<toml_edit::DocumentMut> {
    let content =
        std::fs::read_to_string(src).with_context(|| format!("failed to read {src:?}"))?;
    let doc: toml_edit::DocumentMut = content
        .parse()
        .with_context(|| format!("failed to parse {src:?}"))?;
    Ok(doc)
}

/// Archive a repository by moving its TOML file to `repos/archive/<org>/`
/// and clearing every entry from the `[access.teams]` table.
pub fn archive_repo(data_dir: &Path, name: &str) -> anyhow::Result<()> {
    let (org, repo_name) = name
        .split_once('/')
        .ok_or_else(|| format_err!("repository must be in 'org/name' format, got '{name}'"))?;

    let repos_dir = data_dir.join("repos");
    let src = repos_dir.join(org).join(format!("{repo_name}.toml"));
    let dest_dir = repos_dir.join("archive").join(org);
    let dest = dest_dir.join(format!("{repo_name}.toml"));

    archive_toml_file(&src, &dest_dir, &dest, "repo", |doc| {
        if let Some(table) = get_access_teams(doc) {
            table.clear();
        }
    })
}

/// Gather every username from a team's `leads`, `members`, and `alumni`
/// arrays into a single deduplicated, order-preserving set.
///
/// Handles both bare strings (`"alice"`) and inline tables (`{ github = "alice" }`),
/// skipping any entries that don't match either shape or that have an empty
/// `github` field.
fn collect_all_team_members(people_table: &toml_edit::Table) -> IndexSet<String> {
    let mut all = IndexSet::new();
    for key in ["leads", "members", "alumni"] {
        let Some(arr) = people_table.get(key).and_then(|v| v.as_array()) else {
            continue;
        };
        for item in arr.iter() {
            let username = if let Some(s) = item.as_str() {
                s.to_string()
            } else if let Some(tbl) = item.as_inline_table() {
                match tbl.get("github").and_then(|v| v.as_str()) {
                    Some(s) => s.to_string(),
                    None => continue,
                }
            } else {
                continue;
            };
            if !username.is_empty() {
                all.insert(username);
            }
        }
    }
    all
}

/// Build a TOML array of usernames laid out one per line with `    ` indentation
/// and a trailing comma — matching the style used elsewhere in the team repo.
fn build_alumni_array(usernames: &IndexSet<String>) -> toml_edit::Array {
    let mut arr = toml_edit::Array::new();
    for person in usernames {
        let mut val = toml_edit::Value::from(person.as_str());
        val.decor_mut().set_prefix("\n    ");
        arr.push_formatted(val);
    }
    arr.set_trailing("\n");
    arr.set_trailing_comma(true);
    arr
}

/// Move everyone listed in a team's `leads`, `members`, and existing `alumni`
/// into a single `alumni` array, leaving `leads` and `members` empty.
///
/// No-op if the document has no `[people]` table.
fn move_team_members_to_alumni(doc: &mut toml_edit::DocumentMut) {
    let Some(people_table) = doc.get_mut("people").and_then(|v| v.as_table_mut()) else {
        return;
    };

    let all_alumni = collect_all_team_members(people_table);

    people_table.insert("leads", toml_edit::Array::new().into());
    people_table.insert("members", toml_edit::Array::new().into());
    people_table.insert("alumni", build_alumni_array(&all_alumni).into());
}

/// Archive a team by moving its TOML file to `teams/archive/`, collapsing
/// every `leads`/`members`/`alumni` entry into a single `alumni` array,
/// and removing the team from every repo's `[access.teams]` table.
pub fn archive_team(data_dir: &Path, name: &str) -> anyhow::Result<()> {
    let teams_dir = data_dir.join("teams");
    let src = teams_dir.join(format!("{name}.toml"));
    let dest_dir = teams_dir.join("archive");
    let dest = dest_dir.join(format!("{name}.toml"));

    archive_toml_file(&src, &dest_dir, &dest, "team", move_team_members_to_alumni)?;
    remove_team_from_repos(data_dir, name)?;
    Ok(())
}

fn remove_team_from_repos(data_dir: &Path, team_name: &str) -> anyhow::Result<()> {
    let repos_dir = data_dir.join("repos");
    assert!(repos_dir.is_dir(), "`repos` directory does not exist");

    for org_entry in
        std::fs::read_dir(&repos_dir).with_context(|| format!("failed to read {repos_dir:?}"))?
    {
        let org_path = org_entry?.path();
        assert!(
            org_path.is_dir(),
            "unexpected non-directory entry: `repos/{org_path:?}`"
        );
        if org_path.file_name() == Some(std::ffi::OsStr::new("archive")) {
            continue;
        }

        for repo_entry in
            std::fs::read_dir(&org_path).with_context(|| format!("failed to read {org_path:?}"))?
        {
            let repo_path = repo_entry?.path();
            remove_team_from_repository(team_name, &repo_path)?;
        }
    }

    Ok(())
}

fn remove_team_from_repository(team_name: &str, repo_path: &Path) -> anyhow::Result<()> {
    assert!(
        repo_path.is_file(),
        "unexpected non-file entry: `repos/{repo_path:?}`"
    );
    assert!(
        repo_path.extension() == Some(std::ffi::OsStr::new("toml")),
        "unexpected non-TOML file: `repos/{repo_path:?}`"
    );

    let mut doc = read_toml_mut(repo_path)?;

    let removed = if let Some(table) = get_access_teams(&mut doc) {
        table.remove(team_name).is_some()
    } else {
        false
    };

    if removed {
        std::fs::write(repo_path, doc.to_string())
            .with_context(|| format!("failed to write {repo_path:?}"))?;
        info!("removed team '{team_name}' from {repo_path:?}");
    }
    Ok(())
}

pub fn move_person_to_alumni<'a>(
    data: &'a Data,
    data_dir: &Path,
    username: &str,
    team_filter: Vec<String>,
) -> anyhow::Result<Vec<&'a Team>> {
    let username = username.to_lowercase();

    let mut teams = data.teams().collect::<Vec<_>>();
    if !team_filter.is_empty() {
        teams.retain(|t| team_filter.iter().any(|f| f == t.name()));
    }

    teams.retain(|t| {
        t.members(&data)
            .unwrap()
            .iter()
            .any(|m| m.to_lowercase() == username.to_lowercase())
            && t.name() != "all"
            && t.name() != "leads"
    });
    teams.sort_by_key(|t| t.name());
    println!(
        "User {username} found in {} team(s): {}",
        teams.len(),
        teams
            .iter()
            .map(|t| t.name())
            .collect::<Vec<_>>()
            .join(", ")
    );
    for team in teams.iter() {
        let path = data_dir.join("teams").join(format!("{}.toml", team.name()));
        if !path.is_file() {
            return Err(anyhow::anyhow!("Cannot find {path:?}"));
        }
        let mut document = read_toml_mut(&path)?;
        let Some(people) = document.get_mut("people").and_then(|t| t.as_table_mut()) else {
            continue;
        };
        let Some(members) = people.get_mut("members").and_then(|t| t.as_array_mut()) else {
            continue;
        };
        let Some(index) = members
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                if let Some(name) = entry.as_str()
                    && name.to_lowercase() == username
                {
                    Some(index)
                } else if let Some(table) = entry.as_inline_table()
                    && let Some(name) = table.get("github")
                    && let Some(name) = name.as_str()
                    && name.to_lowercase() == username
                {
                    Some(index)
                } else {
                    None
                }
            })
            .next()
        else {
            println!("{username} not found in {}", path.display());
            continue;
        };
        let entry = members.remove(index);

        let alumni = people
            .entry("alumni")
            .or_insert(Item::Value(Value::Array(Array::new())))
            .as_array_mut()
            .unwrap();
        alumni.push_formatted(entry);

        std::fs::write(path, document.to_string())?;
    }
    Ok(teams)
}
