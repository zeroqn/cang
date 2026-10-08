//! Export cang's `dlopen` interposer (see `src/va_driver.rs`) to the dynamic
//! symbol table.
//!
//! The VM worker is the `cang` binary itself, so a definition in the executable
//! wins the global symbol lookup that libva's own `dlopen` call performs - but
//! only if the linker puts the definition in `.dynsym`. `--export-dynamic-symbol`
//! does that for this one symbol instead of exporting every Rust symbol.

fn main() {
    println!("cargo:rustc-link-arg-bin=cang=-Wl,--export-dynamic-symbol=dlopen");
}
