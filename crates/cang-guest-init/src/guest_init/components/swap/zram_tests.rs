use super::*;
use std::cell::RefCell;
use std::time::Duration;

const MEMINFO_8_GIB: &str = "MemTotal:        8388608 kB\nMemFree:         1048576 kB\n";
const EIGHT_GIB: u64 = 8 * 1024 * 1024 * 1024;
const FOUR_GIB: u64 = 4 * 1024 * 1024 * 1024;
const TWO_GIB: u64 = 2 * 1024 * 1024 * 1024;

#[test]
fn plan_swap_capacity_matches_guest_ram() {
    assert_eq!(plan_swap_capacity(EIGHT_GIB), EIGHT_GIB);
}

#[test]
fn plan_swap_capacity_floors_tiny_guests() {
    assert_eq!(plan_swap_capacity(64 * 1024 * 1024), MIN_SWAP_BYTES);
}

#[test]
fn plan_mem_limit_bounds_the_device_to_a_quarter_of_guest_ram() {
    assert_eq!(plan_mem_limit(EIGHT_GIB), TWO_GIB);
}

#[test]
fn plan_mem_limit_stays_below_the_capacity_it_bounds() {
    for mem_total_bytes in [
        128 * 1024 * 1024,
        1024 * 1024 * 1024,
        4 * 1024 * 1024 * 1024,
        64 * 1024 * 1024 * 1024,
    ] {
        let capacity = plan_swap_capacity(mem_total_bytes);
        let limit = plan_mem_limit(mem_total_bytes);

        assert!(limit < capacity, "{limit} should stay below {capacity}");
    }
}

#[test]
fn parse_mem_total_reads_kibibytes_from_meminfo() {
    assert_eq!(
        parse_mem_total(MEMINFO_8_GIB).unwrap(),
        8 * 1024 * 1024 * 1024
    );
}

#[test]
fn parse_mem_total_rejects_meminfo_without_the_line() {
    let err = parse_mem_total("MemFree: 1024 kB\n").expect_err("MemTotal is required");

    assert!(format!("{err:#}").contains("MemTotal"));
}

#[test]
fn swap_active_matches_the_device_column_only() {
    let swaps = "Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n\
                 /dev/zram0                              partition\t4194300\t0\t100\n";

    assert!(swap_active(swaps, DEVICE));
    assert!(!swap_active(swaps, "/dev/zram1"));
    assert!(!swap_active(
        "Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n",
        DEVICE
    ));
}

#[test]
fn activation_sizes_signs_and_activates_the_zram_device() {
    let backend = FakeSwapBackend::new();

    let report = activate_with(&backend).expect("zram activation should succeed");

    assert_eq!(report.state, GuestSwapState::Ready);
    assert_eq!(report.size_bytes, Some(EIGHT_GIB));
    assert_eq!(report.mem_limit_bytes, Some(TWO_GIB));
    assert_eq!(
        backend.operations(),
        [
            "exists:/sys/block/zram0/disksize",
            "read:/proc/swaps",
            "read:/proc/meminfo",
            "write:/sys/block/zram0/disksize=8589934592",
            "write:/sys/block/zram0/mem_limit=2147483648",
            "run:mkswap /dev/zram0",
            "run:swapon -p 100 /dev/zram0",
        ]
    );
}

#[test]
fn activation_reports_the_active_device_without_resizing_it() {
    let backend = FakeSwapBackend::with_active_device("4294967296\n", "1048576\n");

    let report = activate_with(&backend).expect("an active device is not an error");

    assert_eq!(report.state, GuestSwapState::Ready);
    assert_eq!(report.size_bytes, Some(FOUR_GIB));
    assert_eq!(report.mem_limit_bytes, Some(1024 * 1024));
    assert_eq!(
        backend.operations(),
        [
            "exists:/sys/block/zram0/disksize",
            "read:/proc/swaps",
            "read:/sys/block/zram0/disksize",
            "read:/sys/block/zram0/mem_limit",
        ]
    );
}

/// The worker must not size the device while the enter path is still running:
/// measured beside it, the CPU-bound sizing slowed that path by its own cost
/// one-for-one. `--after-exec` is a gate, so the very first thing the activation
/// does is wait.
/// The gate is the change's whole mechanism: the worker is spawned during
/// `enter` and must not act until the enter process lets it.
#[test]
fn the_exec_gate_blocks_the_worker_until_the_hold_is_released() {
    let ExecGate {
        worker_end,
        enter_end,
    } = exec_gate_pipe().expect("the gate pipe should be creatable");
    let reader = std::thread::spawn(move || read_to_eof(worker_end));

    std::thread::sleep(Duration::from_millis(200));
    assert!(
        !reader.is_finished(),
        "the worker must stay blocked while the enter process holds the gate"
    );
    drop(enter_end);
    assert!(
        reader.join().is_ok(),
        "closing the hold must let the worker read its EOF and continue"
    );
}

fn read_to_eof(mut file: std::fs::File) -> usize {
    let mut byte = [0u8; 1];
    let mut read_bytes = 0usize;
    while let Ok(read) = file.read(&mut byte) {
        if read == 0 {
            break;
        }
        read_bytes += read;
    }
    read_bytes
}

#[test]
fn activation_waits_for_the_enter_process_to_exec_first() {
    let backend = FakeSwapBackend::new();
    ensure_gated(&backend, || {
        backend.record("gate: wait for the enter process to exec")
    });

    let operations = backend.operations();
    assert_eq!(
        operations.first().map(String::as_str),
        Some("gate: wait for the enter process to exec"),
        "waiting is the first thing the worker does: {operations:?}"
    );
    assert_eq!(backend.status().lines().next(), Some("state=ready"));
}

