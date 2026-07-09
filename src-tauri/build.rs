use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    tauri_build::build();

    // Compile + embed the eBPF egress probe only when the `ebpf` feature is on,
    // so the default cross-platform stable build is completely unaffected.
    if env::var_os("CARGO_FEATURE_EBPF").is_some() {
        build_ebpf_probe();
    }
}

/// Build the standalone `ebpf/` crate for `bpfel-unknown-none` using the nightly
/// toolchain + bpf-linker (configured by `ebpf/.cargo/config.toml` and
/// `ebpf/rust-toolchain.toml`), then copy the object into OUT_DIR where the
/// userspace loader embeds it via `include_bytes_aligned!`.
fn build_ebpf_probe() {
    println!("cargo:rerun-if-changed=ebpf/src");
    println!("cargo:rerun-if-changed=ebpf/Cargo.toml");
    println!("cargo:rerun-if-changed=ebpf/Cargo.lock");
    println!("cargo:rerun-if-changed=ebpf/rust-toolchain.toml");
    println!("cargo:rerun-if-changed=ebpf/.cargo/config.toml");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR not set"));
    // Isolated target dir so the nested cargo build can't contend with the outer
    // build's target directory.
    let target_dir = out_dir.join("ebpf-target");

    // Derive the BPF target arch from the host's build arch so the probe's
    // per-arch pt_regs register access is correct everywhere (not hardcoded to
    // one arch). aya-ebpf supports aarch64/x86_64/arm/riscv64/... via this cfg.
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_else(|_| "x86_64".to_string());
    let arch = if arch.starts_with("riscv64") { "riscv64".to_string() } else { arch };
    // CARGO_ENCODED_RUSTFLAGS uses \x1f (unit separator) between flags. Setting it
    // overrides the ebpf crate's config.toml rustflags so the arch is dynamic.
    let sep = "\u{1f}";
    let rustflags = [
        format!("--cfg=bpf_target_arch=\"{arch}\""),
        "-Cdebuginfo=2".to_string(),
        "-Clink-arg=--btf".to_string(),
    ]
    .join(sep);

    let mut cmd = Command::new("rustup");
    cmd.args(["run", "nightly", "cargo", "build", "--release"])
        .arg("--target-dir")
        .arg(&target_dir)
        .current_dir("ebpf");
    // Scrub the outer (stable) build's toolchain/flag env so `rustup run nightly`
    // and -Zbuild-std use nightly's rust-src, then set our own dynamic rustflags.
    for key in ["RUSTUP_TOOLCHAIN", "RUSTC", "RUSTC_WORKSPACE_WRAPPER", "CARGO", "RUSTFLAGS"] {
        cmd.env_remove(key);
    }
    cmd.env("CARGO_ENCODED_RUSTFLAGS", rustflags);

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
