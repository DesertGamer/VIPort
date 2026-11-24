# Security

VIctory runs a title's machine code inside its own process, with the rights of
the user who started it, and it loads libraries, reads and writes files and
talks to the GPU driver on that user's behalf. A mistake or a hostile change
here is more serious than in an ordinary application.

## Report a problem privately

Use **Report a vulnerability** on the repository's *Security* tab
(<https://github.com/DesertGamer/VIctory-V2/security/advisories/new>).
Please do not open a public issue or pull request for it.

If that is not possible, write to desert_gamer@icloud.com. Do not put details
of the problem in a public chat; ask on Telegram (`desertdev`) or Discord
(`desert_gamer`) for a private conversation instead.

Useful to include: what you found, where (file and line), how to trigger it,
and what an attacker would gain.

## What counts

- Code that does more than its description says: network access, starting
  programs, reading or writing outside the configured folders, persistence,
  downloads.
- A way for a crafted game file, save, cache entry or configuration to run
  code or write outside its folders.
- A binary, archive or script in the tree that is not covered by the asset
  policy's single exception.
- A pull request that hides any of the above.

Crashes and rendering faults of the running title are ordinary bugs, not
security problems: use the bug report form.

## For reviewers

Read these line by line, whatever the pull request says it does: `build.rs`,
git hooks, `.cargo/config.toml`, CI workflows, scripts, dependency changes
(`Cargo.toml`, `Cargo.lock`), anything using `std::process`, sockets or HTTP,
and any binary file. Prefer declining to guessing.
