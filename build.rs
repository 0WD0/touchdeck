use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=LIBRIME_LIB_DIR");

    if let Some(lib_dir) = env::var_os("LIBRIME_LIB_DIR").map(PathBuf::from) {
        println!("cargo:rustc-link-search=native={}", lib_dir.display());
        println!(
            "cargo:rustc-link-arg-bin=touchdeck-ime=-Wl,-rpath,{}",
            lib_dir.display()
        );
    }
}
