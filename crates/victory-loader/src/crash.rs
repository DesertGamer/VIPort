//! Names the faulting code when title code crashes.
//!
//! A vectored handler sees every exception first, so it only prints the
//! kinds that end a process (access violations and the like, and
//! breakpoints: titles assert with `int3`, fatal without a debugger) as
//! first-chance, since a handler may still take them (ntdll's loader does
//! with its own probes). What nobody handles, of any kind (an uncaught C++
//! exception too), is reported from the unhandled-exception filter
//! (`report_unhandled`, which the runtime calls ahead of the title's own)
//! and appended to the crash log, with the title's last debug output, which
//! often states the reason. Panics of ours go to the crash log too.
//!
//! The vectored handler also does the job of AMD-only instructions Intel
//! CPUs lack, and an emulated `clzero` whose write faults gives that fault to
//! the title at its own instruction.

use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{
    EXCEPTION_ACCESS_VIOLATION, EXCEPTION_BREAKPOINT, EXCEPTION_ILLEGAL_INSTRUCTION,
    EXCEPTION_INT_DIVIDE_BY_ZERO, EXCEPTION_PRIV_INSTRUCTION, EXCEPTION_STACK_OVERFLOW, HMODULE,
    STATUS_GUARD_PAGE_VIOLATION, STATUS_IN_PAGE_ERROR,
};

use windows_sys::Win32::System::Diagnostics::Debug::{
    AddVectoredExceptionHandler, CONTEXT, EXCEPTION_CONTINUE_EXECUTION, EXCEPTION_POINTERS,
    EXCEPTION_RECORD, M128A, RtlLookupFunctionEntry, RtlVirtualUnwind, SetUnhandledExceptionFilter,
    UNW_FLAG_NHANDLER,
};
use windows_sys::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    GetModuleFileNameW, GetModuleHandleExW,
};
use windows_sys::Win32::System::Threading::GetCurrentThreadStackLimits;

const EXCEPTION_CONTINUE_SEARCH: i32 = 0;
/// Frames listed per report.
pub(crate) const MAX_FRAMES: usize = 40;

/// Memory saved to files when a crash is reported: (address, length, file).
#[cfg(feature = "diagnostics")]
static SAVE: std::sync::OnceLock<Vec<(usize, usize, std::path::PathBuf)>> =
    std::sync::OnceLock::new();

/// Debugging aid: on a reported crash, also write each `[address, address
/// + len)` (readable title memory, e.g. an engine table) to its file.
#[cfg(feature = "diagnostics")]
pub fn save_on_crash(ranges: Vec<(usize, usize, std::path::PathBuf)>) {
    let _ = SAVE.set(ranges);
}

/// The file crash reports are appended to, besides being printed.
static LOG: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Also appends every unhandled-exception report and panic to `file`, so a
/// crash can be read after the console is gone.
pub fn log_crashes_to(file: std::path::PathBuf) {
    if LOG.set(file).is_ok() {
        let print = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            print(info);
            append_to_log(&format!(
                "VIctory: panic on thread {:?}: {info}\n{}",
                std::thread::current().name().unwrap_or("?"),
                title_output()
            ));
        }));
    }
}

