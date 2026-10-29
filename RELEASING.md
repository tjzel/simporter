# Releasing simporter

## One-time setup

The repo is initialised, committed and pointed at `https://github.com/tjzel/simporter`, which is
public and still empty. What is left is the first push:

```sh
git push -u origin main
```

Then get a crates.io token with the `publish-new` and `publish-update` scopes from
<https://crates.io/settings/tokens>, and run `cargo login`.

The name `simporter` is unclaimed on crates.io. The first `cargo publish` claims it.

## Every release

1. Bump `version` in `Cargo.toml`, commit.
2. `cargo publish`
3. Tag and push: `git tag v<X.Y.Z> && git push && git push --tags`

`cargo publish` builds the packed crate from a clean copy before it uploads, so it catches a file
that is on disk but missing from the package.

## Before you publish

```sh
cargo fmt --check
cargo lint
cargo test
cargo package --list     # 25 files; agent-rule.md and agent-skill.md must be among them
cargo publish --dry-run
```

`src/rule.rs` reads `agent-rule.md` and `agent-skill.md` with `include_str!`, so the crate does not
compile if either file is left out of the package. There is no `include` list: everything ships
except hidden paths, which cargo skips on its own, and the two entries in `exclude`. A new asset is
therefore packaged by default rather than silently dropped. Check `cargo package --list` anyway
when you add a file outside `src/`.

## What a published version cannot do

A version is permanent. `cargo yank --version <X.Y.Z>` stops new dependants resolving to it but
does not delete it. Pick the version number carefully.
