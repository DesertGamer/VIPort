use super::host::{Dispatch, Host, Root, View};
use super::{HostShader, lower, validator, with_wave64_on_32};
use crate::native::{
    ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource,
};
use crate::{ShaderError, io, ir, resource, structure};

const THREADS: u32 = 64;
const BASE: u32 = 0x1234_0000;
const S_ENDPGM: u32 = 0xBF81_0000;
const EXEC: u32 = 126;

const fn int(n: u16) -> u16 {
    128 + n
}

const fn vgpr(n: u16) -> u16 {
    256 + n
}

const fn sop1(sdst: u32, op: u32, ssrc0: u32) -> u32 {
    0xBE80_0000 | sdst << 16 | op << 8 | ssrc0
}

const fn vop1(op: u32, vdst: u32, src0: u16) -> u32 {
    0x7E00_0000 | vdst << 17 | op << 9 | src0 as u32
}

const fn vop2(op: u32, vdst: u32, src0: u16, vsrc1: u32) -> u32 {
    op << 25 | vdst << 17 | vsrc1 << 9 | src0 as u32
}

const fn vop3(op: u32, vdst: u32, src: [u16; 3]) -> [u32; 2] {
    [
        0xD400_0000 | op << 16 | vdst,
        src[0] as u32 | (src[1] as u32) << 9 | (src[2] as u32) << 18,
    ]
}

const fn ds(op: u32, vdst: u32, addr: u32, data0: u32) -> [u32; 2] {
    [0x36 << 26 | op << 18, vdst << 24 | data0 << 8 | addr]
}

const V_MOV_B32: u32 = 0x01;
const V_ADD_NC_U32: u32 = 0x25;
const V_AND_B32: u32 = 0x1B;
const V_XOR_B32: u32 = 0x1D;
const V_LSHLREV_B32: u32 = 0x1A;
const V_READLANE_B32: u32 = 0x360;
const V_MBCNT_LO_U32_B32: u32 = 0x365;
const V_MBCNT_HI_U32_B32: u32 = 0x366;
const DS_WRITE_B32: u32 = 0x0D;
const DS_READ_B32: u32 = 0x36;
const DS_BPERMUTE_B32: u32 = 0xB3;
const DS_SWIZZLE_B32: u32 = 0x35;

const V_CMP_GT_U32: u32 = 0x7D88_0000;

fn readlane(sdst: u32, vsrc: u16, lane: u16) -> [u32; 2] {
    vop3(V_READLANE_B32, sdst, [vgpr(vsrc), int(lane), 0])
}

fn per_lane_value() -> Vec<u32> {
    vec![vop2(V_ADD_NC_U32, 7, vgpr(1), 0)]
}

fn native(body: &[u32], lds: bool) -> NativeProgram {
    let mut words = vec![sop1(5, 0x03, 128)];
    words.extend([0x3D << 26 | 0x02 << 18 | 8 << 6 | 2, 125 << 25]);
    for i in 0..4u32 {
        words.push(vop1(V_MOV_B32, i + 1, i as u16));
    }
    words.extend_from_slice(body);
    words.push(vop2(V_LSHLREV_B32, 6, int(2), 0));
    words.extend([
        0x38 << 26 | 0x1C << 18 | 1 << 12,
        0x80 << 24 | 2 << 16 | 5 << 8 | 6,
    ]);
    words.push(S_ENDPGM);
    NativeProgram {
        stage: HwStage::Cs,
        wave64: true,
        vgprs: 16,
        layout: vec![
            RootParam {
                root: RootType::Constants,
                table_offset_dwords: 0,
                sgprs: 4,
                base_register: 0,
                user_data: 0,
                slot: 0,
            },
            RootParam {
                root: RootType::Table,
                table_offset_dwords: 0,
                sgprs: 1,
                base_register: 0,
                user_data: 4,
                slot: 1,
            },
        ],
        used: vec![UsedResource {
            kind: ResourceType::Uav,
            root: RootType::Table,
            table_offset_dwords: 0,
            lower_bound: 0,
            stride: 0,
            dwords: 4,
            user_data: 4,
            slot: 1,
        }],
        io: StageIo::Cs(ComputeIo {
            rsrc2: 5 << 1 | if lds { 1 << 15 } else { 0 },
            threads: [THREADS, 1, 1],
        }),
        code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
    }
}

