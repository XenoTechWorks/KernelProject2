// Structured exception handling (SEH), the Windows x64 way: what C++ `throw` (and C __try/__except)
// is built on.
//
// - RaiseException captures the caller's registers (a CONTEXT) and dispatches: it walks up the stack
//   one frame at a time using each function's unwind information (the image's .pdata and .xdata),
//   and calls every frame's exception handler until one takes the exception.
// - That handler calls RtlUnwindEx, which walks up again, calling handlers with "unwinding" set
//   (that's where destructors run), until it reaches the target frame, then resumes there.
// - RtlLookupFunctionEntry / RtlVirtualUnwind / RtlCaptureContext are the building blocks, and the
//   C++ runtime (libgcc) also calls them directly.
//
// Our own dispatcher code has no Windows unwind information. When a walk climbs out of a handler into
// it, it continues where Windows' would: for a dispatch, at the frame that raised the exception; for
// an unwind started from inside another unwind's handler (a "collided unwind"), at the frame that
// unwind was working on.

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <unistd.h>
#include <vector>

#include "loader.hpp"

namespace {

struct alignas(16) Context {
    uint64_t home[6];
    uint32_t context_flags, mxcsr;
    uint16_t seg_cs, seg_ds, seg_es, seg_fs, seg_gs, seg_ss;
    uint32_t eflags;
    uint64_t dr[6];
    uint64_t gpr[16];  // rax rcx rdx rbx rsp rbp rsi rdi r8..r15: the unwind codes' register numbers
    uint64_t rip;
    uint8_t flt_save[512];  // fxsave area; xmm0 at +0xA0
    uint8_t rest[0x4D0 - 0x300];
};
static_assert(sizeof(Context) == 0x4D0);
static_assert(offsetof(Context, gpr) == 0x78 && offsetof(Context, rip) == 0xF8 && offsetof(Context, flt_save) == 0x100);
constexpr int RAX = 0, RCX = 1, RDX = 2, RSP = 4, R8 = 8, R9 = 9;

struct ExceptionRecord {
    DWORD code;
    DWORD flags;
    ExceptionRecord *nested;
    void *address;
    DWORD parameter_count;
    uint64_t information[15];
};
static_assert(sizeof(ExceptionRecord) == 152);

struct RuntimeFunction {
    uint32_t begin, end, unwind_info;
};

struct DispatcherContext {
    uint64_t control_pc;
    uint64_t image_base;
    RuntimeFunction *function_entry;
    uint64_t establisher_frame;
    uint64_t target_ip;
    Context *context;
    void *language_handler;
    void *handler_data;
    void *history_table;
    uint32_t scope_index;
    uint32_t fill;
};

using Handler = int(WINAPI *)(ExceptionRecord *, void *, Context *, DispatcherContext *);

constexpr DWORD EXCEPTION_NONCONTINUABLE = 0x01;
constexpr DWORD EXCEPTION_UNWINDING = 0x02;
constexpr DWORD EXCEPTION_EXIT_UNWIND = 0x04;
constexpr DWORD EXCEPTION_TARGET_UNWIND = 0x20;
constexpr DWORD UNW_FLAG_EHANDLER = 1, UNW_FLAG_UHANDLER = 2, UNW_FLAG_CHAININFO = 4;
constexpr int ExceptionContinueExecution = 0, ExceptionContinueSearch = 1;

// The walks in progress on this thread, innermost last: where to continue when a walk climbs into
// our own code (see the file comment).
struct Active {
    Context *resume;
};
thread_local std::vector<Active> active;

void *unhandled_filter;

// WINLOAD_SEH_DEBUG=1 prints every frame the walks visit (to stderr).
bool debug_walks = getenv("WINLOAD_SEH_DEBUG") != nullptr;

// ── finding and applying unwind information ────────────────────────────────────────────────────

Module *module_at(uint64_t pc) {
    for (Module *m : loader::modules())
        if (pc >= reinterpret_cast<uint64_t>(m->base) && pc < reinterpret_cast<uint64_t>(m->base) + m->size) return m;
    return nullptr;
}

RuntimeFunction *lookup(uint64_t pc, uint64_t *image_base) {
    Module *m = module_at(pc);
    if (!m) return nullptr;
    *image_base = reinterpret_cast<uint64_t>(m->base);
    uint32_t pe;
    memcpy(&pe, m->base + 0x3C, 4);
    uint32_t rva, size;
    memcpy(&rva, m->base + pe + 24 + 112 + 3 * 8, 4);  // the exception directory (.pdata)
    memcpy(&size, m->base + pe + 24 + 116 + 3 * 8, 4);
    auto *table = reinterpret_cast<RuntimeFunction *>(m->base + rva);
    uint32_t offset = static_cast<uint32_t>(pc - *image_base);
    size_t lo = 0, hi = rva ? size / sizeof(RuntimeFunction) : 0;
    while (lo < hi) {  // sorted by address
        size_t mid = (lo + hi) / 2;
        if (offset < table[mid].begin) hi = mid;
        else if (offset >= table[mid].end) lo = mid + 1;
        else return &table[mid];
    }
    return nullptr;
}

uint64_t load64(uint64_t address) {
    uint64_t v;
    memcpy(&v, reinterpret_cast<void *>(address), 8);
    return v;
}

// Applies one function's unwind codes to `c`. Returns its handler (if `want` matches its flags and
// the pc is past the prolog) and the handler's data.
void *virtual_unwind(DWORD want, uint64_t base, uint64_t pc, RuntimeFunction *fn, Context *c, void **handler_data,
                     uint64_t *establisher, bool chained = false) {
    const uint8_t *info = reinterpret_cast<const uint8_t *>(base + fn->unwind_info);
    uint8_t flags = info[0] >> 3, prolog_size = info[1], count = info[2];
    uint8_t frame_register = info[3] & 0xF, frame_offset = info[3] >> 4;
    const uint16_t *codes = reinterpret_cast<const uint16_t *>(info + 4);
    uint64_t pc_offset = pc - (base + fn->begin);
    bool in_prolog = !chained && pc_offset < prolog_size;

    // The frame: the frame pointer register (when the prolog has set it up) or rsp.
    uint64_t frame = c->gpr[RSP];
    if (frame_register && !in_prolog) frame = c->gpr[frame_register] - frame_offset * 16ull;
    if (!chained && establisher) *establisher = frame;

    for (int i = 0; i < count;) {
        uint8_t code_offset = codes[i] & 0xFF;
        uint8_t op = (codes[i] >> 8) & 0xF, op_info = codes[i] >> 12;
        int slots = 1;
        switch (op) {
            case 1: slots = op_info ? 3 : 2; break;  // UWOP_ALLOC_LARGE
            case 4: slots = 2; break;                // UWOP_SAVE_NONVOL
            case 5: slots = 3; break;                // UWOP_SAVE_NONVOL_FAR
            case 8: slots = 2; break;                // UWOP_SAVE_XMM128
            case 9: slots = 3; break;                // UWOP_SAVE_XMM128_FAR
        }
        bool done = !in_prolog || code_offset <= pc_offset;  // that prolog instruction has run
        if (done) {
            switch (op) {
                case 0:  // UWOP_PUSH_NONVOL
                    c->gpr[op_info] = load64(c->gpr[RSP]);
                    c->gpr[RSP] += 8;
                    break;
                case 1:  // UWOP_ALLOC_LARGE
                    c->gpr[RSP] += op_info ? codes[i + 1] | (static_cast<uint32_t>(codes[i + 2]) << 16) : codes[i + 1] * 8u;
                    break;
                case 2: c->gpr[RSP] += op_info * 8u + 8; break;  // UWOP_ALLOC_SMALL
                case 3: c->gpr[RSP] = c->gpr[frame_register] - frame_offset * 16ull; break;  // UWOP_SET_FPREG
                case 4: c->gpr[op_info] = load64(frame + codes[i + 1] * 8ull); break;  // UWOP_SAVE_NONVOL
                case 5: c->gpr[op_info] = load64(frame + (codes[i + 1] | (static_cast<uint32_t>(codes[i + 2]) << 16))); break;
                case 8: memcpy(c->flt_save + 0xA0 + op_info * 16, reinterpret_cast<void *>(frame + codes[i + 1] * 16ull), 16); break;
                case 9: memcpy(c->flt_save + 0xA0 + op_info * 16, reinterpret_cast<void *>(frame + (codes[i + 1] | (static_cast<uint32_t>(codes[i + 2]) << 16))), 16); break;
                case 10: {  // UWOP_PUSH_MACHFRAME
                    uint64_t sp = c->gpr[RSP] + (op_info ? 8 : 0);
                    c->rip = load64(sp);
                    c->gpr[RSP] = load64(sp + 24);
                    return nullptr;
                }
            }
        }
        i += slots;
    }
    const uint8_t *after = info + 4 + ((count + 1) & ~1) * 2;
    if (flags & UNW_FLAG_CHAININFO) {
        // The rest of this function's prolog is described by another entry ("chained").
        auto *parent = const_cast<RuntimeFunction *>(reinterpret_cast<const RuntimeFunction *>(after));
        virtual_unwind(0, base, pc, parent, c, nullptr, nullptr, true);
    }
    if (chained) return nullptr;
    c->rip = load64(c->gpr[RSP]);  // return to the caller
    c->gpr[RSP] += 8;
    if (in_prolog || !(flags & want) || (flags & UNW_FLAG_CHAININFO)) return nullptr;
    uint32_t handler_rva;
    memcpy(&handler_rva, after, 4);
    if (handler_data) *handler_data = const_cast<uint8_t *>(after + 4);
    return reinterpret_cast<void *>(base + handler_rva);
}

// Moves `c` to its caller. With `fn` null (no unwind information): a leaf function, so only the
// return address is on the stack. False when there's nowhere to go.
bool step(Context *c, RuntimeFunction *fn, uint64_t base) {
    if (fn) {
        virtual_unwind(0, base, c->rip, fn, c, nullptr, nullptr);
        return true;
    }
    if (!module_at(c->rip)) return false;
    c->rip = load64(c->gpr[RSP]);
    c->gpr[RSP] += 8;
    return true;
}

// When a walk reaches code that isn't in any image (our own dispatcher), continue at the innermost
// walk outside it that it hasn't continued from yet (see the file comment). `outside` counts the
// walks it may still use. False when there are none: the top of the stack.
bool continue_outside(Context *c, size_t &outside) {
    if (outside == 0) return false;
    outside--;
    *c = *active[outside].resume;
    return true;
}

}  // namespace

