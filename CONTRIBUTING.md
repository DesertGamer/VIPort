# Contributing to VIPort

Thanks for helping. VIPort is built on the VIctory codebase by
[Marvisak](https://github.com/Marvisak/VIctory) and its contributors: it
collects community changes in one tree that builds and passes its tests. This
page says what we accept and how a change gets in.

## Rule 1: nothing from the game, ever

No game files, no extracts, no screenshots or captures of the game, no shader
dumps, no decompiled or disassembled code, no saves. This holds for code,
tests, fixtures, docs, issues and pull requests alike.
Read [docs/asset-policy.md](docs/asset-policy.md); CI and the commit hook
enforce it.

Do not attach logs or captures to an issue without reading them first. A crash
report may contain paths and names from your machine; remove what you do not
want public.

## Set up

Requirements are in the [README](README.md): Windows 10/11 x64, a CPU with AVX2
and a Direct3D 12 GPU. Rust is pinned in `rust-toolchain.toml`.

```powershell
cargo xtask install-hooks
copy viport.example.toml viport.toml
cargo test --workspace
cargo clippy --workspace --all-targets --locked -- -D warnings
```

The tree is not fully `rustfmt`-clean and CI does not check it. Format the
code you write (`rustfmt.toml` is in the repository) but do not reformat files
or lines you did not change.

`cargo test --workspace` needs no game data. `viport.toml` is ignored by git;
keep every path in it outside the repository.

## What a good pull request looks like

- **One topic.** One fix or one feature. A change that touches several
  unrelated things is hard to review and is the first to be dropped when it
  conflicts with something else.
- **Small and focused.** Do not reformat files you did not change.
- **With a test** when the behaviour can be tested without game data
  (synthetic input is welcome). Say in the description what you ran and what
  you saw; say what you could not run.
- **Say what it overlaps.** If your change touches code that other open pull
  requests also change, list them. Two good changes to the same function often
  contradict each other, and we would rather know before merging.
- **Explain the why.** What breaks without the change, and how you know.

## What needs a conversation first

Open an issue before sending a pull request that:

- adds or updates a dependency;
- adds or changes `build.rs`, a git hook, `.cargo/config.toml`, a CI workflow,
  or any script (`.sh`, `.ps1`, `.bat`, `.cmd`);
- downloads anything, starts another program, or opens a network connection;
- adds any binary file or archive (these are not accepted except the one
  exception listed in the asset policy).

These are the places where a code change can do more than it says, so each is
read line by line, and many are declined.

## How a pull request reaches VIPort

1. It is looked at for the things above: file types, scripts, dependencies,
   network and process use. Anything unusual gets a manual read of the diff.
2. It is merged into a working branch in a batch with others.
3. `cargo test --workspace` runs after each batch. If a batch fails, the
   cause is found and fixed or removed before anything else is merged.
4. When two changes are each fine but contradict each other, one is kept and
   the other is recorded as dropped, with the reason in the commit that removes
   it. A dropped change is not a rejection of its idea: reopen it rebased on
   the current tree.

Authorship is kept: merged commits stay under their authors' names.

## Upstream

A fix that is useful to everyone should also go to the original project. If
your change does not depend on anything VIPort-specific, send it there too.

## Conduct

See [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md). Security problems go through
[SECURITY.md](SECURITY.md), not a public issue.
# Contributing to VIPort

You can help with source code, synthetic tests, documentation, or reproducible
compatibility reports. You do not need game files for the source and test
workflow below.

## Pick a task

Check the open [issues](https://github.com/Marvisak/VIctory/issues) and
[pull requests](https://github.com/Marvisak/VIctory/pulls) for existing work.
For a concrete open issue, comment with the part you plan to tackle before
starting. Keep each PR focused and link the relevant issue.

## Run a small check

Follow the build prerequisites in the [README](README.md). From a clone of
your fork, run the checks that apply to your change:

```powershell
cargo xtask install-hooks
cargo test --locked -p viport-formats
cargo xtask policy
```

The formats suite uses synthetic fixtures and needs no Direct3D device. For
changes in other crates, run their relevant tests and state which checks you
ran in the PR. Run the policy check with the files staged if you have not
committed them yet: `cargo xtask policy --staged`.

Format only the Rust files you touch so unrelated changes stay out of the PR.

## Write a useful report

Include the VIPort commit, build profile, Windows version, CPU, GPU and driver.
Give the command or settings needed to reproduce the issue, the last stage
that worked, and the exact error code if available. Replace personal paths
with placeholders. When comparing two builds, use the same settings and give
both commit IDs.

Read the [asset policy](docs/asset-policy.md) before sharing anything. Use
source or synthetic fixtures. In code, comments, tests, and docs, avoid naming
the console SDK or copying identifiers from its headers. Summarize runtime
observations in text and
keep game files, captures, dumps, and logs that contain proprietary material
out of issues and PRs.