struct Built {
    shader: HostShader,
    wave_size: Option<u32>,
    barriers: u32,
}

fn lowered(native: &NativeProgram, switch: bool) -> Result<Built, ShaderError> {
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir");
    let st = structure::structure(&p).expect("structure");
    let res = resource::track(&p, &native.layout, native.stage, Some(&st)).expect("resources");
    let io = io::raise(&p, native).expect("io");
    let parts = lower::Parts {
        native,
        p: &p,
        st: &st,
        io: &io,
        res: &res,
        inputs: Vec::new(),
        outputs: Vec::new(),
        prims: Vec::new(),
        mesh_stride: 0,
    };
    let out = if switch {
        with_wave64_on_32(|| lower::lower(&parts))
    } else {
        lower::lower(&parts)
    }?;
    let (wave_size, barriers) = (out.wave_size, out.group_barriers);
    let shader = HostShader {
        stage: HwStage::Cs,
        dxil: out.dxil,
        user_data: out.user_data,
        samplers: out.samplers,
        static_samplers: Vec::new(),
        root_cbvs: out.root_cbvs,
        root_srvs: out.root_srvs,
        root_uavs: out.root_uavs,
        memory: out.memory,
        memory_writes: out.memory_writes,
        heap_usage: out.heap_usage,
        view_addresses: out.view_addresses,
        heap: out.heap,
        heap_writes: out.heap_writes,
    };
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref())
    {
        let mut copy = shader.dxil.clone();
        v.validate(&mut copy).expect("DXIL validation");
    }
    Ok(Built {
        shader,
        wave_size,
        barriers,
    })
}

fn run_on_hosts(shader: &HostShader, constants: [u32; 4], check: impl Fn(&str, &[u32])) {
    let _one = super::valu_tests::ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut ran = 0;
    for (backend, warp) in [("hardware", false), ("WARP", true)] {
        let Ok(host) = Host::new(warp) else {
            eprintln!("no SM 6.6 {backend}; skipped");
            continue;
        };
        ran += 1;
        let (root, pso) = host
            .compute_pipeline(shader)
            .unwrap_or_else(|e| panic!("{backend}: pipeline: {e}"));
        let mut ud = vec![0; shader.user_data_dwords().max(1) as usize];
        for u in &shader.user_data {
            ud[u.index as usize] = if u.root == RootType::Constants {
                constants[u.dword as usize]
            } else {
                0
            };
        }
        let out = host
            .dispatch(
                &root,
                &pso,
                &Dispatch {
                    roots: vec![(0, Root::Constants(ud))],
                    views: &[View::RawUav(THREADS)],
                    samplers: &[],
                    groups: [1, 1, 1],
                },
            )
            .unwrap_or_else(|e| panic!("{backend}: dispatch: {e}"));
        check(backend, &out[0][..THREADS as usize]);
    }
    if ran == 0 {
        eprintln!("no SM 6.6 device; GPU checks skipped");
    }
}

fn runs(words: &[u32], lds: bool, want: impl Fn(u32) -> u32) {
    let built = lowered(&native(words, lds), true).expect("lowers");
    assert_eq!(built.wave_size, None, "no wave size asked for");
    run_on_hosts(&built.shader, [BASE, 0, 0, 0], |backend, got| {
        for (lane, &g) in got.iter().enumerate() {
            let w = want(lane as u32);
            assert_eq!(g, w, "{backend}: lane {lane}: got {g:#x}, want {w:#x}");
        }
    });
}

#[test]
fn a_lane_of_the_other_warp_is_read_through_shared_memory() {
    for lane in [0u16, 5, 31, 32, 40, 63] {
        let mut words = per_lane_value();
        words.extend(readlane(12, 7, lane));
        words.push(vop1(V_MOV_B32, 5, 12));
        runs(&words, false, |_| BASE + u32::from(lane));
    }
}