/// Appends a failure that is not an exception (the host GPU's) to the crash
/// log, with the title's last debug output, and prints it.
pub fn log_crash(text: &str) {
    let text = format!("{text}
{}", title_output());
    eprint!("{text}");
    append_to_log(&text);
}

fn append_to_log(text: &str) {
    use std::io::Write;
    let Some(file) = LOG.get() else { return };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let opened = std::fs::OpenOptions::new().create(true).append(true).open(file);
    if let Ok(mut f) = opened {
        let _ = write!(f, "unix time {now}, process {}\n{text}\n", std::process::id());
    }
}

/// The title's last lines of debug output, kept for crash reports.
static SAID: std::sync::Mutex<std::collections::VecDeque<String>> =
    std::sync::Mutex::new(std::collections::VecDeque::new());
const SAID_LINES: usize = 8;

/// Keeps a line of the title's debug output for the next crash report.
pub fn note_title_output(line: &str) {
    if line.is_empty() {
        return;
    }
    let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
    if said.len() == SAID_LINES {
        said.pop_front();
    }
    said.push_back(line.to_owned());
}

/// The kept lines, one per report line; nothing if another thread holds
/// them (it may be the one that faulted).
fn title_output() -> String {
    let Ok(said) = SAID.try_lock() else {
        return String::new();
    };
    said.iter().map(|l| format!("  title said: {l}\n")).collect()
}

/// Installs the handlers; call once before running title code.
pub fn report_crashes() {
    unsafe extern "system" fn filter(info: *const EXCEPTION_POINTERS) -> i32 {
        // SAFETY: the system passes the exception being dispatched.
        unsafe { report_unhandled(info) };
        EXCEPTION_CONTINUE_SEARCH
    }
    // SAFETY: registers a handler that reads the exception it is given and,
    // for an instruction it emulates or a store of `zero_line` it recovers,
    // rewrites that thread's context and record, which belong to the
    // dispatch for its duration; the title's own filter, once it sets one,
    // takes the place of ours and calls `report_unhandled` first.
    unsafe {
        AddVectoredExceptionHandler(0, Some(handler));
        SetUnhandledExceptionFilter(Some(filter));
    }
}

/// What the vectored handler reports as it happens.
const FATAL: [i32; 6] = [
    EXCEPTION_ACCESS_VIOLATION,
    EXCEPTION_ILLEGAL_INSTRUCTION,
    EXCEPTION_PRIV_INSTRUCTION,
    EXCEPTION_INT_DIVIDE_BY_ZERO,
    EXCEPTION_STACK_OVERFLOW,
    EXCEPTION_BREAKPOINT,
];

/// Console CPUs are Zen 2: titles use AMD-only instructions Intel CPUs
/// lack. Does their job instead: `clzero` (`0f 01 fc`) zeroes the 64-byte
/// line at `rax`; `monitorx`/`mwaitx` (`0f 01 fa`/`fb`) are wait hints, so
/// a spin loop around them works with mwaitx as a yield. Where `clzero`'s
/// write faults, the emulated write raises the fault the real instruction
/// raised on Zen 5 (see [`zero_line`]): the emulation then declines and puts
/// that fault, the hardware's own, in the title's record, so the handlers
/// after ours see it at the title's `clzero`. An attached debugger (first
/// chance) and any vectored handler registered ahead of ours (the GPU
/// write-watch handler) see only the nested fault inside
/// `victory_zero_line`, never the one at the title's `clzero`. That one goes
/// to the vectored handlers after ours, the title's frame handlers and the
/// unhandled-exception filter.
///
/// # Safety
/// `info` points at an exception being dispatched; for a `clzero` whose line
/// may fault, the vectored handler is installed (it is the only caller
/// outside tests).
unsafe fn emulate_amd_only(info: *mut EXCEPTION_POINTERS) -> bool {
    // SAFETY: the caller passes valid exception pointers; the faulting
    // instruction's bytes are readable, as the CPU just fetched them. The
    // exception and context records belong to this thread's dispatch for the
    // handler's duration and nothing else touches them, so writing them
    // through `&mut` is sound. `zero_line` stores only to the line the
    // title's `clzero` names, as the instruction itself would, and the
    // caller installed the handler that recovers that store if it faults.
    unsafe {
        let record = &mut *(*info).ExceptionRecord;
        let context = &mut *(*info).ContextRecord;
        if record.ExceptionCode != EXCEPTION_ILLEGAL_INSTRUCTION {
            return false;
        }
        let code = context.Rip as *const u8;
        match code.cast::<[u8; 3]>().read_unaligned() {
            [0x0f, 0x01, 0xfc] => {
                if let Err(fault) = zero_line(context.Rax) {
                    record.ExceptionCode = fault.ExceptionCode;
                    record.NumberParameters = fault.NumberParameters;
                    record.ExceptionInformation = fault.ExceptionInformation;
                    return false;
                }
            }
            [0x0f, 0x01, 0xfa] => {}
            [0x0f, 0x01, 0xfb] => std::thread::yield_now(),
            _ => {
                // Only bytes inside the faulting instruction's page are read: the next page may
                // not be mapped.
                let room = 0x1000 - (context.Rip as usize & 0xfff);
                let mut bytes = [0u8; 32];
                let n = room.min(bytes.len());
                std::ptr::copy_nonoverlapping(code, bytes.as_mut_ptr(), n);
                if let Some(pass) = clwb_loop(&bytes[..n]) {
                    // A whole write-back loop in one exception (see `clwb_loop`).
                    let limit = *gpr(context, pass.limit);
                    let base = gpr(context, pass.base);
                    *base = if *base <= limit {
                        base.wrapping_add(((limit - *base) / 64 + 1) * 64)
                    } else {
                        base.wrapping_add(64)
                    };
                    let flags = cmp_flags(*base, limit);
                    context.EFlags = context.EFlags & !CMP_FLAGS | flags;
                    context.Rip += pass.len as u64;
                    return true;
                }
                if n >= 15
                    && let Some(len) = clwb_len(bytes[..15].try_into().unwrap_or([0; 15]))
                {
                    // A write-back hint: the PC's memory is coherent, so it has nothing to do.
                    context.Rip += len as u64;
                    return true;
                }
                let Some(len) = sse4a(code, &mut context.Anonymous.FltSave.XmmRegisters) else {
                    return false;
                };
                context.Rip += len as u64;
                return true;
            }
        }
        context.Rip += 3;
    }
    true
}

/// The bytes `clzero` zeroes, aligned to their size (AMD's manual allows other
/// sizes; Zen's is 64).
const CLZERO_LINE_BYTES: u64 = 64;

// `victory_zero_line(line: u64 /* rcx */, fault: *mut EXCEPTION_RECORD /* rdx */)
// -> bool`: eight 8-byte stores of zero to `line`, then true. A store that
// faults is resumed at `victory_zero_line_recovered` by `recover_line_fault`,
// which has written `*fault` (`rdx` is left alone for it), and returns false.
// A leaf that touches no stack and no nonvolatile register, so it needs no
// unwind data. A line aligned to its 64 bytes lies inside one page, so the
// stores fault together unless another thread changes the page between them,
// which can leave the line partly zeroed.
std::arch::global_asm!(
    ".text",
    ".p2align 4",
    ".globl victory_zero_line",
    "victory_zero_line:",
    "    xor eax, eax",
    ".globl victory_zero_line_stores",
    "victory_zero_line_stores:",
    "    mov qword ptr [rcx], rax",
    "    mov qword ptr [rcx + 8], rax",
    "    mov qword ptr [rcx + 16], rax",
    "    mov qword ptr [rcx + 24], rax",
    "    mov qword ptr [rcx + 32], rax",
    "    mov qword ptr [rcx + 40], rax",
    "    mov qword ptr [rcx + 48], rax",
    "    mov qword ptr [rcx + 56], rax",
    ".globl victory_zero_line_stored",
    "victory_zero_line_stored:",
    "    mov eax, 1",
    "    ret",
    ".globl victory_zero_line_recovered",
    "victory_zero_line_recovered:",
    "    xor eax, eax",
    "    ret",
);

// The routine's eight 8-byte stores write exactly one line.
const _: () = assert!(CLZERO_LINE_BYTES == 8 * 8);

unsafe extern "C" {
    fn victory_zero_line(line: u64, fault: *mut EXCEPTION_RECORD) -> bool;
    /// The routine's first store.
    #[link_name = "victory_zero_line_stores"]
    static ZERO_LINE_STORES: u8;
    /// Just past the routine's last store.
    #[link_name = "victory_zero_line_stored"]
    static ZERO_LINE_STORED: u8;
    /// Where a faulting store resumes, to return false.
    #[link_name = "victory_zero_line_recovered"]
    static ZERO_LINE_RECOVERED: u8;
}

/// The faults a store of [`zero_line`]'s routine recovers. A stack overflow
/// (a store into this thread's last stack guard page) is not one of them and
/// ends the process as before. Anything else inside the routine (a data
/// breakpoint's single step) is not the store's.
const STORE_FAULTS: [i32; 3] = [
    EXCEPTION_ACCESS_VIOLATION,
    STATUS_GUARD_PAGE_VIOLATION,
    STATUS_IN_PAGE_ERROR,
];

/// Zeroes the line that holds `rax` (its [`CLZERO_LINE_BYTES`] bytes, aligned
/// to their size) as `clzero` would, or returns the record of the fault its
/// write raised, the
/// hardware's own. On a Ryzen 7 9800X3D (Zen 5), with `rax` = line + 5, the
/// real `clzero` raised the same records: a write access violation
/// `[1, line]` for a no-access, read-only or kernel-half line,
/// `[0, usize::MAX]` for a non-canonical `rax`, and a guard-page violation
/// `[1, line]` that clears the guard bit, after which a retry faults or
/// writes as the page allows. The console's Zen 2 is not measured. A guard
/// page on this thread's stack is grown by the kernel as it would be for the
/// title.
///
/// # Safety
/// The title may write the line (a store there is what its `clzero` does).
/// If the store may fault, the vectored handler, which calls
/// [`recover_line_fault`], is installed; otherwise the fault goes on
/// unrecovered.
#[expect(
    clippy::result_large_err,
    reason = "only a faulting clzero returns it, and its code and parameters go to the title's \
              record; boxing it would allocate inside an exception handler"
)]
unsafe fn zero_line(rax: u64) -> Result<(), EXCEPTION_RECORD> {
    let line = rax & !(CLZERO_LINE_BYTES - 1);
    // SAFETY: a zeroed EXCEPTION_RECORD is valid.
    let mut fault: EXCEPTION_RECORD = unsafe { std::mem::zeroed() };
    // SAFETY: the routine stores only to the line's `CLZERO_LINE_BYTES`, which the caller
    // lets it write; a store that faults is recovered by `recover_line_fault`
    // in the vectored handler, which the caller installed if the store may
    // fault, and which writes `fault`, alive until the call returns.
    let zeroed = unsafe { victory_zero_line(line, &raw mut fault) };
    if zeroed { Ok(()) } else { Err(fault) }
}

/// For a store fault in [`zero_line`]'s routine: writes the fault's record
/// through the routine's `fault` argument and resumes it at its recovery
/// label, which returns false. It runs nested inside the handler that called
/// the routine, so it neither logs, allocates nor locks.
///
/// # Safety
/// `info` comes from the system, for an exception being dispatched.
unsafe fn recover_line_fault(info: *mut EXCEPTION_POINTERS) -> bool {
    let stores = (&raw const ZERO_LINE_STORES).addr();
    let stored = (&raw const ZERO_LINE_STORED).addr();
    // SAFETY: the system passes valid exception pointers; the records belong
    // to this thread's dispatch for the handler's duration.
    let (record, context) = unsafe { (&*(*info).ExceptionRecord, &mut *(*info).ContextRecord) };
    if !STORE_FAULTS.contains(&record.ExceptionCode)
        || !(stores..stored).contains(&(context.Rip as usize))
    {
        return false;
    }
    // SAFETY: the handler recognises the routine by the address of its
    // stores, which run only inside `zero_line`'s call; the routine leaves
    // `rdx` as `zero_line` passed it, an `EXCEPTION_RECORD` alive until that
    // call returns.
    unsafe { (context.Rdx as *mut EXCEPTION_RECORD).write(*record) };
    context.Rip = (&raw const ZERO_LINE_RECOVERED).addr() as u64;
    true
}

/// `clwb m8` (`66 [rex] 0f ae /6`, a memory operand), which Intel CPUs before
/// Ice Lake lack: its length, or `None` for any other instruction.
fn clwb_len(code: [u8; 15]) -> Option<usize> {
    let at = if code[1] & 0xf0 == 0x40 { 2 } else { 1 };
    if code[0] != 0x66 || code[at] != 0x0f || code[at + 1] != 0xae {
        return None;
    }
    let modrm = code[at + 2];
    let (mode, rm) = (modrm >> 6, modrm & 7);
    if mode == 3 || (modrm >> 3) & 7 != 6 {
        return None;
    }
    let mut len = at + 3;
    let mut base = rm;
    if rm == 4 {
        base = code[len] & 7;
        len += 1;
    }
    len += match mode {
        0 if rm == 5 || (rm == 4 && base == 5) => 4,
        1 => 1,
        2 => 4,
        _ => 0,
    };
    Some(len)
}

