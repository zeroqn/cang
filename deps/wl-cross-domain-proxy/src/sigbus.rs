use libc::{c_int, c_void};
use log::error;
use std::{
    ptr,
    sync::atomic::{self, Ordering},
};

pub struct SigbusWatcher {
    addr: atomic::AtomicPtr<c_void>,
    size: atomic::AtomicUsize,
    faulted: atomic::AtomicBool,
    // NOTE: We use atomics as the simplest interior mutability, there is no mutli-threading
}

impl SigbusWatcher {
    pub fn watch(&'static self, addr: *mut c_void, size: usize) -> SigbusWatchGuard {
        if addr == ptr::null_mut() {
            panic!("sigbus: trying to watch a null address");
        }
        if self.addr.load(Ordering::Relaxed) != ptr::null_mut() {
            // we're single threaded, so this should never happen
            panic!("sigbus: trying to watch an address while another is being watched");
        }
        self.addr.store(addr, Ordering::Relaxed);
        self.size.store(size, Ordering::Relaxed);
        self.faulted.store(false, Ordering::Relaxed);
        SigbusWatchGuard { watch: &self }
    }
}

pub static SIGBUS_WATCHER: SigbusWatcher = SigbusWatcher {
    addr: atomic::AtomicPtr::new(ptr::null_mut()),
    size: atomic::AtomicUsize::new(0),
    faulted: atomic::AtomicBool::new(false),
};

pub struct SigbusWatchGuard {
    watch: &'static SigbusWatcher,
}

impl SigbusWatchGuard {
    pub fn has_faulted(self) -> bool {
        self.watch.faulted.load(Ordering::Relaxed)
    }
}

impl Drop for SigbusWatchGuard {
    fn drop(&mut self) {
        self.watch.addr.store(ptr::null_mut(), Ordering::Relaxed);
        // This guarantees our handler will just rerun the original handler
    }
}

static mut ORIG_HANDLER: libc::sigaction = unsafe { std::mem::zeroed() };

extern "C" fn shm_pool_sigbus_handler(
    _signum: c_int,
    info: *const libc::siginfo_t,
    _context: *const c_void,
) {
    // EVERYTHING here needs to be async-signal-safe. Do not log!
    // (Sure, mmap isn't guaranteed by POSIX to be safe, but it is in practice. It's just a syscall wrapper.
    //  And it's the only way to do this at all. This is what libwayland does.)
    let fault_addr = unsafe { (*info).si_addr() };
    let start_addr = SIGBUS_WATCHER.addr.load(Ordering::Relaxed);
    let size = SIGBUS_WATCHER.size.load(Ordering::Relaxed);
    if start_addr != ptr::null_mut()
        && fault_addr >= start_addr
        && fault_addr < start_addr.wrapping_byte_add(size)
        && unsafe {
            libc::mmap(
                start_addr,
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_FIXED | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        } != libc::MAP_FAILED
    {
        SIGBUS_WATCHER.faulted.store(true, Ordering::Relaxed);
    } else {
        unsafe {
            libc::sigaction(libc::SIGBUS, &raw const ORIG_HANDLER, ptr::null_mut());
            libc::raise(libc::SIGBUS);
        }
    }
}

pub fn install_handler() {
    let mut new_handler: libc::sigaction = unsafe { std::mem::zeroed() };
    unsafe { libc::sigemptyset(&raw mut new_handler.sa_mask) };
    new_handler.sa_sigaction = shm_pool_sigbus_handler as *const c_void as usize;
    new_handler.sa_flags = libc::SA_SIGINFO | libc::SA_NODEFER;
    if unsafe { libc::sigaction(libc::SIGBUS, &raw const new_handler, &raw mut ORIG_HANDLER) } != 0
    {
        error!("failed to install SIGBUS handler");
    }
}