#[test]
fn the_first_lane_is_lane_zero_for_both_warps() {
    let mut words = per_lane_value();
    words.push(vop1(0x02, 12, vgpr(7)));
    words.extend(readlane(13, 7, 3));
    words.push(vop1(V_MOV_B32, 8, 13));
    words.push(vop2(V_ADD_NC_U32, 5, 12, 8));
    runs(&words, false, |_| BASE + BASE + 3);
}

#[test]
fn counting_lanes_below_spans_both_warps() {
    let mut words = Vec::new();
    words.extend(vop3(V_MBCNT_LO_U32_B32, 5, [193, int(0), 0]));
    words.extend(vop3(V_MBCNT_HI_U32_B32, 5, [193, vgpr(5), 0]));
    runs(&words, false, |lane| lane);
}

#[test]
fn counting_the_lanes_of_a_compare_below_spans_both_warps() {
    let mut words = vec![vop2(V_AND_B32, 7, int(7), 0)];
    words.push(V_CMP_GT_U32 | 7 << 9 | u32::from(int(3)));
    words.extend(vop3(V_MBCNT_LO_U32_B32, 8, [106, int(0), 0]));
    words.extend(vop3(V_MBCNT_HI_U32_B32, 5, [107, vgpr(8), 0]));
    runs(&words, false, |lane| (0..lane).filter(|l| l & 7 < 3).count() as u32);
}

#[test]
fn a_permute_across_the_warps_reverses_the_group() {
    let mut words = per_lane_value();
    words.push(vop2(V_XOR_B32, 8, int(63), 0));
    words.push(vop2(V_LSHLREV_B32, 8, int(2), 8));
    words.extend(ds(DS_BPERMUTE_B32, 5, 8, 7));
    runs(&words, false, |lane| BASE + 63 - lane);
}

#[test]
fn a_swizzle_reads_within_its_half_of_the_group() {
    let mut words = per_lane_value();
    words.extend(ds(DS_SWIZZLE_B32, 5, 7, 0));
    runs(&words, false, |lane| BASE + (lane & !31));
}

#[test]
fn shared_memory_written_by_one_warp_is_read_by_the_other() {
    let mut words = per_lane_value();
    words.push(vop2(V_LSHLREV_B32, 9, int(2), 0));
    words.extend(ds(DS_WRITE_B32, 0, 9, 7));
    words.push(vop2(V_XOR_B32, 8, int(63), 0));
    words.push(vop2(V_LSHLREV_B32, 8, int(2), 8));
    words.extend(ds(DS_READ_B32, 5, 8, 0));
    let built = lowered(&native(&words, true), true).expect("lowers");
    assert_eq!(built.wave_size, None);
    assert!(built.barriers >= 1, "a barrier between the write and the read");
    runs(&words, true, |lane| BASE + 63 - lane);
}

fn prefix_section() -> Vec<u32> {
    let mut w = vec![sop1(0, 0x25, 193)];
    w.extend(vop3(0x101, 1, [int(0), vgpr(0), 0]));
    for shift in [1u32, 2, 4, 8] {
        w.push(vop2(V_ADD_NC_U32, 1, 0xFA, 1));
        w.push(dpp16(1, 0x110 + shift));
    }
    let set_exec = |w: &mut Vec<u32>, lo: u32, hi: u32| {
        w.extend([sop1(EXEC, 0x03, 255), lo, sop1(EXEC + 1, 0x03, 255), hi]);
    };
    w.extend(readlane(2, 1, 15));
    set_exec(&mut w, 0xFFFF_0000, 0);
    w.push(vop2(V_ADD_NC_U32, 1, 2, 1));
    w.extend(readlane(2, 1, 31));
    set_exec(&mut w, 0, 0x0000_FFFF);
    w.push(vop2(V_ADD_NC_U32, 1, 2, 1));
    w.extend(readlane(2, 1, 47));
    set_exec(&mut w, 0, 0xFFFF_0000);
    w.push(vop2(V_ADD_NC_U32, 1, 2, 1));
    w.push(sop1(EXEC, 0x04, 0));
    w.push(vop1(V_MOV_B32, 5, vgpr(1)));
    w
}