/// A `clwb` write-back loop as the title's compilers build it, one cache line a
/// pass (`base` and `limit` any registers):
///
/// ```text
/// top: clwb [base]
///      add  base, 0x40
///      cmp  base, limit
///      jbe  top
/// ```
///
/// An exception costs about a microsecond and the title writes back whole
/// buffers this way while it loads, so the loop is done in one: `clwb` has
/// nothing to do on the PC, `base` ends past `limit` as the last pass leaves it.
struct ClwbLoop {
    base: u8,
    limit: u8,
    /// Bytes from `clwb` to the instruction after `jbe`.
    len: usize,
}

fn clwb_loop(code: &[u8]) -> Option<ClwbLoop> {
    // clwb [base]: 66 [40|41] 0f ae, ModRM mod 00, reg 6, no SIB or displacement.
    let (rex, at) = match code.get(1)? {
        0x40 | 0x41 => (code[1], 2),
        _ => (0, 1),
    };
    if *code.first()? != 0x66 || code.get(at..at + 2)? != [0x0f, 0xae] {
        return None;
    }
    let modrm = *code.get(at + 2)?;
    let rm = modrm & 7;
    if modrm >> 6 != 0 || (modrm >> 3) & 7 != 6 || rm == 4 || rm == 5 {
        return None;
    }
    let base = rm | (rex & 1) << 3;
    // add base, 0x40: REX.W(+B) 83 /0 ib.
    let add = at + 3;
    if *code.get(add..add + 4)? != [0x48 | base >> 3, 0x83, 0xc0 | (base & 7), 0x40] {
        return None;
    }
    // cmp base, limit: REX.W 39 /r with base in r/m.
    let cmp = add + 4;
    let &[cmp_rex @ 0x48..=0x4f, 0x39, cmp_modrm] = code.get(cmp..cmp + 3)? else {
        return None;
    };
    if cmp_modrm >> 6 != 3 || (cmp_modrm & 7 | (cmp_rex & 1) << 3) != base {
        return None;
    }
    let limit = (cmp_modrm >> 3) & 7 | (cmp_rex & 4) << 1;
    // jbe top.
    let jump = cmp + 3;
    let len = jump + 2;
    if *code.get(jump)? != 0x76 || i64::from(*code.get(jump + 1)? as i8) != -(len as i64) {
        return None;
    }
    (limit != base).then_some(ClwbLoop { base, limit, len })
}

/// The general register numbered `n` as instructions encode it.
fn gpr(context: &mut CONTEXT, n: u8) -> &mut u64 {
    match n & 15 {
        0 => &mut context.Rax,
        1 => &mut context.Rcx,
        2 => &mut context.Rdx,
        3 => &mut context.Rbx,
        4 => &mut context.Rsp,
        5 => &mut context.Rbp,
        6 => &mut context.Rsi,
        7 => &mut context.Rdi,
        8 => &mut context.R8,
        9 => &mut context.R9,
        10 => &mut context.R10,
        11 => &mut context.R11,
        12 => &mut context.R12,
        13 => &mut context.R13,
        14 => &mut context.R14,
        _ => &mut context.R15,
    }
}

/// The status flags `cmp a, b` leaves (CF, PF, AF, ZF, SF, OF) as `EFLAGS` bits.
fn cmp_flags(a: u64, b: u64) -> u32 {
    let r = a.wrapping_sub(b);
    u32::from(a < b)
        | u32::from((r as u8).count_ones().is_multiple_of(2)) << 2
        | (((a ^ b ^ r) >> 4) as u32 & 1) << 4
        | u32::from(r == 0) << 6
        | ((r >> 63) as u32) << 7
        | ((((a ^ b) & (a ^ r)) >> 63) as u32) << 11
}
const CMP_FLAGS: u32 = 0x8d5;

/// SSE4a's `extrq`/`insertq` (register forms, the only ones there are) on
/// the saved XMM registers; returns the instruction's length.
unsafe fn sse4a(code: *const u8, xmm: &mut [M128A; 16]) -> Option<usize> {
    // SAFETY: the caller's code bytes are readable up to the instruction's end.
    let b = |i: usize| unsafe { code.add(i).read() };
    let prefix = b(0);
    let (rex, at) = if b(1) & 0xf0 == 0x40 {
        (b(1), 2)
    } else {
        (0, 1)
    };
    if !matches!(prefix, 0x66 | 0xf2) || b(at) != 0x0f || !matches!(b(at + 1), 0x78 | 0x79) {
        return None;
    }
    let modrm = b(at + 2);
    if modrm >> 6 != 3 {
        return None;
    }
    let dst = usize::from((modrm >> 3) & 7 | (rex & 4) << 1);
    let src = usize::from(modrm & 7 | (rex & 1) << 3);
    let immediate = b(at + 1) == 0x78;
    let (fields, len) = if immediate {
        ((b(at + 3), b(at + 4)), at + 5)
    } else {
        // extrq reads its fields from src's low half, insertq from its high.
        let f = if prefix == 0x66 { xmm[src].Low } else { xmm[src].High as u64 };
        ((f as u8, (f >> 8) as u8), at + 3)
    };
    let (n, at_bit) = (u32::from(fields.0 & 63), u32::from(fields.1 & 63));
    let bits = if n == 0 { 64 } else { n };
    let mask = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
    if prefix == 0x66 {
        // extrq: register form takes its fields from src, works on dst.
        if immediate && (modrm >> 3) & 7 != 0 {
            return None;
        }
        let target = if immediate { src } else { dst };
        xmm[target].Low = (xmm[target].Low >> at_bit) & mask;
    } else {
        let value = (xmm[src].Low & mask) << at_bit;
        xmm[dst].Low = xmm[dst].Low & !(mask << at_bit) | value;
    }
    Some(len)
}

unsafe extern "system" fn handler(info: *mut EXCEPTION_POINTERS) -> i32 {
    // SAFETY: the system passes valid exception pointers.
    if unsafe { recover_line_fault(info) || emulate_amd_only(info) } {
        return EXCEPTION_CONTINUE_EXECUTION;
    }
    // SAFETY: as above.
    if FATAL.contains(&unsafe { (*(*info).ExceptionRecord).ExceptionCode }) {
        // SAFETY: as above.
        unsafe { report("first-chance exception", info, false) };
    }
    EXCEPTION_CONTINUE_SEARCH
}

/// For an unhandled-exception filter: reports the exception that is about
/// to end the process, and appends it to the crash log.
///
/// # Safety
/// `info` comes from the system, for an exception being dispatched.
pub unsafe fn report_unhandled(info: *const EXCEPTION_POINTERS) {
    // SAFETY: forwarded from the caller.
    unsafe { report("unhandled exception", info, true) };
}

/// The bytes at `at` as hex, as many as the longest x86 instruction (15),
/// but none past the end of `at`'s page: the next page may not be mapped.
/// For an instruction the CPU refused, they name it (`0f 01 fd` is
/// `rdpru`), which the address alone does not.
///
/// # Safety
/// `at`'s page is readable.
unsafe fn instruction_bytes(at: usize) -> String {
    let n = (0x1000 - (at & 0xfff)).min(15);
    // SAFETY: `n` bytes from `at` stay inside its page, which the caller
    // says is readable.
    let bytes = unsafe { std::slice::from_raw_parts(at as *const u8, n) };
    let hex: Vec<_> = bytes.iter().map(|b| format!("{b:02x}")).collect();
    hex.join(" ")
}

/// MSVC's code for a C++ `throw`, and its magic first parameter.
const CPP_EXCEPTION: i32 = 0xE06D_7363_u32 as i32;
const CPP_MAGIC: usize = 0x1993_0520;

