//! Translated shaders kept on disk (decision 0003): under the configured
//! cache folder, keyed by the container's content and by what the
//! translation depends on ([`build`]), so a changed translator never serves
//! an old translation, while rebuilding the rest of VIPort keeps them.
//! Each translator's entries are in a folder of their own, named by the
//! start of that key: older translators' folders can be deleted whole.
//! Entries go through `viport-pipeline`'s content-addressed cache, which
//! writes them atomically; an entry that does not read back is translated
//! again. A container that does not translate is kept too, with the reason,
//! so it is not tried again on every run.
//!
//! `viport translate --cache` fills the same cache ahead of a run, through
//! [`host_shader`].
//!
//! The entry is our own encoding of [`HostShader`]; this file's text is part
//! of the key, so changing the encoding invalidates every entry.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use viport_pipeline::{Cache, Converter};
use viport_shader::emit::{HostShader, StaticSampler, UserData};
use viport_shader::{HwStage, ResourceType, RootType, UsedResource};

/// A translated shader and the descriptor ranges its console program reads.
#[derive(Debug, PartialEq)]
pub(crate) struct Translated {
    pub(crate) host: HostShader,
    pub(crate) used: Vec<UsedResource>,
}

static CACHE: OnceLock<Cache> = OnceLock::new();
static HITS: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);

/// Keeps translated shaders in a folder of `dir` from now on: the folder
/// of this translator, `None` if it cannot be opened.
pub fn use_shader_cache(dir: PathBuf) -> Option<PathBuf> {
    let dir = dir.join(&blake3::Hash::from(*build()).to_hex()[..16]);
    match Cache::open(&dir) {
        Ok(cache) => {
            tracing::info!(dir = %dir.display(), "shader cache");
            let _ = CACHE.set(cache);
            Some(dir)
        }
        Err(e) => {
            tracing::warn!(error = %e, "no shader cache");
            None
        }
    }
}

/// Shaders found in the cache and translated since the start.
pub fn counts() -> (u64, u64) {
    (HITS.load(Ordering::Relaxed), MISSES.load(Ordering::Relaxed))
}

/// What a translation depends on besides its container: the translator
/// (`viport_shader::SOURCE_HASH`: its sources, manifest, the lock file and
/// the compiler), the entry's encoding (this file's text) and the
/// translation settings (the loop limit, which a diagnostics build can
/// change at run time).
fn build() -> &'static [u8; 32] {
    static BUILD: OnceLock<[u8; 32]> = OnceLock::new();
    BUILD.get_or_init(|| {
        let limit = viport_shader::emit::loop_limit();
        if limit != viport_shader::emit::DEFAULT_LOOP_LIMIT {
            tracing::warn!(
                limit,
                "VIPORT_SHADER_LOOP_LIMIT: translated loops stop sooner"
            );
        }
        key(
            viport_shader::SOURCE_HASH,
            ENCODING,
            limit,
            viport_shader::emit::shader_opt(),
            viport_shader::emit::mesh64_on_32(),
            viport_shader::emit::wave64_on_32(),
        )
    })
}

/// This file, whose `encode` and `decode` define the entries.
const ENCODING: &[u8] = include_bytes!("shader_cache.rs");

fn key(
    translator: &str,
    encoding: &[u8],
    limit: u32,
    passes: u32,
    mesh64_on_32: bool,
    wave64_on_32: bool,
) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(translator.as_bytes());
    h.update(blake3::hash(encoding).as_bytes());
    h.update(&limit.to_le_bytes());
    h.update(&passes.to_le_bytes());
    if mesh64_on_32 {
        h.update(b"mesh64_on_32");
    }
    if wave64_on_32 {
        h.update(b"wave64_on_32");
    }
    *h.finalize().as_bytes()
}

/// The translation of a console shader container, from the cache when
/// there is one, and whether it came from there.
pub fn host_shader(container: &[u8]) -> (Result<HostShader, String>, bool) {
    let (result, hit) = translation(container);
    (result.map(|t| t.host), hit)
}

pub(crate) fn translation(container: &[u8]) -> (Result<Translated, String>, bool) {
    let translate = |c: &[u8]| {
        viport_shader::emit::translate_with_resources(c)
            .map(|(host, used)| Translated { host, used })
            .map_err(|e| e.to_string())
    };
    match CACHE.get() {
        Some(cache) => cached(cache, build(), container, translate),
        None => (translate(container), false),
    }
}