const fn dpp16(vsrc0: u32, ctrl: u32) -> u32 {
    0xF << 28 | 0xF << 24 | ctrl << 8 | vsrc0
}

#[test]
fn a_prefix_sum_of_the_group_joins_both_warps() {
    let words = prefix_section();
    runs(&words, false, |lane| (0..=lane).sum());
}

#[test]
fn a_lane_operation_in_a_branch_only_some_lanes_take_keeps_the_console_wave_size() {
    let mut words = per_lane_value();
    words.push(V_CMP_GT_U32 | u32::from(int(20)));
    words.push(sop1(14, 0x24, 106));
    words.push(0xBF88_0002);
    words.extend(readlane(12, 7, 40));
    words.push(sop1(EXEC, 0x04, 14));
    words.push(vop1(V_MOV_B32, 5, 12));
    let built = lowered(&native(&words, false), true).expect("lowers");
    assert_eq!(built.wave_size, Some(64));
    assert_eq!(built.barriers, 0);
}

#[test]
fn shared_memory_used_both_ways_in_a_branch_keeps_the_wave_size() {
    let mut words = per_lane_value();
    words.push(vop2(V_LSHLREV_B32, 9, int(2), 0));
    words.push(V_CMP_GT_U32 | u32::from(int(20)));
    words.push(sop1(14, 0x24, 106));
    words.push(0xBF88_0004);
    words.extend(ds(DS_WRITE_B32, 0, 9, 7));
    words.extend(ds(DS_READ_B32, 5, 9, 0));
    words.push(sop1(EXEC, 0x04, 14));
    let built = lowered(&native(&words, true), true).expect("lowers");
    assert_eq!(built.wave_size, Some(64));
}

#[test]
fn several_writes_in_a_branch_are_ordered_before_the_read_after_it() {
    let mut words = per_lane_value();
    words.push(vop2(V_LSHLREV_B32, 9, int(2), 0));
    words.push(V_CMP_GT_U32 | u32::from(int(20)));
    words.push(sop1(14, 0x24, 106));
    words.push(0xBF88_0004);
    words.extend(ds(DS_WRITE_B32, 0, 9, 7));
    words.extend(ds(DS_WRITE_B32, 0, 9, 7));
    words.push(sop1(EXEC, 0x04, 14));
    words.push(vop2(V_XOR_B32, 8, int(63), 0));
    words.push(vop2(V_LSHLREV_B32, 8, int(2), 8));
    words.extend(ds(DS_READ_B32, 5, 8, 0));
    let built = lowered(&native(&words, true), true).expect("lowers");
    assert_eq!(built.wave_size, None);
    assert!(built.barriers >= 1);
}

#[test]
fn shared_memory_only_read_in_a_branch_runs_on_the_waves_of_the_host() {
    let mut words = per_lane_value();
    words.push(vop2(V_LSHLREV_B32, 9, int(2), 0));
    words.extend(ds(DS_WRITE_B32, 0, 9, 7));
    words.push(V_CMP_GT_U32 | u32::from(int(20)));
    words.push(sop1(14, 0x24, 106));
    words.push(0xBF88_0002);
    words.extend(ds(DS_READ_B32, 5, 9, 0));
    words.push(sop1(EXEC, 0x04, 14));
    let built = lowered(&native(&words, true), true).expect("lowers");
    assert_eq!(built.wave_size, None);
    assert!(built.barriers >= 2);
}

#[test]
fn without_the_switch_the_console_wave_size_is_asked_for() {
    let mut words = per_lane_value();
    words.extend(readlane(12, 7, 40));
    words.push(vop1(V_MOV_B32, 5, 12));
    let built = lowered(&native(&words, false), false).expect("lowers");
    assert_eq!(built.wave_size, Some(64));
    assert_eq!(built.barriers, 0);
}
