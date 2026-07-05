use std::env;
use std::path::PathBuf;

fn main() {
    let lib_dir = env::var_os("LIBRIME_LIB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/home/disk/Projects/librime/build-touchdeck/lib"));

    println!("cargo:rerun-if-env-changed=LIBRIME_LIB_DIR");
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=dylib=rime");
    println!(
        "cargo:rustc-link-arg-bin=touchdeck-ime=-Wl,-rpath,{}",
        lib_dir.display()
    );
}