// Defined in assembly below.
extern "C" [[noreturn]] void seh_restore_context(Context *c);

namespace {

[[noreturn]] void resume(Context *c) {
    active.clear();  // every walk in progress is over: execution continues in the program
    seh_restore_context(c);
}

[[noreturn]] void unhandled(ExceptionRecord *rec) {
    fprintf(stderr, "winload: unhandled exception 0x%08x at %p\n", rec->code, rec->address);
    _exit(static_cast<int>(rec->code & 0xFF ? rec->code & 0xFF : 255));
}

}  // namespace

// RaiseException, after the assembly entry has captured the caller's registers in `c`.
extern "C" void seh_raise(Context *c) {
    ExceptionRecord rec{};
    rec.code = static_cast<DWORD>(c->gpr[RCX]);
    rec.flags = static_cast<DWORD>(c->gpr[RDX]) & EXCEPTION_NONCONTINUABLE;
    rec.address = reinterpret_cast<void *>(c->rip);
    rec.parameter_count = static_cast<DWORD>(c->gpr[R8]) > 15 ? 15 : static_cast<DWORD>(c->gpr[R8]);
    if (c->gpr[R9]) memcpy(rec.information, reinterpret_cast<void *>(c->gpr[R9]), rec.parameter_count * 8);

    Context start = *c, frame = *c;
    size_t outside = active.size();
    active.push_back({&start});  // for unwinds started by the handlers this calls
    for (;;) {
        if (!module_at(frame.rip)) {
            // Raised from inside a handler that an unwind called (libgcc does this to run
            // destructors): continue at the frame that unwind was working on, as Windows does for
            // a collided unwind. Otherwise this is past the program's first frame: nobody took it.
            if (!continue_outside(&frame, outside)) break;
            continue;
        }
        uint64_t base = 0;
        RuntimeFunction *fn = lookup(frame.rip, &base);
        if (!fn) {
            if (!step(&frame, nullptr, 0)) break;
            continue;
        }
        Context caller = frame;
        DispatcherContext d{};
        d.control_pc = frame.rip;
        d.image_base = base;
        d.function_entry = fn;
        d.context = &frame;
        d.language_handler = virtual_unwind(UNW_FLAG_EHANDLER, base, frame.rip, fn, &caller, &d.handler_data, &d.establisher_frame);
        if (debug_walks) fprintf(stderr, "[seh] dispatch %08x: rip %#lx rsp %#lx frame %#lx handler %p\n", rec.code, frame.rip, frame.gpr[RSP], d.establisher_frame, d.language_handler);
        if (d.language_handler) {
            int r = reinterpret_cast<Handler>(d.language_handler)(&rec, reinterpret_cast<void *>(d.establisher_frame), &frame, &d);
            if (r == ExceptionContinueExecution && !(rec.flags & EXCEPTION_NONCONTINUABLE)) {
                active.pop_back();
                return;  // the assembly entry returns to RaiseException's caller
            }
        }
        frame = caller;
    }
    active.pop_back();
    // Nobody took it: the program's unhandled-exception filter, if it set one, then the end.
    if (unhandled_filter) {
        struct {
            ExceptionRecord *rec;
            Context *ctx;
        } pointers{&rec, c};
        long r = reinterpret_cast<long(WINAPI *)(void *)>(unhandled_filter)(&pointers);
        if (r == -1) return;  // EXCEPTION_CONTINUE_EXECUTION
    }
    unhandled(&rec);
}

