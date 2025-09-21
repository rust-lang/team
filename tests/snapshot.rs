use anyhow::{bail, Error};
use duct::{cmd, Expression};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

#[test]
fn static_api() -> Result<(), Error> {
    let dir_output = dir_valid().join("_output");
    let dir_expected = dir_valid().join("_expected");

    if dir_output.exists() {
        std::fs::remove_dir_all(&dir_output)?;
    }

    step("checking whether the data is valid");
    cmd!(bin(), "check", "--skip", "validate_github_usernames")
        .dir(dir_valid())
        .assert_success()?;

    step("generating the static api contents");
    cmd!(bin(), "static-api", &dir_output)
        .dir(dir_valid())
        .assert_success()?;

    step("checking whether the output matched the expected one");

    // Collect all the files present in either the output or expected dirs
    let mut files = HashSet::new();
    for dir in &[&dir_output, &dir_expected] {
        for entry in walkdir::WalkDir::new(dir) {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }
            files.insert(entry.path().strip_prefix(dir)?.to_path_buf());
        }
    }

    // Check whether any file is different
    let mut failed = false;
    for file in &files {
        let expected = std::fs::read_to_string(dir_expected.join(file))
            .ok()
            .unwrap_or_default();
        let output = std::fs::read_to_string(dir_output.join(file))
            .ok()
            .unwrap_or_default();

        let changeset = difference::Changeset::new(&expected, &output, "\n");
        if changeset.distance != 0 {
            failed = true;
            println!(
                "{} {} {}",
                ansi_term::Color::Red.bold().paint("!!! the file"),
                ansi_term::Color::White
                    .bold()
                    .paint(file.to_str().unwrap().to_string()),
                ansi_term::Color::Red.bold().paint("does not match"),
            );
            println!("{changeset}");
        }
    }
    if failed {
        println!(
            "{} {}",
            ansi_term::Color::Cyan
                .bold()
                .paint("==> If you believe the new content is right, run:"),
            ansi_term::Color::White.bold().paint("tests/bless.sh")
        );
        println!();
        panic!("there were differences in the expected output");
    }

    Ok(())
}

#[test]
fn static_api_determinism() -> Result<(), Error> {
    // Ensure that the output of `static-api` is deterministic
    let dir = tempfile::TempDir::new()?;
    let reference_dir = dir.path().join("reference");
    cmd!(bin(), "static-api", &reference_dir).assert_success()?;

    for i in 0..10 {
        let out = dir.path().join(format!("output-{i}"));
        cmd!(bin(), "static-api", &out).assert_success()?;
        assert!(
            !dir_diff::is_different(&reference_dir, &out)?,
            "static-api produced non-deterministic output"
        );
    }
    Ok(())
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_rust-team")
}

fn dir_valid() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("static-api")
}

fn step(name: &str) {
    println!(
        "{}",
        ansi_term::Color::White.bold().paint(format!("==> {name}"))
    );
}

trait ExpressionExt {
    fn assert_success(self) -> Result<(), Error>;
}

impl ExpressionExt for Expression {
    fn assert_success(mut self) -> Result<(), Error> {
        // If the environment variable is not passed colors will never be shown.
        if atty::is(atty::Stream::Stdout) {
            self = self.env("RUST_TEAM_FORCE_COLORS", "1");
        }

        // Redirects stderr to stdout to be able to print the output in the correct order.
        let res = self.stderr_to_stdout().stdout_capture().unchecked().run()?;
        print!("{}", String::from_utf8_lossy(&res.stdout));

        if !res.status.success() {
            bail!("command returned a non-zero exit code!");
        }
        Ok(())
    }
}
