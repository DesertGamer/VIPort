//! Agility SDK opt-in.
//!
//! Windows 10's in-box D3D12 runtime tops out at shader model 6.5, and this crate emits
//! SM 6.6 (dynamic resources, `WaveSize`, compute derivatives). The Agility SDK loader in
//! `d3d12.dll` switches to a redistributable `D3D12Core.dll` when the host module exports
//! the two symbols below; `D3D12SDKPath` names the folder holding it, relative to the
//! executable, and `D3D12SDKVersion` must match the `D3D12_SDK_VERSION` in the SDK's
//! `d3d12.h` (620 for package 1.620.2).
//!
//! `build.rs` sets `cfg(agility_runtime)` once the runtime is staged next to the test
//! binaries, and everything here hangs off that: exporting the symbols also makes the
//! loader refuse to create a device when the DLL is missing (`0x887e0003`), so a build
//! without the staged runtime stays opted out instead of exporting and failing.
//!
//! The exports come from the `.drectve` section at the end of this module, not from a
//! `cargo:rustc-link-arg` in `build.rs`: the directive travels with the object file that
//! defines the symbols, so no other target of the package is asked to export them.

#[cfg(agility_runtime)]
#[allow(dead_code)]
#[unsafe(no_mangle)]
pub static D3D12SDKVersion: u32 = 620;

/// A `const char*`, not a char array: the loader dereferences this symbol.
#[cfg(agility_runtime)]
#[allow(dead_code)]
#[unsafe(no_mangle)]
pub static D3D12SDKPath: &[u8; 9] = b".\\D3D12\\\0";

/// The linker reads `.drectve` sections out of object files as extra command-line
/// directives, which puts both `/EXPORT` options in the one object that defines the
/// symbols. They are `DATA` symbols; exporting the version alone is not enough for the
/// loader to switch runtimes.
#[cfg(agility_runtime)]
#[used]
#[unsafe(link_section = ".drectve")]
static EXPORT_SDK_SELECTORS: [u8; 54] =
    *b"/EXPORT:D3D12SDKVersion,DATA /EXPORT:D3D12SDKPath,DATA";