// RtlUnwindEx, after the assembly entry has captured the caller's registers in `c`. The arguments
// are in the captured rcx, rdx, r8, r9 and on the caller's stack.
extern "C" [[noreturn]] void seh_unwind(Context *c) {
    uint64_t target_frame = c->gpr[RCX], target_ip = c->gpr[RDX];
    auto *rec_in = reinterpret_cast<ExceptionRecord *>(c->gpr[R8]);
    uint64_t return_value = c->gpr[R9];
    ExceptionRecord rec{};
    if (rec_in) rec = *rec_in;
    else rec.code = 0xC0000027;  // STATUS_UNWIND
    rec.flags |= EXCEPTION_UNWINDING | (target_frame ? 0 : EXCEPTION_EXIT_UNWIND);

    Context frame = *c;
    size_t outside = active.size();
    active.push_back({&frame});  // for unwinds started by the handlers this calls (collided unwinds)
    for (;;) {
        if (!module_at(frame.rip)) {
            // Climbed into our own code: continue where the walk this one interrupted was.
            if (!continue_outside(&frame, outside)) break;
            continue;
        }
        uint64_t base = 0;
        RuntimeFunction *fn = lookup(frame.rip, &base);
        if (!fn) {
            if (!step(&frame, nullptr, 0)) break;
            continue;
        }
        Context caller = frame;
        DispatcherContext d{};
        d.control_pc = frame.rip;
        d.image_base = base;
        d.function_entry = fn;
        d.target_ip = target_ip;
        d.context = &frame;
        d.language_handler = virtual_unwind(UNW_FLAG_UHANDLER, base, frame.rip, fn, &caller, &d.handler_data, &d.establisher_frame);
        if (debug_walks) fprintf(stderr, "[seh] unwind to %#lx: rip %#lx rsp %#lx frame %#lx handler %p\n", target_frame, frame.rip, frame.gpr[RSP], d.establisher_frame, d.language_handler);
        if (d.establisher_frame == target_frame) rec.flags |= EXCEPTION_TARGET_UNWIND;
        if (d.language_handler) reinterpret_cast<Handler>(d.language_handler)(&rec, reinterpret_cast<void *>(d.establisher_frame), &frame, &d);
        if (d.establisher_frame == target_frame) {
            frame.gpr[RAX] = return_value;
            frame.rip = target_ip;
            resume(&frame);
        }
        frame = caller;
    }
    unhandled(&rec);
}

