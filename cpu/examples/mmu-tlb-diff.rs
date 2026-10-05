//! PERMANENT TEST (M117): differential check of the stage-1 walk + TLB.
//!
//! Builds synthetic page tables (with the MMU off, so the stores are
//! plain RAM writes) and compares `translate()` against a hand-computed
//! PA in three states: cold (after a flush), on a TLB hit, and after
//! evicting the direct-mapped slot with a colliding VPN. A cache must
//! return the same answer in all three, so this catches both a wrong
//! walk and a stale/aliased entry.
//!
//! Coverage: 4K pages (three levels, offset planting, a read-only page),
//! 2M and 1G blocks, the high half with the SAME VPN as a low mapping
//! (no cross-half aliasing), and an interleave loop that hammers both
//! halves through the same slot 64 times.
//!
//! The TCR is the live kernel's shape: T0SZ=T1SZ=25 (39-bit VA),
//! TG0=0b00 (4K), TG1=0b10 (4K — note TG1 is INVERTED vs TG0), cacheable.

use pi_cpu::{Bus, Cpu};

// T0SZ=25 (39-bit low), TG0=0b00=4K, T1SZ=25, TG1=0b10=4K, IRGN/ORGN/SH=3
const TCR: u64 = 0x8019_3F19;

/// LPAE index of `va` at `shift`.
fn idx(va: u64, shift: u32) -> u64 {
    (va >> shift) & 0x1ff
}

fn wr(bus: &mut Bus, pa: u64, v: u64) {
    bus.write(pa, 8, v).expect("table write");
}

fn table(next_pa: u64) -> u64 {
    next_pa | 0b11
}
fn page(pa: u64, ap: u64) -> u64 {
    (pa & 0x0000_ffff_ffff_f000) | 0b11 | (ap << 6) | (1 << 10) // AF
}
fn block(pa: u64, ap: u64) -> u64 {
    (pa & 0x0000_ffff_ffff_f000) | 0b01 | (ap << 6) | (1 << 10)
}

/// Map `va` -> `pa` with a 4K page, creating the three levels under the
/// root table at `root`. vabits=39 => shifts 30 / 21 / 12.
fn map4k(bus: &mut Bus, root: u64, l2_base: u64, l3_base: u64, va: u64, pa: u64, ap: u64) {
    wr(bus, root + idx(va, 30) * 8, table(l2_base));
    wr(bus, l2_base + idx(va, 21) * 8, table(l3_base));
    wr(bus, l3_base + idx(va, 12) * 8, page(pa, ap));
}

fn map2m(bus: &mut Bus, root: u64, l2_base: u64, va: u64, pa: u64, ap: u64) {
    wr(bus, root + idx(va, 30) * 8, table(l2_base));
    wr(bus, l2_base + idx(va, 21) * 8, block(pa, ap));
}

fn map1g(bus: &mut Bus, root: u64, va: u64, pa: u64, ap: u64) {
    wr(bus, root + idx(va, 30) * 8, block(pa, ap));
}

fn check(name: &str, bus: &mut Bus, va: u64, want: u64, fails: &mut u32) {
    bus.tlb_flush();
    let cold = bus.translate(va);
    let hot = bus.translate(va);
    // Evict the direct-mapped slot: same slot, different VPN.
    let evict_va = (va & !0xfff) ^ (1 << 19);
    let _ = bus.translate(evict_va);
    let after = bus.translate(va);
    let ok = cold == Ok(want) && hot == Ok(want) && after == Ok(want);
    if !ok {
        *fails += 1;
    }
    println!(
        "{:<24} va=0x{:012x} cold={:<18} hot={:<18} evicted={:<18} want=0x{:x} {}",
        name,
        va,
        format!("{:?}", cold.map(|p| format!("0x{:x}", p))),
        format!("{:?}", hot.map(|p| format!("0x{:x}", p))),
        format!("{:?}", after.map(|p| format!("0x{:x}", p))),
        want,
        if ok { "ok" } else { "FAIL" }
    );
}

