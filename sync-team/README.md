# sync-github

This CLI tool synchronizes our GitHub organizations with the [team repo][team].

To run the tool you need to have the `GITHUB_TOKEN` environment variable set
with a personal access token with owner access to all the synchronized orgs,
and then you can run:

```
$ cargo run
```

That command will execute a **dry run**, without actually applying any changes
to GitHub. Once you're satisfied with the changes you can synchronize the live
data by passing the `--live` flag:

```
$ cargo run -- --live
```

By default the tool will run on the production team API. If you want to run it
against a local instance you can set the `TEAM_DATA_BASE_URL` environment
variable to point to it.

[team]: https://github.com/rust-lang/team