// The register save/restore parts have to be assembly: they must see the caller's registers exactly.
//
// Each entry point reserves a CONTEXT on the stack, saves every register into it (rsp and rip as the
// caller will see them after the return), and calls the C++ part with it. RaiseException can return:
// then rsi, rdi and xmm6-15 (preserved in the Windows convention, not in the Linux one the C++ part
// follows) are restored from the CONTEXT first.
#define SAVE_CONTEXT(ctx)                                                                                    \
    "mov [" ctx "+0x78], rax\n mov [" ctx "+0x80], rcx\n mov [" ctx "+0x88], rdx\n mov [" ctx "+0x90], rbx\n" \
    "mov [" ctx "+0xA0], rbp\n mov [" ctx "+0xA8], rsi\n mov [" ctx "+0xB0], rdi\n"                           \
    "mov [" ctx "+0xB8], r8\n mov [" ctx "+0xC0], r9\n mov [" ctx "+0xC8], r10\n mov [" ctx "+0xD0], r11\n"   \
    "mov [" ctx "+0xD8], r12\n mov [" ctx "+0xE0], r13\n mov [" ctx "+0xE8], r14\n mov [" ctx "+0xF0], r15\n" \
    "fxsave [" ctx "+0x100]\n stmxcsr [" ctx "+0x34]\n"                                                       \
    "mov word ptr [" ctx "+0x38], cs\n mov word ptr [" ctx "+0x42], ss\n"                                     \
    "mov dword ptr [" ctx "+0x30], 0x10000B\n"

