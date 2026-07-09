use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    tauri_build::build();

    // Compile + embed the eBPF egress probe only when the `ebpf` feature is on,
    // so the default cross-platform stable build is completely unaffected.
    if env::var_os("CARGO_FEATURE_EBPF").is_some() {
        build_ebpf_probe();
    }
}

/// Build the standalone `ebpf/` crate for the `bpfel-unknown-none` target using
/// the nightly toolchain + bpf-linker (configured by `ebpf/.cargo/config.toml`
/// and `ebpf/rust-toolchain.toml`), then copy the resulting object into OUT_DIR
/// where the userspace loader embeds it via `include_bytes_aligned!`.
fn build_ebpf_probe() {
    println!("cargo:rerun-if-changed=ebpf/src");
    println!("cargo:rerun-if-changed=ebpf/Cargo.toml");
    println!("cargo:rerun-if-changed=ebpf/.cargo/config.toml");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR not set"));
    // Isolated target dir so the nested cargo build can't deadlock on the outer
    // build's target directory.
    let target_dir = out_dir.join("ebpf-target");

    let mut cmd = Command::new("rustup");
    cmd.args(["run", "nightly", "cargo", "build", "--release"])
        .arg("--target-dir")
        .arg(&target_dir)
        .current_dir("ebpf");
    // The outer (stable) build exports RUSTUP_TOOLCHAIN / RUSTC / wrapper vars
    // that would otherwise override `rustup run nightly` and make -Zbuild-std
    // look for rust-src in the wrong toolchain. Scrub them so the nightly
    // toolchain (with rust-src) is used for the eBPF sub-build.
    for key in ["RUSTUP_TOOLCHAIN", "RUSTC", "RUSTC_WORKSPACE_WRAPPER", "CARGO", "CARGO_ENCODED_RUSTFLAGS", "RUSTFLAGS"] {
        cmd.env_remove(key);
    }
    let status = cmd
        .status()
        .expect("failed to invoke `rustup run nightly cargo build` for the eBPF probe");
    assert!(status.success(), "eBPF probe build failed");

    let obj = target_dir
        .join("bpfel-unknown-none")
        .join("release")
        .join("aetheris-ebpf");
    let dst = out_dir.join("aetheris-ebpf");
    fs::copy(&obj, &dst)
        .unwrap_or_else(|e| panic!("failed to copy {} -> {}: {e}", obj.display(), dst.display()));
}
