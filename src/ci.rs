use anyhow::Context;
use std::path::{Path, PathBuf};

/// Generates the contents of `.github/CODEOWNERS`, based on
/// the infra admins in `infra-admins.txt`.
pub fn generate_codeowners_file() -> anyhow::Result<()> {
    let admins = load_infra_admins()?;
    let codeowners_content = generate_codeowners_content(admins);
    std::fs::write(codeowners_path(), codeowners_content).context("cannot write CODEOWNERS")?;
    Ok(())
}

/// Check if `.github/CODEOWNERS` are up-to-date, based on the
/// `infra-admins.txt` file.
pub fn check_codeowners() -> anyhow::Result<()> {
    let admins = load_infra_admins()?;
    let expected_codeowners = generate_codeowners_content(admins);
    let actual_codeowners =
        std::fs::read_to_string(codeowners_path()).context("cannot read CODEOWNERS")?;
    if expected_codeowners != actual_codeowners {
        return Err(anyhow::anyhow!("CODEOWNERS content is not up-to-date. Regenerate it using `cargo run ci generate-codeowners`."));
    }

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
        .into_iter()
        .map(|admin| format!("@{admin}"))
        .collect::<Vec<_>>()
        .join(" ");

    // Set of paths that should only be modifiable by infra-admins
    let secure_paths = &[
        "/.github/",
        "/src/",
        "/rust_team_data/",
        "/repos/rust-lang/team.toml",
        "/repos/rust-lang/sync-team.toml",
        "/teams/infra-admins.toml",
        "/teams/team-repo-admins.toml",
        ".cargo",
        "target",
        "Cargo.lock",
        "Cargo.toml",
        "config.toml",
        "infra-admins.txt",
    ];
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
    let admins =
        std::fs::read_to_string(Path::new(&env!("CARGO_MANIFEST_DIR")).join("infra-admins.txt"))
            .context("cannot load infra-admins.txt")?;
    Ok(admins.lines().map(|s| s.trim().to_string()).collect())
}
