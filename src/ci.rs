use crate::schema::Team;
use anyhow::Context;
use std::path::{Path, PathBuf};

/// Generates the contents of `.github/CODEOWNERS`, based on
/// the infra admins in `infra-admins.toml`.
pub fn generate_codeowners_file() -> anyhow::Result<()> {
    let admins = load_infra_admins()?;
    let codeowners_content = generate_codeowners_content(admins);
    std::fs::write(codeowners_path(), codeowners_content).context("cannot write CODEOWNERS")?;
    Ok(())
}

fn generate_codeowners_content(admins: Vec<String>) -> String {
    use std::fmt::Write;

    let mut output = String::new();
    writeln!(
        output,
        r#"# This is an automatically generated file
# Run `cargo run ci generate-codeowners` to regenerate it.
"#
    )
    .unwrap();

    let admin_list = admins
        .iter()
        .map(|admin| format!("@{admin}"))
        .collect::<Vec<_>>()
        .join(" ");

    // Set of paths that should only be modifiable by infra-admins
    let mut secure_paths = vec![
        "/.github/".to_string(),
        "/src/".to_string(),
        "/rust_team_data/".to_string(),
        "/repos/rust-lang/team.toml".to_string(),
        "/repos/rust-lang/sync-team.toml".to_string(),
        "/teams/infra-admins.toml".to_string(),
        "/teams/team-repo-admins.toml".to_string(),
        ".cargo".to_string(),
        "target".to_string(),
        "Cargo.lock".to_string(),
        "Cargo.toml".to_string(),
        "config.toml".to_string(),
    ];
    for admin in admins {
        secure_paths.push(format!("/people/{admin}.toml"));
    }

    for path in secure_paths {
        writeln!(output, "{path} {admin_list}").unwrap();
    }
    output
}

fn codeowners_path() -> PathBuf {
    Path::new(&env!("CARGO_MANIFEST_DIR"))
        .join(".github")
        .join("CODEOWNERS")
}

fn load_infra_admins() -> anyhow::Result<Vec<String>> {
    let admins = std::fs::read_to_string(
        Path::new(&env!("CARGO_MANIFEST_DIR"))
            .join("teams")
            .join("infra-admins.toml"),
    )
    .context("cannot load infra-admins.toml")?;
    let team: Team = toml::from_str(&admins).context("cannot deserialize infra-admins")?;
    Ok(team
        .raw_people()
        .members
        .iter()
        .map(|member| member.github.clone())
        .collect())
}
