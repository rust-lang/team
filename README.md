# Rust teams structure

This repository contains the structure of the Rust teams. The repository is
automatically synchronized with:

| Service | Synchronized every | |
| --- | :---: | --- |
| [@bors][bors] | *In real time* | [Integration source][bors-src] |
| [Crater and @craterbot][crater] | *In real time* | [Integration source][crater-src] |
| [Perf and @rust-timer][perf] | *In real time* | [Integration source][perf-src] |
| [@rfcbot][rfcbot] | 5 minutes | [Integration source][rfcbot-src] |
| GitHub teams membership | 5 minutes | [Integration source][github-teams-src] |
| Mailing lists and aliases (`@rust-lang.org`, `@crates.io`) | 5 minutes | [Integration source][ml-src] |
| [Governance section on the website][www] | 2 minutes | [Integration source][www-src] |

If you need to add or remove a person from a team send a PR to this repository,
and after it's merged their account will be added/removed from all the
supported services.

[bors]: https://buildbot2.rust-lang.org/homu
[bors-src]: https://github.com/rust-lang/homu/blob/master/homu/auth.py
[www]: https://www.rust-lang.org/governance
[www-src]: https://github.com/rust-lang/www.rust-lang.org/blob/master/src/teams.rs
[crater]: https://github.com/rust-lang-nursery/crater
[crater-src]: https://github.com/rust-lang-nursery/crater/blob/master/src/server/auth.rs
[ml-src]: https://github.com/rust-lang/rust-central-station/tree/master/sync-mailgun
[perf]: https://perf.rust-lang.org
[perf-src]: https://github.com/rust-lang-nursery/rustc-perf/blob/master/site/src/server.rs
[rfcbot]: https://rfcbot.rs
[rfcbot-src]: https://github.com/anp/rfcbot-rs/blob/master/src/teams.rs
[github-teams-src]: https://github.com/rust-lang/rust-central-station/tree/master/sync-github

## Documentation

* [TOML schema reference](docs/toml-schema.md)

## Using the CLI tool

It's possible to interact with this repository through its CLI tool.

### Verifying the integrity of the repository

This repository contains some sanity checks to avoid having stale or broken
data. You can run the checks locally with the `check` command:

```
cargo run check
```

### Adding a person to the repository

It's possible to fetch the public information present in a GitHub profile and
store it in a person's TOML file. To do that you need to have the
`GITHUB_TOKEN` environment variable setup with a valid personal access token,
and you need to run the command:

```
cargo run add-person <username>
```

### Querying information out of the repository

There are a few CLI commands that allow you to get some information generated
from the data in the repository.

You can get a list of all the people in a team:

```
cargo run dump-team all
```

You can get a list of all the email addresses subscribed to a list:

```
cargo run dump-list all@rust-lang.org
```

You can get a list of all the users with a permission:

```
cargo run dump-permission perf
```


You can generate [www.rust-lang.org](https://github.com/rust-lang/www.rust-lang.org)'s locales/en-US/tools.ftl file by running

```
cargo run dump-website
```

The website will automatically load new teams added here, however they cannot be translated unless `tools.ftl` is also updated.

### Building the static API

You can build locally the content of `https://team-api.infra.rust-lang.org/v1/`
by running the command:

```
cargo run static-api output-dir/
```

The content will be placed in `output-dir/`.