/// The cache entry's converter: the build and the container in, the
/// encoded translation (or refusal) out. `ran` tells a miss from a hit.
struct Translate<F> {
    translate: F,
    ran: AtomicBool,
}

impl<F: Fn(&[u8]) -> Result<Translated, String> + Send + Sync> Converter for Translate<F> {
    fn id(&self) -> &'static str {
        "shader.dxil"
    }
    fn version(&self) -> u32 {
        // The build fingerprint in the input does the versioning.
        1
    }
    fn convert(&self, input: &[u8]) -> anyhow::Result<Vec<u8>> {
        self.ran.store(true, Ordering::Relaxed);
        Ok(encode(&(self.translate)(&input[32..])))
    }
}

fn cached(
    cache: &Cache,
    build: &[u8; 32],
    container: &[u8],
    translate: impl Fn(&[u8]) -> Result<Translated, String> + Send + Sync,
) -> (Result<Translated, String>, bool) {
    let input = [&build[..], container].concat();
    let converter = Translate {
        translate,
        ran: AtomicBool::new(false),
    };
    for _ in 0..2 {
        let path = match cache.get_or_convert(&converter, &input) {
            Ok(path) => path,
            Err(e) => {
                tracing::warn!(error = format!("{e:#}"), "shader cache entry not written");
                break;
            }
        };
        if let Some(shader) = std::fs::read(&path).ok().and_then(|b| decode(&b)) {
            let hit = !converter.ran.load(Ordering::Relaxed);
            let (counter, what) = if hit {
                (&HITS, "from the cache")
            } else {
                (&MISSES, "translated and cached")
            };
            counter.fetch_add(1, Ordering::Relaxed);
            tracing::debug!(path = %path.display(), "shader {what}");
            return (shader, hit);
        }
        tracing::debug!(path = %path.display(), "unreadable shader cache entry; translating again");
        let _ = std::fs::remove_file(&path);
    }
    MISSES.fetch_add(1, Ordering::Relaxed);
    ((converter.translate)(container), false)
}

const MAGIC: &[u8; 4] = b"VSH2";
/// A container that does not translate: the reason follows.
const REFUSED: &[u8; 4] = b"VSHE";
const STAGES: [HwStage; 8] = [
    HwStage::Ls,
    HwStage::Hs,
    HwStage::Es,
    HwStage::Gs,
    HwStage::Vs,
    HwStage::Ps,
    HwStage::Ms,
    HwStage::Cs,
];

/// The raw root type values, as the console shader stores them. Keep these
/// in step with `RootType::from_raw` in `viport-shader`: a second numbering
/// here used to overflow for `Other(u32::MAX)` and decoded native values
/// into the wrong variant.
fn root_code(r: RootType) -> u32 {
    match r {
        RootType::Constants => 0,
        RootType::Descriptor => 1,
        RootType::Table => 3,
        RootType::ImmediateSampler => 4,
        RootType::ResourceHeap => 5,
        RootType::SamplerHeap => 6,
        RootType::Other(x) => x,
    }
}

fn root_type(code: u32) -> RootType {
    match code {
        0 => RootType::Constants,
        1 => RootType::Descriptor,
        3 => RootType::Table,
        4 => RootType::ImmediateSampler,
        5 => RootType::ResourceHeap,
        6 => RootType::SamplerHeap,
        x => RootType::Other(x),
    }
}