asm(R"(
.intel_syntax noprefix
.text

# RtlCaptureContext(CONTEXT *rcx): the caller's registers, as if right after this call returns.
.global seh_capture_context
seh_capture_context:
)" SAVE_CONTEXT("rcx") R"(
    lea rax, [rsp+8]
    mov [rcx+0x98], rax
    mov rax, [rsp]
    mov [rcx+0xF8], rax
    pushfq
    pop rax
    mov [rcx+0x44], eax
    mov rax, [rcx+0x78]
    ret

# RaiseException(code, flags, count, params): 0x4D8 bytes keeps the CONTEXT 16-byte aligned.
.global seh_raise_exception
seh_raise_exception:
    sub rsp, 0x4D8
)" SAVE_CONTEXT("rsp") R"(
    lea rax, [rsp+0x4E0]
    mov [rsp+0x98], rax
    mov rax, [rsp+0x4D8]
    mov [rsp+0xF8], rax
    pushfq
    pop rax
    mov [rsp+0x44], eax
    mov rdi, rsp
    call seh_raise
    mov rsi, [rsp+0xA8]
    mov rdi, [rsp+0xB0]
    fxrstor [rsp+0x100]
    add rsp, 0x4D8
    ret

# RtlUnwindEx(target_frame, target_ip, record, return_value, context, history): never returns.
.global seh_rtl_unwind_ex
seh_rtl_unwind_ex:
    sub rsp, 0x4D8
)" SAVE_CONTEXT("rsp") R"(
    lea rax, [rsp+0x4E0]
    mov [rsp+0x98], rax
    mov rax, [rsp+0x4D8]
    mov [rsp+0xF8], rax
    mov rdi, rsp
    call seh_unwind
    ud2

# Loads every register from a CONTEXT (rdi) and continues at its rip.
.global seh_restore_context
seh_restore_context:
    fxrstor [rdi+0x100]
    mov rax, [rdi+0x98]
    sub rax, 8
    mov rcx, [rdi+0xF8]
    mov [rax], rcx
    mov rbx, [rdi+0x90]
    mov rbp, [rdi+0xA0]
    mov rsi, [rdi+0xA8]
    mov rdx, [rdi+0x88]
    mov rcx, [rdi+0x80]
    mov r8, [rdi+0xB8]
    mov r9, [rdi+0xC0]
    mov r10, [rdi+0xC8]
    mov r11, [rdi+0xD0]
    mov r12, [rdi+0xD8]
    mov r13, [rdi+0xE0]
    mov r14, [rdi+0xE8]
    mov r15, [rdi+0xF0]
    mov rsp, rax
    mov rax, [rdi+0x78]
    mov rdi, [rdi+0xB0]
    ret
.att_syntax prefix
)");

