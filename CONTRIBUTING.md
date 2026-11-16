# Contributing

## Branches

Alloy has two long-lived branches.

- `upstream` is where the language changes. Open a pull request against
  it. CI runs on each pull request and on each push to the branch.
- `main` holds the releases. Nothing lands there directly except a sync
  from `upstream` and a fix that cannot wait for one.

A release is a pull request from `upstream` into `main`, with the title
`Sync to upstream/release/<version>`. Merge it with a merge commit, not
a squash, so the two branches keep one history. Then tag the merge
commit `v<version>`. The tag starts the release workflow.

A fix that lands on `main` first goes back to `upstream` with a merge of
`main` into it.

## Feature flags

A feature that is not final lands on `upstream` behind a flag, off by
default. The branch can then release at any time, with the feature
unfinished.

The name of a flag starts with `Alloy`, as a Luau flag starts with
`Luau`: `AlloyNativeIfLocal`, `AlloyExperimentalPipeOperator`.

1. Add a `Flag` to `crates/alloy/src/fflags.rs` and name it in `FLAGS`.
2. Add its key to the `fflags` table in `crates/alloy/src/schema.rs`. A
   test fails when the two lists differ.
3. Read it where the feature starts: `options.fflags.on(&fflags::NAME)`.
   With the flag off, the output must not change.
4. Test the feature with the flag on, and one case with it off.

A project sets flags in `[fflags]` of alloy.toml or `.config.aly`. The
table holds Luau flags too. The prefix tells them apart: an `Alloy` name
stays with the compiler, and any other name goes to luau-lsp.

```toml
[fflags]
AlloyNativeIfLocal = true
LuauSolverV2 = true
```

The editor has one list of its own, `alloy-luau.fflags.override` in
VS Code, which goes over the list of the project.

When the feature is final, make it the default and delete the flag. A
configuration that still names a deleted flag gets a warning, so an old
file keeps loading.

## Checks

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