/// Little-endian words and length-prefixed lists; a refusal is its
/// reason's text.
fn encode(translation: &Result<Translated, String>) -> Vec<u8> {
    let s = match translation {
        Ok(s) => s,
        Err(reason) => return [REFUSED.as_slice(), reason.as_bytes()].concat(),
    };
    // Every field by name: a field added to `HostShader` must be added here
    // (and to `decode`).
    let HostShader {
        stage,
        dxil,
        user_data,
        samplers,
        static_samplers,
        root_cbvs,
        root_srvs,
        root_uavs,
        memory,
        memory_writes,
        heap_usage,
        view_addresses,
        heap,
        heap_writes,
    } = &s.host;
    let mut out = MAGIC.to_vec();
    let word = |out: &mut Vec<u8>, v: u32| out.extend(v.to_le_bytes());
    let stage = STAGES.iter().position(|x| x == stage).unwrap_or(0) as u32;
    word(&mut out, stage);
    word(&mut out, dxil.len() as u32);
    out.extend(dxil);
    word(&mut out, user_data.len() as u32);
    for u in user_data {
        for v in [u.index, u32::from(u.slot), root_code(u.root), u.dword] {
            word(&mut out, v);
        }
    }
    word(&mut out, samplers.len() as u32);
    for sampler in samplers {
        for &v in sampler {
            word(&mut out, v);
        }
    }
    word(&mut out, static_samplers.len() as u32);
    for t in static_samplers {
        let [u, v, w] = t.address;
        let words = [
            t.filter,
            u,
            v,
            w,
            t.mip_lod_bias.to_bits(),
            t.max_anisotropy,
            t.comparison,
            t.border,
            t.min_lod.to_bits(),
            t.max_lod.to_bits(),
        ];
        for v in words {
            word(&mut out, v);
        }
    }
    for slots in [root_cbvs, root_srvs, root_uavs] {
        word(&mut out, slots.len() as u32);
        out.extend(slots);
    }
    word(
        &mut out,
        u32::from(*memory)
            | u32::from(*memory_writes) << 1
            | u32::from(*view_addresses) << 2
            | u32::from(*heap) << 3
            | u32::from(*heap_writes) << 4
            | u32::from(*heap_usage) << 5,
    );
    word(&mut out, s.used.len() as u32);
    for u in &s.used {
        let kind = match u.kind {
            ResourceType::Srv => 0,
            ResourceType::Sampler => 1,
            ResourceType::Cbv => 2,
            ResourceType::Uav => 3,
            ResourceType::Other(n) => 256 + u32::from(n),
        };
        let (root, raw_root) = match u.root {
            RootType::Other(raw) => (6, raw),
            root => (root_code(root), 0),
        };
        let fields = u32::from_le_bytes([u.dwords, u.user_data, u.slot, 0]);
        for v in [
            kind,
            root,
            raw_root,
            u.table_offset_dwords,
            u.lower_bound,
            u.stride,
            fields,
        ] {
            word(&mut out, v);
        }
    }
    out
}

