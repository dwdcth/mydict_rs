//! ort 预编译静态库（ORT_LIB_LOCATION 指向 .cache/onnxruntime-123）的链接补充。
//!
//! pyke 构建的 onnxruntime 引用 glibc>=2.38 的 __isoc23_* 符号，本仓库附带
//! 垫片（scripts/isoc23_shim.c → 转发到普通 strtol 族）把 glibc 要求降回 2.34。
//!
//! 垫片以**目标文件**（编译进 OUT_DIR + `cargo:rustc-link-arg`）参与最终链接，
//! 不用静态库：显式 .o 全量链接，与归档拉取语义/链接器实现无关（CI 实测
//! `-l static=isoc23shim` 在 aarch64 上声明齐全但成员不被拉取、符号仍 UND，
//! x86_64 同指令却正常；两种机制同时开还会 duplicate symbol）。
fn main() {
    let Ok(dir) = std::env::var("ORT_LIB_LOCATION") else {
        return;
    };
    if dir.is_empty() {
        return;
    }
    println!("cargo:rustc-link-search=native={dir}");
    println!("cargo:rerun-if-env-changed=ORT_LIB_LOCATION");

    // 垫片源在仓库根 scripts/（server 包的 manifest 的上两级）
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let shim_src = std::path::Path::new(&manifest)
        .join("../..")
        .join("scripts/isoc23_shim.c");
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let shim_obj = std::path::Path::new(&out_dir).join("isoc23_shim.o");
    let ok = std::process::Command::new("cc")
        .arg("-c")
        .arg(&shim_src)
        .arg("-o")
        .arg(&shim_obj)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok {
        println!("cargo:rustc-link-arg={}", shim_obj.display());
        println!("cargo:rerun-if-changed={}", shim_src.display());
    } else {
        println!("cargo:warning=isoc23 垫片编译失败（cc 不可用？），glibc 兼容回退失效");
    }
}
