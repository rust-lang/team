# Automated tests

The tool developed in this repository is the cornerstone of the Rust Team's
access control, and a bug in it could result in unauthorized people gaining
access to protected resources and systems. To avoid that, the repository
employs automated tests to discover regressions quickly.

You can run all the tests with the command:

```
cargo test
```

## Static API test

The Static API test ensures the output of the Static API for a dummy repository
is what we expect. This test uses the snapshot testing technique, and the
expected copy is a build of the Static API stored in the git repository.

To add something to the Static API test make the changes you need to the files
in `tests/static-api` direcory: its layout is the same as the top-level
contents. Once you made the changes, run `cargo test` to preview the diff. If
the diff is what you expect, run the following command to mark it as the
expected one:

```
tests/bless.sh
```