fn decode(bytes: &[u8]) -> Option<Result<Translated, String>> {
    struct Reader<'a>(&'a [u8]);
    impl<'a> Reader<'a> {
        fn take(&mut self, n: usize) -> Option<&'a [u8]> {
            let (head, rest) = self.0.split_at_checked(n)?;
            self.0 = rest;
            Some(head)
        }
        fn word(&mut self) -> Option<u32> {
            Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
        }
        fn list(&mut self) -> Option<Vec<u8>> {
            let n = self.word()? as usize;
            Some(self.take(n)?.to_vec())
        }
    }
    if let Some(reason) = bytes.strip_prefix(REFUSED) {
        return Some(Err(String::from_utf8(reason.to_vec()).ok()?));
    }
    let mut r = Reader(bytes);
    if r.take(4)? != MAGIC {
        return None;
    }
    let stage = *STAGES.get(r.word()? as usize)?;
    let dxil = r.list()?;
    let n = r.word()? as usize;
    let mut user_data = Vec::with_capacity(n.min(256));
    for _ in 0..n {
        user_data.push(UserData {
            index: r.word()?,
            slot: u8::try_from(r.word()?).ok()?,
            root: root_type(r.word()?),
            dword: r.word()?,
        });
    }
    let n = r.word()? as usize;
    let mut samplers = Vec::with_capacity(n.min(256));
    for _ in 0..n {
        samplers.push([r.word()?, r.word()?, r.word()?, r.word()?]);
    }
    let n = r.word()? as usize;
    let mut static_samplers = Vec::with_capacity(n.min(256));
    for _ in 0..n {
        static_samplers.push(StaticSampler {
            filter: r.word()?,
            address: [r.word()?, r.word()?, r.word()?],
            mip_lod_bias: f32::from_bits(r.word()?),
            max_anisotropy: r.word()?,
            comparison: r.word()?,
            border: r.word()?,
            min_lod: f32::from_bits(r.word()?),
            max_lod: f32::from_bits(r.word()?),
        });
    }
    let root_cbvs = r.list()?;
    let root_srvs = r.list()?;
    let root_uavs = r.list()?;
    let memory = r.word()?;
    const RESOURCE_BYTES: usize = 7 * size_of::<u32>();
    let n = r.word()? as usize;
    if r.0.len() != n.checked_mul(RESOURCE_BYTES)? {
        return None;
    }
    let mut used = Vec::with_capacity(n.min(256));
    for _ in 0..n {
        let kind = match r.word()? {
            0 => ResourceType::Srv,
            1 => ResourceType::Sampler,
            2 => ResourceType::Cbv,
            3 => ResourceType::Uav,
            n @ 256..=511 => ResourceType::Other((n - 256) as u8),
            _ => return None,
        };
        let root = match (r.word()?, r.word()?) {
            (n @ 0..=5, 0) => root_type(n),
            (6, raw) => RootType::Other(raw),
            _ => return None,
        };
        let table_offset_dwords = r.word()?;
        let lower_bound = r.word()?;
        let stride = r.word()?;
        let [dwords, user_data, slot, reserved] = r.word()?.to_le_bytes();
        if reserved != 0 {
            return None;
        }
        used.push(UsedResource {
            kind,
            root,
            table_offset_dwords,
            lower_bound,
            stride,
            dwords,
            user_data,
            slot,
        });
    }
    let host = HostShader {
        stage,
        dxil,
        user_data,
        samplers,
        static_samplers,
        root_cbvs,
        root_srvs,
        root_uavs,
        memory: memory & 1 != 0,
        memory_writes: memory & 2 != 0,
        heap_usage: memory & 32 != 0,
        view_addresses: memory & 4 != 0,
        heap: memory & 8 != 0,
        heap_writes: memory & 16 != 0,
    };
    Some(Ok(Translated { host, used }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shader() -> Translated {
        let host = HostShader {
            stage: HwStage::Ps,
            dxil: b"DXBC and the rest".to_vec(),
            user_data: vec![UserData {
                index: 8,
                slot: 1,
                root: RootType::Table,
                dword: 0,
            }],
            samplers: vec![[0x400, 0xc0_0000, 0x18f0_0000, 0]],
            static_samplers: vec![StaticSampler::from_words([
                0x400,
                0xc0_0000,
                0x18f0_0000,
                0,
            ])],
            root_cbvs: vec![2],
            root_srvs: vec![0, 3],
            root_uavs: vec![4],
            memory: true,
            memory_writes: true,
            heap_usage: true,
            view_addresses: true,
            heap: true,
            heap_writes: true,
        };
        let srv = UsedResource {
            kind: ResourceType::Srv,
            root: RootType::Table,
            table_offset_dwords: 16,
            lower_bound: 2,
            stride: 8,
            dwords: 8,
            user_data: 3,
            slot: 1,
        };
        let used = vec![
            srv,
            UsedResource {
                kind: ResourceType::Other(u8::MAX),
                root: RootType::Other(u32::MAX),
                ..srv
            },
        ];
        Translated { host, used }
    }

    #[test]
    fn translations_come_back_from_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        let build = [7u8; 32];
        let container = b"a console container".as_slice();
        let runs = AtomicU64::new(0);
        let translate = |_: &[u8]| {
            runs.fetch_add(1, Ordering::Relaxed);
            Ok(shader())
        };
        assert_eq!(
            cached(&cache, &build, container, translate),
            (Ok(shader()), false)
        );
        assert_eq!(
            cached(&cache, &build, container, translate),
            (Ok(shader()), true)
        );
        assert_eq!(runs.load(Ordering::Relaxed), 1, "the second came from disk");

        // Another build translates again.
        assert_eq!(
            cached(&cache, &[8; 32], container, translate).0,
            Ok(shader())
        );
        assert_eq!(runs.load(Ordering::Relaxed), 2);

        // A damaged entry is a miss, translated and stored again.
        for e in walk(dir.path()) {
            std::fs::write(e, b"VSH2 broken").unwrap();
        }
        assert_eq!(
            cached(&cache, &build, container, translate),
            (Ok(shader()), false)
        );
        assert_eq!(runs.load(Ordering::Relaxed), 3);
        assert_eq!(
            cached(&cache, &build, container, translate),
            (Ok(shader()), true)
        );
        assert_eq!(runs.load(Ordering::Relaxed), 3);

        // Refusals are stored with their reason, and not tried again.
        let fails = AtomicU64::new(0);
        let fail = |_: &[u8]| {
            fails.fetch_add(1, Ordering::Relaxed);
            Err::<Translated, _>("unsupported".to_owned())
        };
        let refused = (Err("unsupported".to_owned()), false);
        assert_eq!(cached(&cache, &build, b"other", fail), refused);
        assert_eq!(cached(&cache, &build, b"other", fail), (refused.0, true));
        assert_eq!(fails.load(Ordering::Relaxed), 1);
    }

    /// The key's parts: a change to any of them (the translator's sources,
    /// this file's encoding, the loop limit) makes another key, so no entry
    /// written before it is read.
    #[test]
    fn the_key_covers_translator_encoding_and_settings() {
        let k = key("0123456789abcdef", b"encoding", 1 << 20, 31, false, false);
        assert_eq!(k, key("0123456789abcdef", b"encoding", 1 << 20, 31, false, false));
        assert_ne!(k, key("0123456789abcdee", b"encoding", 1 << 20, 31, false, false));
        assert_ne!(k, key("0123456789abcdef", b"encoding.", 1 << 20, 31, false, false));
        assert_ne!(k, key("0123456789abcdef", b"encoding", 1 << 19, 31, false, false));
        assert_ne!(k, key("0123456789abcdef", b"encoding", 1 << 20, 0, false, false));
        assert_ne!(k, key("0123456789abcdef", b"encoding", 1 << 20, 31, true, false));
        assert_ne!(k, key("0123456789abcdef", b"encoding", 1 << 20, 31, false, true));
        assert_ne!(
            key("0123456789abcdef", b"encoding", 1 << 20, 31, true, false),
            key("0123456789abcdef", b"encoding", 1 << 20, 31, false, true)
        );
        assert_eq!(
            *build(),
            key(
                viport_shader::SOURCE_HASH,
                ENCODING,
                viport_shader::emit::loop_limit(),
                viport_shader::emit::shader_opt(),
                viport_shader::emit::mesh64_on_32(),
                viport_shader::emit::wave64_on_32()
            )
        );
        // The encoding part is this file as built.
        assert!(
            std::str::from_utf8(ENCODING)
                .unwrap()
                .contains("fn encode(translation: &Result<Translated, String>)")
        );
    }

    /// The cache must use the same numbers as the console shader: a second
    /// numbering lost `Other(2)` and overflowed on large unknown values.
    #[test]
    fn root_types_round_trip_native_codes() {
        for (root, code) in [
            (RootType::Constants, 0),
            (RootType::Descriptor, 1),
            (RootType::Other(2), 2),
            (RootType::Table, 3),
            (RootType::ImmediateSampler, 4),
            (RootType::ResourceHeap, 5),
            (RootType::SamplerHeap, 6),
            (RootType::Other(u32::MAX), u32::MAX),
        ] {
            assert_eq!(root_code(root), code, "{root:?}");
            assert_eq!(root_type(code), root, "{code:#x}");
        }
    }

    fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            if e.path().is_dir() {
                files.extend(walk(&e.path()));
            } else {
                files.push(e.path());
            }
        }
        files
    }

    #[test]
    fn entries_round_trip_and_reject_damage() {
        let bytes = encode(&Ok(shader()));
        assert_eq!(decode(&bytes), Some(Ok(shader())));
        assert_eq!(decode(&bytes[..bytes.len() - 1]), None);
        assert_eq!(decode(&[bytes.as_slice(), &[0]].concat()), None);
        let mut legacy = bytes.clone();
        legacy[..4].copy_from_slice(b"VSH1");
        assert_eq!(decode(&legacy), None);
        let mut bad_kind = bytes.clone();
        let first_kind = bad_kind.len() - 2 * 7 * size_of::<u32>();
        bad_kind[first_kind] = 4;
        assert_eq!(decode(&bad_kind), None);
        let mut bad_count = bytes.clone();
        bad_count[first_kind - 4..first_kind].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(decode(&bad_count), None);
        let refusal = Err("GFX10 code at 0x10: x is not supported".to_owned());
        assert_eq!(decode(&encode(&refusal)), Some(refusal));
        assert_eq!(decode(&[b"VSHE".as_slice(), &[0xff]].concat()), None);
    }
}
