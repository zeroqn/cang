// Anonymous presser for a responsiveness / memory-pressure arm.
//
// Compiled in-guest with `rustc -O presser.rs -o presser`; usage and knobs are in
// tools/responsive-arm/README.md.
//
// v1 filled each page with zeros (vec![0u8; n] plus one touched byte per page)
// and never reached a wall: linux-7.3.5's zram_write_page() detects a page whose
// every byte is identical (page_same_filled) and stores only the fill value
// without allocating from the pool, so the presser's 8.2 GiB of identical pages
// cost zram nothing (mm_stat same_pages 5 841, mem_used_max 16 MiB).
//
// v2 fills every page with its own xorshift32 byte stream, so pages are distinct
// and effectively incompressible: the worst case for a zram budget. Each chunk
// logs the process's own RSS and VmSwap, which is the guest's own readback of
// how much of the set is resident and how much is in zram.
use std::fs::OpenOptions;
use std::io::Write;

fn status_kb(field: &str) -> u64 {
    let Ok(s) = std::fs::read_to_string("/proc/self/status") else { return 0 };
    let prefix = format!("{field}:");
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix(&prefix) {
            return rest.split_whitespace().next().and_then(|v| v.parse().ok()).unwrap_or(0);
        }
    }
    0
}

fn main() {
    let chunk_mb: usize = std::env::var("CHUNK_MB").ok().and_then(|s| s.parse().ok()).unwrap_or(24);
    let max_mb: usize = std::env::var("MAX_MB").ok().and_then(|s| s.parse().ok()).unwrap_or(8192);
    let sleep_ms: u64 = std::env::var("SLEEP_MS").ok().and_then(|s| s.parse().ok()).unwrap_or(100);
    let progress = std::env::var("PROGRESS").unwrap_or_else(|_| "/workspace/out/press-progress.txt".to_string());
    let mut f = match OpenOptions::new().create(true).append(true).open(&progress) {
        Ok(file) => Box::new(file) as Box<dyn Write>,
        // A progress path the guest cannot create is not a reason to lose the arm;
        // the same lines go to stderr, which the payload's own log captures.
        Err(err) => {
            eprintln!("presser: cannot write {progress}: {err}; progress goes to stderr");
            Box::new(std::io::stderr())
        }
    };
    writeln!(f, "PRESS START chunk_mb={chunk_mb} max_mb={max_mb} sleep_ms={sleep_ms} pages=distinct").ok();
    f.flush().ok();
    let chunk = chunk_mb * 1024 * 1024;
    let mut blocks: Vec<Vec<u8>> = Vec::new();
    let mut alloc_mb: usize = 0;
    let mut page: u32 = 0;
    while alloc_mb < max_mb {
        let mut b = vec![0u8; chunk];
        let mut off = 0usize;
        while off < chunk {
            let mut st: u32 = (page.wrapping_mul(2654435761) ^ 0x9e3779b9) | 1;
            for j in 0..4096usize {
                st ^= st << 13;
                st ^= st >> 17;
                st ^= st << 5;
                unsafe { std::ptr::write_volatile(b.as_mut_ptr().add(off + j), st as u8) };
            }
            off += 4096;
            page = page.wrapping_add(1);
        }
        blocks.push(b);
        alloc_mb += chunk_mb;
        writeln!(
            f,
            "PRESS alloc_mb={alloc_mb} rss_kb={} vmswap_kb={}",
            status_kb("VmRSS"),
            status_kb("VmSwap")
        )
        .ok();
        f.flush().ok();
        std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
    }
    writeln!(f, "PRESS DONE alloc_mb={alloc_mb} rss_kb={} vmswap_kb={}", status_kb("VmRSS"), status_kb("VmSwap")).ok();
    f.flush().ok();
    let hold: u64 = std::env::var("HOLD_S").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    std::thread::sleep(std::time::Duration::from_secs(hold));
    drop(blocks);
    writeln!(f, "PRESS RELEASED alloc_mb={alloc_mb}").ok();
    f.flush().ok();
}