fn main() {
    let mut bus = Bus::new();
    let _cpu = Cpu::new(0);
    bus.mmu_sctlr = 0; // MMU off while the tables are built
    bus.mmu_tcr = TCR;
    bus.mmu_ttbr0 = 0x10_0000;
    bus.mmu_ttbr1 = 0x11_0000;

    // Table bases, well clear of each other.
    let l2 = 0x10_1000u64;
    let l3 = 0x10_2000u64;
    let l2b = 0x10_3000u64;
    let hl2 = 0x11_1000u64;
    let hl3 = 0x11_2000u64;

    // --- 4K pages, low half ---
    map4k(&mut bus, 0x10_0000, l2, l3, 0x61c000, 0x0017_7000, 0b11);
    map4k(&mut bus, 0x10_0000, l2, l3, 0x61c268, 0x0017_7000, 0b11);
    map4k(&mut bus, 0x10_0000, l2, l3, 0x623800, 0x0020_0000, 0b11);
    map4k(&mut bus, 0x10_0000, l2, l3, 0x61d000, 0x0044_4000, 0b10);
    // --- 2M block, low half ---
    // 2M block under its OWN L1 slot (a 2M-block VA below 1G shares
    // L1 idx 0 with the low 4K mappings).
    map2m(&mut bus, 0x10_0000, l2b, 0x5000_0000, 0x00a0_0000, 0b11);
    // --- 1G block, low half ---
    map1g(&mut bus, 0x10_0000, 0x8000_0200, 0x4000_0000, 0b11); // L1 idx 2, 1G-aligned PA
    // --- 4K page, high half, same VPN as a low mapping ---
    let hva = 0xffff_ff80_0000_0000u64 | 0x61c000; // 39-bit high half
    map4k(&mut bus, 0x11_0000, hl2, hl3, hva, 0x0033_3000, 0b11);

    bus.mmu_sctlr = 1; // MMU on

    let mut fails = 0;
    check("4K page off=0", &mut bus, 0x61c000, 0x177000, &mut fails);
    check("4K page off=0x268", &mut bus, 0x61c268, 0x177268, &mut fails);
    check("4K page off=0xfff", &mut bus, 0x61cfff, 0x177fff, &mut fails);
    check("4K page second", &mut bus, 0x623800, 0x200800, &mut fails); // offset 0x800 planted
    check("RO page translate", &mut bus, 0x61d000, 0x444000, &mut fails);
    check("2M block", &mut bus, 0x5000_0000, 0xa00000, &mut fails);
    check("2M block +off", &mut bus, 0x5012_3456, 0xb23456, &mut fails); // 2M block base 0xa00000 + 0x123456
    check("1G block", &mut bus, 0x8000_0200, 0x4000_0200, &mut fails);
    check("high half 4K", &mut bus, hva, 0x333000, &mut fails);

    // Interleave both halves on the same slot repeatedly: a cache must
    // keep answering with the right half's PA.
    bus.tlb_flush();
    for i in 0..64 {
        let want_lo = 0x0017_7000u64;
        let _ = bus.translate(0x61c000 + (i & 0xf) * 8);
        let a = bus.translate(0x61c000);
        let b = bus.translate(hva);
        if a != Ok(want_lo) || b != Ok(0x333000) {
            fails += 1;
            println!("interleave FAIL i={} a={:?} b={:?}", i, a, b);
            break;
        }
    }

    println!("--- walk_dump for the high-half VA ---");
    println!("{}", bus.walk_dump(hva));
    println!("hroot[{}] = 0x{:x}", idx(hva, 30), bus.mem_u64_dbg_pub(0x11_0000 + idx(hva, 30) * 8));
    println!("hl2[{}] = 0x{:x}", idx(hva, 21), bus.mem_u64_dbg_pub(hl2 + idx(hva, 21) * 8));
    println!("--- walk_dump for the first low 4K VA ---");
    println!("{}", bus.walk_dump(0x61c000));
    println!("root[{}] = 0x{:x}", idx(0x61c000, 30), bus.mem_u64_dbg_pub(0x10_0000 + idx(0x61c000, 30) * 8));
    println!("l2[{}] = 0x{:x}", idx(0x61c000, 21), bus.mem_u64_dbg_pub(l2 + idx(0x61c000, 21) * 8));
    println!("l3[{}] = 0x{:x}", idx(0x61c000, 12), bus.mem_u64_dbg_pub(l3 + idx(0x61c000, 12) * 8));
    println!("mmu-tlb-diff fails={}", fails);
    if fails > 0 {
        std::process::exit(1);
    }
}