extern "C" {
void seh_capture_context(Context *);
void seh_raise_exception();
void seh_rtl_unwind_ex();

WINAPI RuntimeFunction *k32_RtlLookupFunctionEntry(uint64_t pc, uint64_t *image_base, void *) {
    *image_base = 0;
    return lookup(pc, image_base);
}

WINAPI void *k32_RtlVirtualUnwind(DWORD type, uint64_t base, uint64_t pc, RuntimeFunction *fn, Context *c,
                                   void **handler_data, uint64_t *establisher, void *) {
    void *data = nullptr;
    void *handler = virtual_unwind(type, base, pc, fn, c, &data, establisher);
    if (handler_data) *handler_data = data;
    return handler;
}

WINAPI void k32_RtlRestoreContext(Context *c, ExceptionRecord *) { resume(c); }

WINAPI void *k32_SetUnhandledExceptionFilter(void *filter) {
    void *old = unhandled_filter;
    unhandled_filter = filter;
    return old;
}

// For C code's __try / __except / __finally: the handler data is a table of protected ranges.
WINAPI int crt___C_specific_handler(ExceptionRecord *rec, void *frame, Context *c, DispatcherContext *d) {
    struct Scope {
        uint32_t begin, end, handler, target;
    };
    auto *table = static_cast<uint32_t *>(d->handler_data);
    uint32_t count = table[0];
    auto *scopes = reinterpret_cast<Scope *>(table + 1);
    uint64_t pc = d->control_pc - d->image_base;
    for (uint32_t i = d->scope_index; i < count; i++) {
        const Scope &s = scopes[i];
        if (pc < s.begin || pc >= s.end) continue;
        if (rec->flags & EXCEPTION_UNWINDING) {
            if (s.target != 0) continue;  // __except blocks don't run while unwinding
            if ((rec->flags & EXCEPTION_TARGET_UNWIND) && d->target_ip >= d->image_base + s.begin && d->target_ip < d->image_base + s.end) break;
            d->scope_index = i + 1;
            reinterpret_cast<void(WINAPI *)(BOOL, void *)>(d->image_base + s.handler)(1, frame);  // __finally
        } else if (s.target != 0) {
            long decision = 1;  // EXCEPTION_EXECUTE_HANDLER
            if (s.handler != 1) {
                struct {
                    ExceptionRecord *rec;
                    Context *ctx;
                } pointers{rec, c};
                decision = reinterpret_cast<long(WINAPI *)(void *, void *)>(d->image_base + s.handler)(&pointers, frame);
            }
            if (decision < 0) return ExceptionContinueExecution;
            if (decision > 0) {
                using Unwind = void(WINAPI *)(void *, void *, ExceptionRecord *, uint64_t, Context *, void *);
                reinterpret_cast<Unwind>(&seh_rtl_unwind_ex)(frame, reinterpret_cast<void *>(d->image_base + s.target), rec, rec->code, c, d->history_table);
            }
        }
    }
    return ExceptionContinueSearch;
}
}  // extern "C"

#define EXPORT(name) {#name, reinterpret_cast<void *>(&k32_##name)}

namespace {
const Export exports[] = {
    {"RaiseException", reinterpret_cast<void *>(&seh_raise_exception)},
    {"RtlCaptureContext", reinterpret_cast<void *>(&seh_capture_context)},
    EXPORT(RtlLookupFunctionEntry),
    EXPORT(RtlRestoreContext),
    {"RtlUnwindEx", reinterpret_cast<void *>(&seh_rtl_unwind_ex)},
    EXPORT(RtlVirtualUnwind),
    EXPORT(SetUnhandledExceptionFilter),
};
}  // namespace

const BuiltinDll kernel32_seh_dll{"kernel32.dll", exports, sizeof exports / sizeof exports[0]};