/// What a C++ throw threw: the decorated name of its type, then its value
/// where that names the cause (an integer, e.g. an HRESULT; a standard
/// exception's message).
///
/// # Safety
/// `p` are the parameters of a live MSVC throw (`p[0]` the magic).
unsafe fn thrown(p: &[usize]) -> String {
    // SAFETY: the throw's ThrowInfo, its RVAs relative to `p[3]`: catchable
    // type array at +12 (count, then type RVAs), each type's descriptor at
    // +4, whose decorated name starts at +16; the object is at `p[1]`.
    unsafe {
        let rva = |at: usize| (at as *const u32).read_unaligned() as usize;
        let types = p[3] + rva(p[2] + 12);
        let name = |i: usize| {
            let descriptor = p[3] + rva(p[3] + rva(types + 4 + 4 * i) + 4);
            std::ffi::CStr::from_ptr((descriptor + 16) as *const std::ffi::c_char)
                .to_string_lossy()
        };
        let first = name(0);
        let value = match &*first {
            // int, unsigned, long (HRESULT), unsigned long
            ".H" | ".I" | ".J" | ".K" if p[1] != 0 => {
                format!(" = {:#010x}", (p[1] as *const u32).read_unaligned())
            }
            // std::exception keeps its message after the table pointer.
            _ if (0..rva(types)).any(|i| name(i) == ".?AVexception@std@@") => {
                let message = (p[1] as *const *const std::ffi::c_char).add(1).read();
                if message.is_null() {
                    String::new()
                } else {
                    format!(": {}", std::ffi::CStr::from_ptr(message).to_string_lossy())
                }
            }
            _ => String::new(),
        };
        format!("{first}{value}")
    }
}

/// # Safety
/// See [`report_unhandled`].
unsafe fn report(what: &str, info: *const EXCEPTION_POINTERS, to_file: bool) {
    // SAFETY: the system passes valid exception pointers.
    let (record, context) = unsafe { (&*(*info).ExceptionRecord, &*(*info).ContextRecord) };
    let code = record.ExceptionCode;
    let at = record.ExceptionAddress as usize;
    let p = record.ExceptionInformation;
    let detail = if code == EXCEPTION_ACCESS_VIOLATION {
        let kind = match p[0] {
            0 => "read",
            1 => "write",
            _ => "execute",
        };
        format!(" ({kind} {:#x}: {})", p[1], memory_at(p[1]))
    } else if code == CPP_EXCEPTION && record.NumberParameters >= 4 && p[0] == CPP_MAGIC {
        // SAFETY: a live MSVC throw.
        format!(" (C++ {} thrown)", unsafe { thrown(&p) })
    } else if code == EXCEPTION_ILLEGAL_INSTRUCTION || code == EXCEPTION_PRIV_INSTRUCTION {
        // SAFETY: the CPU refused the instruction at `at`, so it fetched it.
        format!(" (code {})", unsafe { instruction_bytes(at) })
    } else {
        String::new()
    };
    // Printed line by line (a fault while unwinding must not lose the
    // lines before it), and kept for the file.
    let mut text = String::new();
    let mut line = |l: String| {
        eprintln!("{l}");
        text.push_str(&l);
        text.push('\n');
    };
    line(format!(
        "VIctory: {what} {:#010x} at {}{detail}",
        code as u32,
        locate(at)
    ));
    line(format!(
        "  rax {:#x} rbx {:#x} rcx {:#x} rdx {:#x} rsi {:#x} rdi {:#x} rsp {:#x} rbp {:#x}",
        context.Rax,
        context.Rbx,
        context.Rcx,
        context.Rdx,
        context.Rsi,
        context.Rdi,
        context.Rsp,
        context.Rbp
    ));
    line(format!(
        "  r8 {:#x} r9 {:#x} r10 {:#x} r11 {:#x} r12 {:#x} r13 {:#x} r14 {:#x} r15 {:#x}",
        context.R8,
        context.R9,
        context.R10,
        context.R11,
        context.R12,
        context.R13,
        context.R14,
        context.R15
    ));
    for (i, pc) in unwind(*context).into_iter().enumerate() {
        line(format!("  #{i:<2} {}", locate(pc as usize)));
    }
    if to_file {
        let said = title_output();
        eprint!("{said}");
        text.push_str(&said);
        append_to_log(&text);
    }
    #[cfg(feature = "diagnostics")]
    for (address, len, file) in SAVE.get().into_iter().flatten() {
        // SAFETY: the caller of `save_on_crash` named readable memory.
        let bytes = unsafe { std::slice::from_raw_parts(*address as *const u8, *len) };
        let saved = std::fs::write(file, bytes);
        eprintln!(
            "  saved {address:#x}+{len:#x} to {} ({saved:?})",
            file.display()
        );
    }
}

/// Return addresses of the faulting thread's frames, from the faulting
/// instruction up, by the unwind data of each image (the Windows loader
/// registered it for title images too). Code without unwind data (our
/// thunks, leaf functions) is taken to have its return address at `rsp`.
fn unwind(context: CONTEXT) -> Vec<u64> {
    let (mut low, mut high) = (0, 0);
    // SAFETY: writes the current thread's stack bounds.
    unsafe { GetCurrentThreadStackLimits(&mut low, &mut high) };
    let mut frames = Vec::with_capacity(MAX_FRAMES);
    unwind_into(context, low, high, &mut frames);
    frames
}

/// A `CONTEXT` at the 16-byte alignment the unwind and capture functions need.
#[repr(C, align(16))]
pub(crate) struct Aligned16(pub(crate) CONTEXT);

/// [`unwind`] for any thread of the process, its stack being `low..high`;
/// pushes no more than `MAX_FRAMES`, so it does not allocate when `frames`
/// has that capacity (another thread may be suspended holding the heap).
pub(crate) fn unwind_into(context: CONTEXT, low: usize, high: usize, frames: &mut Vec<u64>) {
    // `RtlVirtualUnwind` writes the context with 16-byte aligned stores, and
    // windows-sys declares `CONTEXT` without that alignment: a by-value copy on
    // the stack can land 8 bytes off and fault (seen in a debug build).
    let mut aligned = Aligned16(context);
    let context = &mut aligned.0;
    frames.push(context.Rip);
    while frames.len() < MAX_FRAMES {
        // Stop where reading the stack would fault inside this handler.
        let rsp = context.Rsp as usize;
        if rsp < low || rsp + 8 > high {
            break;
        }
        let mut image = 0;
        // SAFETY: looks up the function table entry covering `Rip`, if any.
        let entry = unsafe { RtlLookupFunctionEntry(context.Rip, &mut image, null_mut()) };
        if entry.is_null() {
            // SAFETY: `rsp` is inside this thread's stack.
            context.Rip = unsafe { (rsp as *const u64).read() };
            context.Rsp += 8;
        } else {
            let (mut data, mut frame) = (null_mut(), 0);
            // SAFETY: `entry` belongs to `image`; the context is our copy.
            unsafe {
                RtlVirtualUnwind(
                    UNW_FLAG_NHANDLER,
                    image,
                    context.Rip,
                    entry,
                    context,
                    &mut data,
                    &mut frame,
                    null_mut(),
                )
            };
        }
        if context.Rip == 0 {
            break;
        }
        frames.push(context.Rip);
    }
}

/// `module+rva`, or the bare address.
pub fn locate(address: usize) -> String {
    module_of(address).unwrap_or_else(|| format!("{address:#x}"))
}

fn module_of(address: usize) -> Option<String> {
    let mut module: HMODULE = std::ptr::null_mut();
    // SAFETY: looks up the module containing `address`; no reference taken.
    let found = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            address as *const u16,
            &mut module,
        )
    };
    if found == 0 {
        return None;
    }
    let mut path = [0u16; 260];
    // SAFETY: `module` is a loaded module; the buffer length is passed.
    let len = unsafe { GetModuleFileNameW(module, path.as_mut_ptr(), path.len() as u32) };
    let path = String::from_utf16_lossy(&path[..len as usize]);
    let name = path.rsplit('\\').next().unwrap_or(&path);
    Some(format!("{name}+{:#x}", address - module as usize))
}

