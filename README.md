# VIctory V2

[![Telegram](https://img.shields.io/badge/Telegram-@desertdev-26A5E4?logo=telegram&logoColor=white)](https://t.me/desertdev)
![Discord](https://img.shields.io/badge/Discord-desert__gamer-5865F2?logo=discord&logoColor=white)
[![E-mail](https://img.shields.io/badge/E--mail-desert__gamer@icloud.com-3693F3?logo=icloud&logoColor=white)](mailto:desert_gamer@icloud.com)
[![Issues](https://img.shields.io/github/issues/DesertGamer/VIctory-V2?logo=github)](https://github.com/DesertGamer/VIctory-V2/issues)

> **This is VIctory V2, an integration fork of
> [Marvisak/VIctory](https://github.com/Marvisak/VIctory).**
>
> VIctory is developed in the open and receives a lot of community pull
> requests (shader lowering, GPU fixes, HLE, formats, tooling). A big queue of
> them builds up faster than one maintainer can review. V2 exists to put those
> contributions together in one tree that builds, passes the tests, and can be
> tried today, without waiting for each one to land upstream.
>
> **V2 is not the official project and is not endorsed by its author.** The
> original lives at the link above; its history is kept here intact, and V2 is
> synchronized from it.

Runs Grand Theft Auto VI (Xbox Series X|S) natively on Windows, from files
the user supplies.

The title's x86-64 code runs unchanged inside our process; VIctory supplies
the platform underneath it and converts the title's data, ahead of time where
it can (shaders and pipelines too, see Getting started) and on first use
otherwise. See [docs/architecture.md](docs/architecture.md).

## What V2 changes

- **Community pull requests, merged.** Open upstream PRs are merged into V2 in
  batches. The authors of the original commits stay the authors in the history.
- **Reviewed before merging.** Every PR is checked first for things that do not
  belong in a code change: binary files and archives, build scripts
  (`build.rs`), git hooks, `.cargo` configuration, shell/PowerShell/batch
  scripts, downloads, and new dependencies. PRs with such content are not taken
  without a manual read of the diff.
- **Tested after every batch.** `cargo test --workspace` runs after each batch.
  If a batch fails, the PR that caused it is found and dealt with before
  anything else is merged.
- **Conflicts are resolved, not hidden.** When two PRs are fine alone but
  contradict each other (they change the same behaviour in different ways), V2
  keeps one and records why in the commit message of the change that removes
  the other.

## What V2 does not do

- It does not contain, and will never contain, any game data. The asset policy
  from upstream applies unchanged: see
  [docs/asset-policy.md](docs/asset-policy.md). You supply your own files.
- It does not promise that the game is playable. See *Status* below.

## Where to talk

- [#1: entering the world fails with a GPU device removed](https://github.com/DesertGamer/VIctory-V2/issues/1): the main blocker; post diagnostics and findings here.
- [#2: what is merged into V2 and what is not](https://github.com/DesertGamer/VIctory-V2/issues/2): the list of merged bundles and of those that still need a hand merge.
- [#3: single upstream pull requests](https://github.com/DesertGamer/VIctory-V2/issues/3): how to take one and how to report a clash.
- [#4: hardware reports](https://github.com/DesertGamer/VIctory-V2/issues/4): how far it gets on your machine.

Outside GitHub: Telegram `desertdev`, Discord `desert_gamer`, e-mail
desert_gamer@icloud.com.

## Contributing, security, upstream

- How to contribute: [CONTRIBUTING.md](CONTRIBUTING.md).
- Found a security problem: [SECURITY.md](SECURITY.md). This code runs inside a
  process with your user's rights, so please read it before reporting in public.
- V2 keeps the upstream repository as a remote and merges its `main` regularly.
  A fix that is useful to everyone belongs upstream too: if you send a change
  to V2, consider sending it to the original project as well.

## Status (2026-11-24)

**Not playable yet.** The title boots, runs its menus at its own 30 fps,
loads the world and shows it: slowly, and with rendering defects.

V2, 2026-11-25, on Windows 11 with an NVIDIA RTX 5070: the title no longer
loses the host GPU at start. Two fixes made that so: textures in formats the
GPU refuses to render to or write (B4G4R4A4, B5G5R5A1) are created without
those uses, and typed unordered views are made only in formats the GPU can
store to. The start sequence, the intro splash included, then runs smoothly.
Trouble begins when the first cutscene starts: it stutters, and a run can end
with the host GPU removed after a single submission that took about 5
seconds, longer than the Windows driver watchdog allows. In a release run the
frame counter read 16 to 30 fps, and 127 shaders were translated at first use
(70 seconds in all, the slowest 13 seconds); a run with the shader cache warm
has not been measured yet. V2 makes no claim that it renders more than
upstream; that needs a measurement under the same conditions.

The tables below are upstream's findings of 2026-11-23; V2 has not re-measured
them.

| Stage | State |
|-------|-------|
| Boot, first-boot screens (accessibility, language, settings, legal) | works, 30 fps |
| Intro video with sound | works |
| Start screen, profile and saves | works |
| Loading into the world (about 50 s) | works |
| The world (first mission's intro) | drawn and shown: streets, buildings, vehicles, characters, the title card and credits; 1-2 s per frame. Surfaces are unlit, a band of distant geometry is missing, characters come out broken in some shots |

Upstream's table was measured on Windows 11 with an AMD RX 6700 XT; V2's
notes above on Windows 11 with an NVIDIA RTX 5070.

> **Warning.** Entering the world on a real GPU has hung it in earlier
> builds, and Windows then resets the display driver (every monitor goes
> black for a moment). The latest runs did not, but it is not ruled out; a
> run can also end on a GPU fault (`crashes.log`). A diagnostics build with
> `VICTORY_D3D12_WARP=1` renders on the software adapter instead (too slow
> to reach the world in minutes).

What each part does today and what it still needs:

| Subsystem | Today | Needs |
|-----------|-------|-------|
| Loader | Runs the title's executable and its DLLs in our process; crash reports name the faulting code and are kept in `crashes.log` | – |
| Runtime services | One signed-in offline user, licences, packages, saves that survive a kill at any step, text entry dialog | Speech is silent; the dialog and relaunch are untested live |
| Network | Offline by design: the title cannot reach the network | – |
| Files and streaming | Archives and streamed data read, zlib and BCPack decoded in parallel | Loading is limited by writes into title memory |
| Input | XInput pads, keyboard, keyboard stand-in pad | Trigger rumble |
| Audio | Menu, video and loading sound; XMA voices and Opus streams decode | Reverb is silent; Opus results unverified against the title |
| GPU: menus | Draws, compute and presentation on host D3D12; only changed title memory is copied | Frame pacing drifts on one screen in release builds |
| GPU: world | The scene is drawn and presented: mesh pipelines, indirect execution, views the shaders pick from the heap themselves, title memory read by address (ray-tracing structures), depth written from compute, pipelines pre-built or made by worker threads and kept on disk | Lighting (surfaces are unlit) and some missing geometry; frames take 1-2 s; draws of untranslated shaders are dropped; real queries, depth bounds, HDR and scaling are missing |
| Shaders | Console GPU code recompiled to DXIL, ahead of time (`translate --cache`) or at first use, cached on disk. Menus: all. World: mesh 8,765 of 8,765, vertex/pixel 12,550 of 12,592, compute 2,579 of 2,637 | Merged geometry stages (195 programs) and about 100 others; fused multiply-adds follow the host GPU; some compute programs need a GPU with 64-lane waves |
| Textures | Console tiled layouts and detiling for the modes the title uses | Render-target metadata planes |
| Series S mode | Runs through the menus to the loading screen | The world, as above |

## Getting started

Requirements: Windows 10/11 x64, a CPU with AVX2, a Direct3D 12 GPU with
shader model 6.6 (and mesh shaders for the world), Rust (pinned in
`rust-toolchain.toml`).

For a dated summary of Windows 10 runtime and GPU feature caveats, see
[Windows and GPU support](docs/windows-and-gpu-support.md).

The title also needs several GiB of available Windows commit at startup.
Check **Committed** under **Task Manager → Performance → Memory**. If
`crashes.log` reports Windows error 1455, close other applications or increase
the Windows paging file size, then retry.

```powershell
cargo xtask install-hooks                # asset-policy check on every commit
copy victory.example.toml victory.toml   # then set your paths (outside the repo)
cargo test --workspace                   # needs no game data
cargo run -p victory -- config           # check your paths
```

Run the title:

```powershell
cargo build --release -p victory
target\release\victory.exe translate --cache --pipelines <game dir>\shaders.rpf <game dir>\<the title's exe> <game dir>\XboxBvhBuilder.dll <game dir>\amd_fidelityfx_upscaler_xbox.dll
target\release\victory.exe run <game dir>\<the title's exe> --execute
```

The `translate --cache` step (minutes; again after a change of the shader
translator) translates every shader the title can ask for into the cache
(about 1.3 GB), so a run does not translate them while it plays; without it
they are translated at first use, and their draws wait. With `--pipelines`
it then makes every pipeline in the title's shader pack (inside
`shaders.rpf`) into the host pipeline library (about 27,000 pipelines, 4 MB;
some 20 minutes on 16 threads, most of it the 2,700 compute ones), so a run
compiles none of those either; the library belongs to the GPU and driver it
was made on. Run again, it skips what is there in seconds; stopped
early, it keeps what it made.

Upscaling (Series S mode): the title upscales with AMD FidelityFX. To run
it with the PC build on your GPU instead of the console build, get the
[FidelityFX SDK](https://github.com/GPUOpen-LibrariesAndSDKs/FidelityFX-SDK)
and point `VICTORY_FIDELITYFX_DIR` (or `[paths].fidelityfx_dir`) at its
`Kits/FidelityFX/signedbin` folder, which holds
`amd_fidelityfx_upscaler_dx12.dll`. Without it, or if that DLL offers no
upscaler for your GPU (the log says so), the title's own DLLs run.

The window caption shows the framerate. Saves, settings and `crashes.log`
are kept under the data folder (default `%LOCALAPPDATA%\victory\data`), derived
data under the cache folder.

Input: XInput pads. With none connected the keyboard is a pad while the
title's window has focus:

| Keys | Pad |
|------|-----|
| W A S D / I J K L | left / right stick |
| arrows | D-pad |
| Enter, Backspace, X, Y | A, B, X, Y |
| Q / E, Z / C | shoulders, triggers |
| F / H | stick clicks |
| Esc, Tab | Menu, View |

## Commands

```
victory inspect <exe|dll> [--imports] [--exports]
victory census [paths...] [--missing] [--strict]
victory lib-exports <import.lib> [--list]
victory shader-info [paths...] [--verbose] [--disasm] [--effects]
victory translate [paths...] [--validate] [--pipelines] [--list] [--jobs N]
victory translate [paths...] --cache [--pipelines] [--jobs N]
victory run <exe> [--execute] [--unimplemented trap|zero] [--strict]
            [--console series-x|series-s] [-- <title arguments>]
victory config
```

Logging: `VICTORY_LOG=victory_loader=trace,info` (a release build logs
info and above).

## Debugging

A release build has no debugging aids. Build them in with

```powershell
cargo build --profile diag -p victory --features diagnostics   # target\diag\victory.exe
```

| Variable | Effect |
|----------|--------|
| `VICTORY_LOG=<target>=debug,...` | debug and trace output |
| `VICTORY_STACKS=20,60` | every thread's stack at those seconds |
| `VICTORY_WATCH=<rva>,...` | logs a title u32 when it changes |
| `VICTORY_POKE=<rva>:<byte>,...` | writes a title byte twice a second (a hang watchdog's tick, say) |
| `VICTORY_KEYS=30:Enter,35:X` | presses keys at those seconds, focus or not |
| `VICTORY_NO_HOST_PAD=1` | ignores XInput pads, so the keyboard drives the stand-in pad |
| `VICTORY_CRASH_SAVE=<rva>:<len>,...` | saves title memory to the cache folder on a crash |
| `VICTORY_CAPTURE=<dir>` | saves presented frames (powers of two, or `VICTORY_CAPTURE_FRAMES=<list>`) |
| `VICTORY_DUMP_TARGETS=1` | with the two above, saves every render target of those frames |
| `VICTORY_CAPTURE_WORLD=<list>` | with `VICTORY_CAPTURE`, also saves (and with `VICTORY_DUMP_TARGETS` dumps) the frames at these offsets from the first frame that ran 500 draws (the world's first; its number depends on how fast the title loaded) |
| `VICTORY_DUMP_WRITERS=<hash>,...` | with the two above, also the textures a pipeline containing a listed shader hash writes, the first frame it runs, and for its buffers a log line (dwords changed, dwords not zero) |
| `VICTORY_PEEK=<hex address>[:<bytes>],...` | in listed frames, those bytes of title memory logged after each submission. Listed frames also log targets of 16 texels or fewer as values, each table view's view words and whether its host slot is null, the host copy of every buffer of 64 bytes or fewer a draw reads (at submission and after its list), and each 1D or 3D texture a dispatch writes, read back right after it |
| `VICTORY_D3D12_DEBUG=1` | host debug layer |
| `VICTORY_D3D12_GBV=1` | with the above, GPU-based validation (bad descriptor use reported, not faulted; slow) |
| `VICTORY_D3D12_DRED=1` | on a GPU failure, the allocation hit and the commands in flight |
| `VICTORY_SKIP_PIPELINES=<hash>,...` | omit draws and dispatches, including indirect ones, whose pipeline contains a listed 16-digit shader hash |
| `VICTORY_GPU_TIMING=1` | each list of a submission run and timed on its own; slow lists are logged, and with `VICTORY_D3D12_DRED=1` their slowest commands are named too — the per-command notes come from that switch, so `VICTORY_GPU_TIMING` alone reports `commands=0`; a device removal names the list it happened in and lists all its commands |
| `VICTORY_TRACE_SHADERS=<hash>,...` | with `VICTORY_D3D12_DRED`, every draw and dispatch of a listed shader logged with what it binds (user data, heap slots, root views and their first dwords; the first 500) |
| `VICTORY_D3D12_WARP=1` | host device on the software adapter (slow; cannot reset the driver) |
| `VICTORY_PIPELINE_LOG=<file>` | a line per title pipeline created (key, shaders, state), first used, and made or loaded (library name); `translate --cache --pipelines` writes the last kind too |
| `VICTORY_PREBUILD_KEYS=<file>` | `translate --cache --pipelines` makes nothing and lists each pack record's key, shaders and state (or why it is skipped) |
| `VICTORY_SHADER_LOOP_LIMIT=<n>` | translated shader loops stop after `n` iterations per lane instead of 2^20 (own shader cache entries); if a lower `n` makes world frames much faster, loops hitting the limit are what makes them slow |
| `VICTORY_AUDIO_CAPTURE=<file.wav>` | records what the title plays (our process only) |
| `VICTORY_XMA_DUMP=<dir>` | saves the first 32 XMA voices' context and input |

Every build appends crashes and host GPU failures to `crashes.log` in the
title's data folder.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
