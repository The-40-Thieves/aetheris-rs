#![no_std]
#![no_main]

//! aetheris egress byte-accounting eBPF probe.
//!
//! Two kprobes on the kernel send paths — `tcp_sendmsg(sk, msg, size)` and
//! `udp_sendmsg(sk, msg, len)` — accumulate the requested byte count per process
//! (tgid) into a HashMap the userspace side reads. These run in the *sending
//! process's* context, so `bpf_get_current_pid_tgid()` reliably identifies the
//! owner (unlike cgroup_skb egress, which runs in softirq). The `size` argument
//! is the application-layer payload requested — not wire bytes (no IP/TCP
//! headers, no retransmits) — which is the right granularity for "how much data
//! did this process send".

use aya_ebpf::{
    helpers::bpf_get_current_pid_tgid,
    macros::{kprobe, map},
    maps::HashMap,
    programs::ProbeContext,
};

/// tgid (process id) -> cumulative egress payload bytes.
#[map]
static EGRESS_BYTES: HashMap<u32, u64> = HashMap::with_max_entries(16384, 0);

#[kprobe]
pub fn tcp_sendmsg(ctx: ProbeContext) -> u32 {
    account(&ctx);
    0
}

#[kprobe]
pub fn udp_sendmsg(ctx: ProbeContext) -> u32 {
    account(&ctx);
    0
}

#[inline(always)]
fn account(ctx: &ProbeContext) {
    // Arg 2 (0-indexed) of both send paths is the byte count (size_t).
    let size: usize = match ctx.arg(2) {
        Some(s) => s,
        None => return,
    };
    let tgid = (bpf_get_current_pid_tgid() >> 32) as u32;
    if tgid == 0 {
        return;
    }
    let bytes = size as u64;
    unsafe {
        if let Some(counter) = EGRESS_BYTES.get_ptr_mut(&tgid) {
            *counter += bytes;
        } else {
            let _ = EGRESS_BYTES.insert(&tgid, &bytes, 0);
        }
    }
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