/// What is at `address`, for an access violation there: nothing, memory
/// reserved but never committed, a guard page, or the protection the access
/// broke (with the module, for an image's pages). Each points at a different
/// cause: a wild pointer, an allocation whose commit failed, a stack
/// overrun, a write to code or constants.
fn memory_at(address: usize) -> String {
    use windows_sys::Win32::System::Memory::{
        MEM_FREE, MEM_IMAGE, MEM_RESERVE, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE,
        PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE, PAGE_EXECUTE_WRITECOPY, PAGE_GUARD,
        PAGE_NOACCESS, PAGE_READONLY, PAGE_READWRITE, PAGE_WRITECOPY, VirtualQuery,
    };
    // SAFETY: a zeroed MEMORY_BASIC_INFORMATION is valid.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: reads the address space's bookkeeping, not `address` itself.
    let n = unsafe { VirtualQuery(address as *const _, &mut info, size_of_val(&info)) };
    if n == 0 {
        // Above the process's address space: Windows reports a
        // non-canonical address as all ones.
        return "outside the address space".into();
    }
    let protect = match info.State {
        MEM_FREE => return "nothing mapped there".into(),
        MEM_RESERVE => return "reserved, never committed".into(),
        _ if info.Protect & PAGE_GUARD != 0 => return "a guard page".into(),
        _ => match info.Protect & 0xff {
            PAGE_NOACCESS => "no access".into(),
            PAGE_READONLY => "read-only".into(),
            PAGE_READWRITE => "read-write".into(),
            PAGE_WRITECOPY => "copy-on-write".into(),
            PAGE_EXECUTE => "execute-only".into(),
            PAGE_EXECUTE_READ => "read-execute".into(),
            PAGE_EXECUTE_READWRITE => "read-write-execute".into(),
            PAGE_EXECUTE_WRITECOPY => "copy-on-write, executable".into(),
            _ => format!("protection {:#x}", info.Protect),
        },
    };
    match module_of(address) {
        Some(module) if info.Type == MEM_IMAGE => format!("{protect}, in {module}"),
        _ => protect,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Foundation::EXCEPTION_SINGLE_STEP;
    use windows_sys::Win32::System::Diagnostics::Debug::{
        RtlCaptureContext, RtlCaptureStackBackTrace,
    };

    #[test]
    #[inline(never)]
    fn unwinds_like_the_system_walk() {
        // SAFETY: zeroed CONTEXT is valid; both calls write our buffers.
        let (frames, walked) = unsafe {
            // `RtlCaptureContext` wants it 16-byte aligned too.
            let mut context = Aligned16(std::mem::zeroed());
            RtlCaptureContext(&mut context.0);
            let mut walked = [null_mut(); 8];
            let n = RtlCaptureStackBackTrace(0, 8, walked.as_mut_ptr(), null_mut());
            (unwind(context.0), walked[..usize::from(n)].to_vec())
        };
        // Frame 0 differs (two call sites in this function); callers agree.
        assert!(walked.len() >= 4, "{walked:?}");
        let ours: Vec<_> = frames[1..walked.len()]
            .iter()
            .map(|&f| f as usize)
            .collect();
        let system: Vec<_> = walked[1..].iter().map(|&f| f as usize).collect();
        assert_eq!(ours, system);
    }

    #[test]
    fn memory_at_names_what_an_access_hit() {
        use windows_sys::Win32::System::Memory::{
            MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_GUARD, PAGE_NOACCESS, PAGE_READONLY,
            PAGE_READWRITE, VirtualAlloc, VirtualFree,
        };
        // SAFETY: reserves four pages of ours and commits three of them;
        // nothing reads or writes them.
        let base = unsafe {
            let base = VirtualAlloc(std::ptr::null(), 0x4000, MEM_RESERVE, PAGE_NOACCESS);
            assert!(!base.is_null());
            let page = |i: usize, protect| {
                let at = base.cast::<u8>().add(i * 0x1000).cast();
                assert!(!VirtualAlloc(at, 0x1000, MEM_COMMIT, protect).is_null());
            };
            page(1, PAGE_READONLY);
            page(2, PAGE_READWRITE | PAGE_GUARD);
            page(3, PAGE_NOACCESS);
            base as usize
        };
        assert_eq!(memory_at(base + 8), "reserved, never committed");
        assert_eq!(memory_at(base + 0x1008), "read-only");
        assert_eq!(memory_at(base + 0x2008), "a guard page");
        assert_eq!(memory_at(base + 0x3008), "no access");
        // SAFETY: releases the reservation made above.
        unsafe { VirtualFree(base as *mut _, 0, MEM_RELEASE) };
        assert_eq!(memory_at(base + 8), "nothing mapped there");
        assert_eq!(memory_at(0), "nothing mapped there");
        assert_eq!(memory_at(usize::MAX), "outside the address space");
        // This test's own code: an image page, named by its module.
        let code = memory_at(memory_at_names_what_an_access_hit as *const () as usize);
        let named = code.starts_with("read-execute, in victory_loader-");
        assert!(named, "{code}");
    }

    /// On an Intel CPU, the real instructions go through the vectored
    /// handler. On an AMD CPU they run natively, and the tests below test
    /// the emulation.
    #[test]
    fn clwb_loops() {
        // clwb [rcx]; add rcx, 0x40; cmp rcx, rax; jbe top
        let a = clwb_loop(&[
            0x66, 0x0f, 0xae, 0x31, 0x48, 0x83, 0xc1, 0x40, 0x48, 0x39, 0xc1, 0x76, 0xf3,
        ])
        .expect("rcx loop");
        assert_eq!((a.base, a.limit, a.len), (1, 0, 13));
        // clwb [r9]; add r9, 0x40; cmp r9, rax; jbe top
        let b = clwb_loop(&[
            0x66, 0x41, 0x0f, 0xae, 0x31, 0x49, 0x83, 0xc1, 0x40, 0x49, 0x39, 0xc1, 0x76, 0xf2,
        ])
        .expect("r9 loop");
        assert_eq!((b.base, b.limit, b.len), (9, 0, 14));
        // A different stride, a jump elsewhere, or a cut-off loop is not one.
        assert!(
            clwb_loop(&[
                0x66, 0x0f, 0xae, 0x31, 0x48, 0x83, 0xc1, 0x20, 0x48, 0x39, 0xc1, 0x76, 0xf3
            ])
            .is_none()
        );
        assert!(
            clwb_loop(&[
                0x66, 0x0f, 0xae, 0x31, 0x48, 0x83, 0xc1, 0x40, 0x48, 0x39, 0xc1, 0x76, 0xf0
            ])
            .is_none()
        );
        assert!(clwb_loop(&[0x66, 0x0f, 0xae, 0x31, 0x48, 0x83, 0xc1, 0x40]).is_none());
        // The flags the last cmp leaves: base just past limit.
        assert_eq!(cmp_flags(0x1040, 0x1000) & 0x41, 0); // CF and ZF clear: the loop ends
        assert_eq!(cmp_flags(0x1000, 0x1000) & 0x40, 0x40);
    }

    #[test]
    fn clwb_lengths() {
        let pad = |b: &[u8]| {
            let mut c = [0u8; 15];
            c[..b.len()].copy_from_slice(b);
            c
        };
        assert_eq!(clwb_len(pad(&[0x66, 0x41, 0x0f, 0xae, 0x31])), Some(5)); // clwb [r9]
        assert_eq!(clwb_len(pad(&[0x66, 0x0f, 0xae, 0x30])), Some(4)); // clwb [rax]
        assert_eq!(clwb_len(pad(&[0x66, 0x0f, 0xae, 0x70, 0x40])), Some(5)); // clwb [rax+0x40]
        assert_eq!(
            clwb_len(pad(&[0x66, 0x0f, 0xae, 0xb0, 0, 1, 0, 0])),
            Some(8)
        ); // [rax+disp32]
        assert_eq!(clwb_len(pad(&[0x66, 0x0f, 0xae, 0x34, 0x24])), Some(5)); // clwb [rsp]
        assert_eq!(
            clwb_len(pad(&[0x66, 0x0f, 0xae, 0x35, 0, 0, 0, 0])),
            Some(8)
        ); // [rip+disp32]
        assert_eq!(clwb_len(pad(&[0x66, 0x0f, 0xae, 0x38])), None); // clflushopt, /7
        assert_eq!(clwb_len(pad(&[0x0f, 0xae, 0x30])), None); // xsaveopt, no 66
        assert_eq!(clwb_len(pad(&[0x66, 0x0f, 0xae, 0xf0])), None); // register form
    }

    #[test]
    fn clzero_zeroes_its_line_on_any_cpu() {
        report_crashes();
        #[repr(align(64))]
        struct Line([u8; 128]);
        let mut line = Line([0xff; 128]);
        let at = line.0.as_mut_ptr().wrapping_add(5); // inside the first line
        // SAFETY: clzero writes only the 64-byte line holding `at`, ours.
        unsafe { std::arch::asm!(".byte 0x0f, 0x01, 0xfc", in("rax") at) };
        assert!(line.0[..64].iter().all(|&b| b == 0));
        assert!(line.0[64..].iter().all(|&b| b == 0xff));
        // insertq xmm0, xmm1, len 16, at 16; then extrq xmm0, len 8, at 20.
        let out: u64;
        // SAFETY: touches only the named registers.
        unsafe {
            std::arch::asm!(
                "movq xmm0, {a}", "movq xmm1, {b}",
                ".byte 0xf2, 0x0f, 0x78, 0xc1, 0x10, 0x10",
                "movq {a}, xmm0",
                ".byte 0x66, 0x0f, 0x78, 0xc0, 0x08, 0x14",
                "movq {b}, xmm0",
                a = inout(reg) 0x1111_2222_3333_4444u64 => _, b = inout(reg) 0xabcdu64 => out,
                out("xmm0") _, out("xmm1") _)
        };
        assert_eq!(out, 0xbc); // 0x1111_2222_abcd_4444 >> 20, 8 bits
        // SAFETY: monitorx/mwaitx are wait hints that write no memory.
        unsafe {
            std::arch::asm!(".byte 0x0f, 0x01, 0xfa", ".byte 0x0f, 0x01, 0xfb",
                in("rax") at, in("ecx") 0, in("edx") 0)
        };
    }

    fn reg(low: u64, high: u64) -> M128A {
        M128A {
            Low: low,
            High: high as i64,
        }
    }

    /// Gives `code` to the emulation as if it faulted with `exception` at
    /// its first byte, with `rax` and `xmm` as the saved registers. The
    /// emulation does not execute `code`, so this works on any CPU. Returns
    /// the number of bytes that the emulation skipped, or `None` if it did
    /// not emulate the instruction. `xmm` gets the saved registers after.
    fn emulate_as(exception: i32, code: &[u8], rax: u64, xmm: &mut [M128A; 16]) -> Option<u64> {
        let (emulated, skipped, _) = emulate_with_record(exception, code, rax, xmm);
        emulated.then_some(skipped)
    }

    /// [`emulate_as`], also returning whether the emulation took the
    /// instruction, the bytes it moved `Rip` by either way, and the exception
    /// record it leaves.
    fn emulate_with_record(
        exception: i32,
        code: &[u8],
        rax: u64,
        xmm: &mut [M128A; 16],
    ) -> (bool, u64, EXCEPTION_RECORD) {
        // `int3` after the instruction, so that a decoder that reads too
        // far sees no valid bytes.
        let mut bytes = [0xcc_u8; 16];
        bytes[..code.len()].copy_from_slice(code);
        let start = bytes.as_ptr() as u64;
        // SAFETY: a zeroed record and context are valid; the emulation
        // reads only `bytes` and writes only the record and `context` (and
        // the line at `rax` for clzero, which the caller may write, or whose
        // store faults and is recovered by the handler the caller installed).
        unsafe {
            let mut record: EXCEPTION_RECORD = std::mem::zeroed();
            record.ExceptionCode = exception;
            let mut context: CONTEXT = std::mem::zeroed();
            context.Rip = start;
            context.Rax = rax;
            context.Anonymous.FltSave.XmmRegisters = *xmm;
            let mut pointers = EXCEPTION_POINTERS {
                ExceptionRecord: &mut record,
                ContextRecord: &mut context,
            };
            let emulated = emulate_amd_only(&mut pointers);
            *xmm = context.Anonymous.FltSave.XmmRegisters;
            (emulated, context.Rip - start, record)
        }
    }

    fn emulate(code: &[u8], rax: u64, xmm: &mut [M128A; 16]) -> Option<u64> {
        emulate_as(EXCEPTION_ILLEGAL_INSTRUCTION, code, rax, xmm)
    }

    fn low(xmm: &[M128A; 16], i: usize) -> u64 {
        xmm[i].Low
    }

    #[test]
    fn emulated_clzero_zeroes_its_line() {
        #[repr(align(64))]
        struct Line([u8; 128]);
        let mut line = Line([0xff; 128]);
        let at = line.0.as_mut_ptr().wrapping_add(5) as u64; // inside the first line
        let mut xmm = [reg(0, 0); 16];
        assert_eq!(emulate(&[0x0f, 0x01, 0xfc], at, &mut xmm), Some(3));
        assert!(line.0[..64].iter().all(|&b| b == 0));
        assert!(line.0[64..].iter().all(|&b| b == 0xff));
    }

    #[test]
    fn zero_line_zeroes_the_whole_line_that_holds_its_address() {
        #[repr(align(64))]
        struct Line([u8; 128]);
        let mut line = Line([0xff; 128]);
        let inside = line.0.as_mut_ptr().wrapping_add(5) as u64;
        // SAFETY: the store goes to the first 64 bytes of `line`, ours and writable.
        let zeroed = unsafe { zero_line(inside) };
        assert!(zeroed.is_ok());
        assert!(line.0[..64].iter().all(|&b| b == 0));
        assert!(line.0[64..].iter().all(|&b| b == 0xff));
    }

    /// Gives `record` to `recover_line_fault` as if raised at `rip`, with the
    /// routine's `fault` argument pointing at a record of code 0x1234.
    /// Returns whether it recovered, `Rip` after, and the argument's record.
    fn recover_as(mut record: EXCEPTION_RECORD, rip: u64) -> (bool, u64, EXCEPTION_RECORD) {
        // SAFETY: zeroed records and context are valid; `recover_line_fault`
        // reads the record and context and writes only the context and
        // through `Rdx`, at `fault`, ours.
        unsafe {
            let mut fault: EXCEPTION_RECORD = std::mem::zeroed();
            fault.ExceptionCode = 0x1234;
            let mut context: CONTEXT = std::mem::zeroed();
            context.Rip = rip;
            context.Rdx = (&raw mut fault).addr() as u64;
            let mut pointers = EXCEPTION_POINTERS {
                ExceptionRecord: &mut record,
                ContextRecord: &mut context,
            };
            let recovered = recover_line_fault(&mut pointers);
            (recovered, context.Rip, fault)
        }
    }

    #[test]
    fn a_single_step_inside_the_line_stores_is_not_a_store_fault() {
        // A data breakpoint on the first store leaves `Rip` at the second.
        let second_store = (&raw const ZERO_LINE_STORES).addr() as u64 + 3;
        // SAFETY: a zeroed record is valid.
        let mut record: EXCEPTION_RECORD = unsafe { std::mem::zeroed() };
        record.ExceptionCode = EXCEPTION_SINGLE_STEP;
        let (recovered, rip, fault) = recover_as(record, second_store);
        assert!(!recovered);
        assert_eq!(rip, second_store);
        assert_eq!(fault.ExceptionCode, 0x1234);
    }

    #[test]
    fn a_store_fault_keeps_its_whole_record() {
        let first_store = (&raw const ZERO_LINE_STORES).addr() as u64;
        // SAFETY: a zeroed record is valid.
        let mut record: EXCEPTION_RECORD = unsafe { std::mem::zeroed() };
        record.ExceptionCode = STATUS_IN_PAGE_ERROR;
        record.NumberParameters = 3;
        // A write at 0x1000 whose page read failed with STATUS_DEVICE_NOT_CONNECTED.
        record.ExceptionInformation[..3].copy_from_slice(&[1, 0x1000, 0xc000_009d]);
        let (recovered, rip, fault) = recover_as(record, first_store);
        assert!(recovered);
        assert_eq!(rip, (&raw const ZERO_LINE_RECOVERED).addr() as u64);
        assert_eq!(fault.ExceptionCode, STATUS_IN_PAGE_ERROR);
        assert_eq!(fault.NumberParameters, 3);
        assert_eq!(fault.ExceptionInformation[..3], [1, 0x1000, 0xc000_009d]);
    }

    // `victory_test_clzero(rax: u64)`: `ud2`, then a `clzero` of `rax`, then
    // returns. A leaf without stack use, so it needs no unwind data.
    std::arch::global_asm!(
        ".text",
        ".globl victory_test_clzero",
        "victory_test_clzero:",
        "    mov rax, rcx",
        ".globl victory_test_clzero_ud2",
        "victory_test_clzero_ud2:",
        "    ud2",
        ".globl victory_test_clzero_instruction",
        "victory_test_clzero_instruction:",
        "    .byte 0x0f, 0x01, 0xfc",
        "    ret",
    );

    unsafe extern "C" {
        fn victory_test_clzero(rax: u64);
        #[link_name = "victory_test_clzero_ud2"]
        static TEST_CLZERO_UD2: u8;
        #[link_name = "victory_test_clzero_instruction"]
        static TEST_CLZERO_INSTRUCTION: u8;
    }

    /// Ahead of ours: turns the stub's `ud2` into the illegal instruction an
    /// Intel CPU raises at its `clzero`, which this CPU would run natively.
    unsafe extern "system" fn clzero_is_illegal(info: *mut EXCEPTION_POINTERS) -> i32 {
        // SAFETY: the system passes valid exception pointers, for this
        // thread's dispatch.
        let (record, context) =
            unsafe { (&mut *(*info).ExceptionRecord, &mut *(*info).ContextRecord) };
        let ud2 = (&raw const TEST_CLZERO_UD2).addr() as u64;
        if record.ExceptionCode == EXCEPTION_ILLEGAL_INSTRUCTION && context.Rip == ud2 {
            let clzero = (&raw const TEST_CLZERO_INSTRUCTION).cast_mut();
            context.Rip = clzero.addr() as u64;
            record.ExceptionAddress = clzero.cast();
        }
        EXCEPTION_CONTINUE_SEARCH
    }

    /// What the title's handler saw at the stub's `clzero`: code, parameter
    /// count, the first two parameters, exception address, `Rip`.
    type TitleSaw = (i32, u32, [usize; 2], usize, u64);
    static TITLE_SAW: std::sync::Mutex<Option<TitleSaw>> = std::sync::Mutex::new(None);
    static TITLE_CALLS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    /// After ours, as the title's: notes the fault at the stub's `clzero`,
    /// makes its page writable and goes back to the `ud2`, so the `clzero`
    /// is emulated again.
    unsafe extern "system" fn title_handler(info: *mut EXCEPTION_POINTERS) -> i32 {
        use windows_sys::Win32::System::Memory::{PAGE_READWRITE, VirtualProtect};
        // SAFETY: the system passes valid exception pointers, for this
        // thread's dispatch.
        let (record, context) = unsafe { (&*(*info).ExceptionRecord, &mut *(*info).ContextRecord) };
        if context.Rip != (&raw const TEST_CLZERO_INSTRUCTION).addr() as u64 {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        // A second call means the first did not fix the fault: decline, so
        // the child dies rather than loops.
        if TITLE_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 0 {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        let [kind, address, ..] = record.ExceptionInformation;
        let saw = (
            record.ExceptionCode,
            record.NumberParameters,
            [kind, address],
            record.ExceptionAddress.addr(),
            context.Rip,
        );
        *TITLE_SAW.lock().unwrap_or_else(|e| e.into_inner()) = Some(saw);
        let mut old = 0;
        // SAFETY: changes the protection of the test's own page at `address`.
        unsafe { VirtualProtect(address as *const _, 1, PAGE_READWRITE, &mut old) };
        context.Rip = (&raw const TEST_CLZERO_UD2).addr() as u64;
        EXCEPTION_CONTINUE_EXECUTION
    }

    /// In a child process of the next test (a store that faults in the
    /// emulation would end the test process): gives the emulation `clzero`s
    /// whose write faults. The expected records are those the real
    /// instruction gave on a Ryzen 7 9800X3D (Zen 5) with `rax` = line + 5.
    #[test]
    fn clzero_child() {
        use windows_sys::Win32::System::Diagnostics::Debug::RemoveVectoredExceptionHandler;
        use windows_sys::Win32::System::Memory::{
            MEM_COMMIT, MEM_RESERVE, PAGE_GUARD, PAGE_NOACCESS, PAGE_READONLY, PAGE_READWRITE,
            VirtualAlloc, VirtualProtect,
        };
        if std::env::var_os("VICTORY_CLZERO_CHILD").is_none() {
            return;
        }
        // The handler recovers a store of the emulation that faults.
        report_crashes();
        // The exception the emulation leaves for a clzero with `rax`, or
        // `None` when it zeroed the line and stepped past the instruction.
        let clzero = |rax: u64| {
            let mut xmm = [reg(0, 0); 16];
            let code = [0x0f, 0x01, 0xfc];
            let (emulated, skipped, record) =
                emulate_with_record(EXCEPTION_ILLEGAL_INSTRUCTION, &code, rax, &mut xmm);
            if emulated {
                assert_eq!(skipped, 3, "rax {rax:#x}");
                return None;
            }
            assert_eq!(skipped, 0, "rax {rax:#x}");
            assert_eq!(record.NumberParameters, 2, "rax {rax:#x}");
            let [kind, address, ..] = record.ExceptionInformation;
            Some((record.ExceptionCode, [kind, address]))
        };
        let violation = |information| Some((EXCEPTION_ACCESS_VIOLATION, information));
        let guard = |line| Some((STATUS_GUARD_PAGE_VIOLATION, [1, line]));
        // SAFETY: reserves three pages of ours and commits two, then
        // allocates three more pages; only the emulation's stores, the
        // handlers above and this test touch them.
        unsafe {
            let base = VirtualAlloc(std::ptr::null(), 0x3000, MEM_RESERVE, PAGE_NOACCESS);
            assert!(!base.is_null());
            let commit = |i: usize, protect| {
                let at = base.cast::<u8>().add(i * 0x1000);
                assert!(!VirtualAlloc(at.cast(), 0x1000, MEM_COMMIT, protect).is_null());
            };
            commit(1, PAGE_NOACCESS);
            commit(2, PAGE_READONLY);
            // Each its own allocation: a guard page committed inside a
            // no-access reservation gives an access violation without the
            // guard fault, for the real instruction too.
            let allocate = |protect| {
                let at = VirtualAlloc(std::ptr::null(), 0x1000, MEM_RESERVE | MEM_COMMIT, protect);
                assert!(!at.is_null());
                at.cast::<u8>()
            };
            let pages = [
                base.cast::<u8>(),
                base.cast::<u8>().add(0x1000),
                base.cast::<u8>().add(0x2000),
                allocate(PAGE_READONLY | PAGE_GUARD),
                allocate(PAGE_READWRITE),
                allocate(PAGE_READWRITE),
            ];
            let page = |i: usize| pages[i];
            // Pages 4 and 5 hold 0xff; page 4 then becomes a guard page,
            // not on this thread's stack, and page 5 no-access.
            page(4).write_bytes(0xff, 0x1000);
            page(5).write_bytes(0xff, 0x1000);
            let mut old = 0;
            let (guarded, closed) = (PAGE_READWRITE | PAGE_GUARD, PAGE_NOACCESS);
            assert_ne!(VirtualProtect(page(4).cast(), 0x1000, guarded, &mut old), 0);
            assert_ne!(VirtualProtect(page(5).cast(), 0x1000, closed, &mut old), 0);
            let line = |i: usize| page(i) as usize + 0x40;
            let rax = |i: usize| line(i) as u64 + 5;
            for i in 0..3 {
                assert_eq!(clzero(rax(i)), violation([1, line(i)]), "page {i}");
            }
            // Not canonical with 48-bit or with 57-bit addresses.
            assert_eq!(clzero(0x8000_0000_0000_0005), violation([0, usize::MAX]));
            // Canonical, in the kernel half.
            let kernel_line = 0xffff_8000_0000_0000;
            assert_eq!(clzero(kernel_line as u64 + 5), violation([1, kernel_line]));
            // A guard page faults first and loses its guard bit; then the
            // page's own protection decides.
            assert_eq!(clzero(rax(3)), guard(line(3)));
            assert_eq!(clzero(rax(3)), violation([1, line(3)]));
            assert_eq!(clzero(rax(4)), guard(line(4)));
            assert_eq!(page(4).add(0x40).read(), 0xff);
            assert_eq!(clzero(rax(4)), None);
            let page4 = std::slice::from_raw_parts(page(4), 0x1000);
            assert!(page4[0x40..0x80].iter().all(|&b| b == 0));
            assert!(page4[0x80..].iter().all(|&b| b == 0xff));
            // Through the real handlers, as on an Intel CPU: the title's
            // handler sees the fault at its clzero, makes the page writable,
            // and the clzero runs again.
            let front = AddVectoredExceptionHandler(1, Some(clzero_is_illegal));
            let back = AddVectoredExceptionHandler(0, Some(title_handler));
            assert!(!front.is_null() && !back.is_null());
            victory_test_clzero(rax(5));
            assert_ne!(RemoveVectoredExceptionHandler(front), 0);
            assert_ne!(RemoveVectoredExceptionHandler(back), 0);
            let at = (&raw const TEST_CLZERO_INSTRUCTION).addr();
            let saw = *TITLE_SAW.lock().unwrap();
            let expected = (EXCEPTION_ACCESS_VIOLATION, 2, [1, line(5)], at, at as u64);
            assert_eq!(saw, Some(expected));
            assert_eq!(TITLE_CALLS.load(std::sync::atomic::Ordering::SeqCst), 1);
            let page5 = std::slice::from_raw_parts(page(5), 0x1000);
            assert!(page5[..0x40].iter().all(|&b| b == 0xff));
            assert!(page5[0x40..0x80].iter().all(|&b| b == 0));
            assert!(page5[0x80..].iter().all(|&b| b == 0xff));
        }
    }

    #[test]
    fn emulated_clzero_raises_the_fault_of_its_write() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "crash::tests::clzero_child", "--nocapture"])
            .env("VICTORY_CLZERO_CHILD", "1")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{:?}: {text}", out.status);
        let ran = "test crash::tests::clzero_child ... ok";
        assert!(text.contains(ran), "expected `{ran}` in: {text}");
    }

    #[test]
    fn emulated_monitorx_and_mwaitx_are_skipped() {
        let mut xmm = [reg(1, 2); 16];
        assert_eq!(emulate(&[0x0f, 0x01, 0xfa], 0, &mut xmm), Some(3));
        assert_eq!(emulate(&[0x0f, 0x01, 0xfb], 0, &mut xmm), Some(3));
        assert!(xmm.iter().all(|r| r.Low == 1 && r.High == 2));
    }

    #[test]
    fn emulated_insertq_and_extrq_with_immediates() {
        let mut xmm = [reg(0, 0); 16];
        xmm[0] = reg(0x1111_2222_3333_4444, 0x5555);
        xmm[1] = reg(0xabcd, 0x6666);
        // insertq xmm0, xmm1, length 16, index 16
        let insertq = [0xf2, 0x0f, 0x78, 0xc1, 0x10, 0x10];
        assert_eq!(emulate(&insertq, 0, &mut xmm), Some(6));
        assert_eq!(low(&xmm, 0), 0x1111_2222_abcd_4444);
        assert_eq!((low(&xmm, 1), xmm[1].High), (0xabcd, 0x6666));
        // extrq xmm0, length 8, index 20
        let extrq = [0x66, 0x0f, 0x78, 0xc0, 0x08, 0x14];
        assert_eq!(emulate(&extrq, 0, &mut xmm), Some(6));
        assert_eq!(low(&xmm, 0), 0xbc);
    }

    #[test]
    fn emulated_insertq_and_extrq_with_fields_in_a_register() {
        let mut xmm = [reg(0, 0); 16];
        // insertq xmm0, xmm1: length and index in xmm1's high half.
        xmm[0] = reg(0x1111_2222_3333_4444, 0);
        xmm[1] = reg(0xabcd, 0x10_10);
        assert_eq!(emulate(&[0xf2, 0x0f, 0x79, 0xc1], 0, &mut xmm), Some(4));
        assert_eq!(low(&xmm, 0), 0x1111_2222_abcd_4444);
        // extrq xmm0, xmm1: length and index in xmm1's low half.
        xmm[1] = reg(0x14_08, 0);
        assert_eq!(emulate(&[0x66, 0x0f, 0x79, 0xc1], 0, &mut xmm), Some(4));
        assert_eq!(low(&xmm, 0), 0xbc);
    }

    #[test]
    fn emulated_sse4a_reaches_xmm8_to_xmm15() {
        let mut xmm = [reg(0, 0); 16];
        xmm[8] = reg(0x1111_2222_3333_4444, 0);
        xmm[9] = reg(0xabcd, 0);
        // insertq xmm8, xmm9, length 16, index 16 (REX.R and REX.B)
        let insertq = [0xf2, 0x45, 0x0f, 0x78, 0xc1, 0x10, 0x10];
        assert_eq!(emulate(&insertq, 0, &mut xmm), Some(7));
        assert_eq!(low(&xmm, 8), 0x1111_2222_abcd_4444);
        assert_eq!((low(&xmm, 0), low(&xmm, 1)), (0, 0));
        // extrq xmm9, length 8, index 4 (REX.B)
        let extrq = [0x66, 0x41, 0x0f, 0x78, 0xc1, 0x08, 0x04];
        assert_eq!(emulate(&extrq, 0, &mut xmm), Some(7));
        assert_eq!(low(&xmm, 9), 0xbc);
        assert_eq!(low(&xmm, 1), 0);
    }

    #[test]
    fn emulated_sse4a_length_0_is_64_bits() {
        let mut xmm = [reg(0, 0); 16];
        xmm[0] = reg(0x1122_3344_5566_7788, 0);
        // extrq xmm0, length 0 (64), index 0. A non-zero index with
        // length 64 goes past bit 63, and AMD leaves that result undefined.
        assert_eq!(
            emulate(&[0x66, 0x0f, 0x78, 0xc0, 0x00, 0x00], 0, &mut xmm),
            Some(6)
        );
        assert_eq!(low(&xmm, 0), 0x1122_3344_5566_7788);
        // insertq xmm0, xmm1, length 0 (64), index 0
        xmm[1] = reg(0xdead_beef_cafe_f00d, 0);
        assert_eq!(
            emulate(&[0xf2, 0x0f, 0x78, 0xc1, 0x00, 0x00], 0, &mut xmm),
            Some(6)
        );
        assert_eq!(low(&xmm, 0), 0xdead_beef_cafe_f00d);
    }

    #[test]
    fn other_instructions_and_exceptions_are_not_emulated() {
        let mut xmm = [reg(0, 0); 16];
        assert_eq!(emulate(&[0x0f, 0x0b], 0, &mut xmm), None); // ud2
        // extrq with a memory operand, which does not exist
        assert_eq!(emulate(&[0x66, 0x0f, 0x79, 0x00], 0, &mut xmm), None);
        // extrq with immediates needs ModRM.reg 0
        let bad_extrq = [0x66, 0x0f, 0x78, 0xc8, 0x08, 0x14];
        assert_eq!(emulate(&bad_extrq, 0, &mut xmm), None);
        // a clzero that did not cause an illegal instruction
        let clzero = [0x0f, 0x01, 0xfc];
        assert_eq!(
            emulate_as(EXCEPTION_ACCESS_VIOLATION, &clzero, 0, &mut xmm),
            None
        );
    }

    #[test]
    fn instruction_bytes_stop_at_the_end_of_their_page() {
        use windows_sys::Win32::System::Memory::{
            MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_NOACCESS, PAGE_READWRITE, VirtualAlloc,
            VirtualFree, VirtualProtect,
        };
        // rdpru, then the next instruction (mov rcx, rax) and padding.
        let mut code = [0x90_u8; 16];
        code[..6].copy_from_slice(&[0x0f, 0x01, 0xfd, 0x48, 0x89, 0xc1]);
        // SAFETY: two fresh pages of ours, the second made unreadable, so a
        // read past the end of the first faults.
        unsafe {
            let page = VirtualAlloc(
                std::ptr::null(),
                0x2000,
                MEM_RESERVE | MEM_COMMIT,
                PAGE_READWRITE,
            )
            .cast::<u8>();
            assert!(!page.is_null());
            let mut old = 0;
            let guard = page.add(0x1000).cast();
            assert_ne!(VirtualProtect(guard, 0x1000, PAGE_NOACCESS, &mut old), 0);
            page.copy_from_nonoverlapping(code.as_ptr(), code.len());
            let whole = "0f 01 fd 48 89 c1 90 90 90 90 90 90 90 90 90";
            assert_eq!(instruction_bytes(page as usize), whole);
            // The page's last three bytes: rdpru, and nothing read after it.
            let end = page.add(0x1000 - 3);
            end.copy_from_nonoverlapping(code.as_ptr(), 3);
            assert_eq!(instruction_bytes(end as usize), "0f 01 fd");
            VirtualFree(page.cast(), 0, MEM_RELEASE);
        }
    }
}
