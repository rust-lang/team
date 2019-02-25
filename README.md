# Rust teams structure

This repository contains the structure of the Rust teams. The repository is
automatically synchronized with:

| Service | Synchronized every | |
| --- | :---: | --- |
| [Crater and @craterbot][crater] | *In real time* | [Integration source][crater-src] |
| Mailing lists and aliases (`@rust-lang.org`, `@crates.io`) | 5 minutes | [Integration source][ml-src]
| [Governance section on the website][www] | 2 minutes | [Integration source][www-src] |

If you need to add or remove a person from a team send a PR to this repository,
and after it's merged their account will be added/removed from all the
supported services.

[www]: https://www.rust-lang.org/governance
[www-src]: https://github.com/rust-lang/www.rust-lang.org/blob/master/src/teams.rs
[crater]: https://github.com/rust-lang-nursery/crater
[crater-src]: https://github.com/rust-lang-nursery/crater/blob/master/src/server/auth.rs
[ml-src]: https://github.com/rust-lang/rust-central-station/tree/master/sync-mailgun

## Documentation

* [TOML schema reference](docs/toml-schema.md)

## Using the CLI tool

It's possible to interact with this repository through its CLI tool.

### Verifying the integrity of the repository

This repository contains some sanity checks to avoid having stale or broken
data. You can run the checks locally with the `check` command:

```
$ cargo run check
```

### Querying information out of the repository

There are a few CLI commands that allow you to get some information generated
from the data in the repository.

You can get a list of all the people in a team:

```
$ cargo run dump-team all
```

You can get a list of all the email addresses subscribed to a list:

```
$ cargo run dump-list all@rust-lang.org
```

You can get a list of all the users with a permission:

```
$ cargo run dump-permission perf
```

### Building the static API

You can build locally the content of `https://team-api.infra.rust-lang.org/v1/`
by running the command:

```
$ cargo run static-api output-dir/
```

The content will be placed in `output-dir/`.