#[test]
fn ensure_records_a_ready_device_in_the_status_file() {
    let backend = FakeSwapBackend::new();

    ensure_gated(&backend, || {});

    assert_eq!(
        backend.status(),
        format!(
            "state=ready\ndevice={DEVICE}\nsize_bytes={EIGHT_GIB}\nmem_limit_bytes={TWO_GIB}\n\
             priority={SWAP_PRIORITY}\n"
        )
    );
}

#[test]
fn ensure_treats_a_kernel_without_zram_as_unavailable_and_boots_anyway() {
    let backend = FakeSwapBackend::without_zram();

    ensure_gated(&backend, || {});

    assert_eq!(
        backend.status(),
        format!("state=unavailable\ndevice={DEVICE}\npriority={SWAP_PRIORITY}\n")
    );
    assert_eq!(
        backend.operations(),
        [
            "exists:/sys/block/zram0/disksize",
            "status:state=unavailable",
        ]
    );
}

#[test]
fn ensure_records_a_failed_activation_without_failing_the_session() {
    let backend = FakeSwapBackend::failing_on("mkswap");

    ensure_gated(&backend, || {});

    let status = backend.status();
    assert!(status.contains("state=failed\n"), "status was: {status}");
    assert!(status.contains("error="), "status was: {status}");
    assert!(status.contains("mkswap"), "status was: {status}");
}

struct FakeSwapBackend {
    operations: RefCell<Vec<String>>,
    status: RefCell<Option<String>>,
    zram_present: bool,
    swaps: RefCell<String>,
    meminfo: RefCell<String>,
    device_size: RefCell<Option<String>>,
    mem_limit: RefCell<Option<String>>,
    failing: RefCell<Option<String>>,
}

impl FakeSwapBackend {
    fn new() -> Self {
        Self {
            operations: RefCell::new(Vec::new()),
            status: RefCell::new(None),
            zram_present: true,
            swaps: RefCell::new("Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n".to_owned()),
            meminfo: RefCell::new(MEMINFO_8_GIB.to_owned()),
            device_size: RefCell::new(None),
            mem_limit: RefCell::new(None),
            failing: RefCell::new(None),
        }
    }

    fn without_zram() -> Self {
        Self {
            zram_present: false,
            ..Self::new()
        }
    }

    fn with_active_device(size: &str, mem_limit: &str) -> Self {
        let backend = Self::new();
        *backend.swaps.borrow_mut() = format!(
            "Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n\
             {DEVICE}                              partition\t4194300\t0\t100\n"
        );
        *backend.device_size.borrow_mut() = Some(size.to_owned());
        *backend.mem_limit.borrow_mut() = Some(mem_limit.to_owned());
        backend
    }

    fn failing_on(program: &str) -> Self {
        let backend = Self::new();
        *backend.failing.borrow_mut() = Some(program.to_owned());
        backend
    }

    fn record(&self, operation: impl Into<String>) {
        self.operations.borrow_mut().push(operation.into());
    }

    fn operations(&self) -> Vec<String> {
        self.operations.borrow().clone()
    }

    fn status(&self) -> String {
        self.status.borrow().clone().unwrap_or_default()
    }
}

impl SwapBackend for FakeSwapBackend {
    fn path_exists(&self, path: &Path) -> bool {
        self.record(format!("exists:{}", path.display()));
        self.zram_present
    }

    fn read(&self, path: &Path) -> Result<String> {
        self.record(format!("read:{}", path.display()));
        match path.to_str() {
            Some(SWAPS_PATH) => Ok(self.swaps.borrow().clone()),
            Some(MEMINFO_PATH) => Ok(self.meminfo.borrow().clone()),
            Some(DEVICE_SIZE_PATH) => self
                .device_size
                .borrow()
                .clone()
                .ok_or_else(|| anyhow!("zram device has no size")),
            Some(MEM_LIMIT_PATH) => self
                .mem_limit
                .borrow()
                .clone()
                .ok_or_else(|| anyhow!("zram device has no memory limit")),
            other => Err(anyhow!("unexpected read of {other:?}")),
        }
    }

    fn write(&self, path: &Path, value: &str) -> Result<()> {
        self.record(format!("write:{}={}", path.display(), value.trim()));
        if self.failing.borrow().as_deref() == Some("write") {
            return Err(anyhow!("failed to write {}", path.display()));
        }
        match path.to_str() {
            Some(DEVICE_SIZE_PATH) => {
                *self.device_size.borrow_mut() = Some(value.trim().to_owned())
            }
            Some(MEM_LIMIT_PATH) => *self.mem_limit.borrow_mut() = Some(value.trim().to_owned()),
            other => return Err(anyhow!("unexpected write to {other:?}")),
        }
        Ok(())
    }

    fn run(&self, program: &str, args: &[&str]) -> Result<()> {
        self.record(format!("run:{} {}", program, args.join(" ")));
        if self.failing.borrow().as_deref() == Some(program) {
            return Err(anyhow!("{program} exited with status 1"));
        }
        Ok(())
    }

    fn record_status(&self, contents: &str) {
        self.record(format!(
            "status:{}",
            contents.lines().next().unwrap_or_default()
        ));
        *self.status.borrow_mut() = Some(contents.to_owned());
    }
}
