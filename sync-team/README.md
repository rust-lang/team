# Team synchronization tool

This repository contains the CLI tool used to synchronize the contents of the
[rust-lang/team] repository with some of the services the Rust Team uses. There
is usually no need to run this tool manually, and running it requires elevated
privileges on our infrastructure.

| Service name | Description | Environment variables |
| --- | --- | --- |
| github | Synchronize GitHub team membership | `GITHUB_TOKEN` |
| mailgun | Synchronize mailing lists on Mailgun | `MAILGUN_API_TOKEN`, `EMAIL_ENCRYPTION_KEY`|
| zulip | Synchronize Zulip user groups | `ZULIP_USERNAME`, `ZULIP_API_TOKEN` |

The contents of this repository are available under both the MIT and Apache 2.0
license.

## Running the tool

By default the tool will run in *dry mode* on all the services we synchronize,
meaning that the changes will be previewed on the console output but no actual
change will be applied:

```
cargo run
```

Once you're satisfied with the changes you can run the full synchronization by
passing the `--live` flag:

```
cargo run -- --live
```

You can also limit the services to synchronize on by passing a list of all the
service names you want to sync. For example, to synchronize only GitHub and
Mailgun you can run:

```
cargo run -- github mailgun
cargo run -- github mailgun --live
```

## Using a local copy of the team repository

By default this tool works on the production dataset, pulled from
[rust-lang/team]. When making changes to the tool it might be useful to test
with dummy data though. You can do that by making the changes in a local copy
of the team repository and passing the `--team-repo` flag to the CLI:

```
cargo run -- --team-repo ~/code/rust-lang/team
```

When `--team-repo` is passed, the CLI will build the Static API in a temporary
directory, and fetch the data from it instead of the production instance.

[rust-lang/team]: https://github.com/rust-lang/team
