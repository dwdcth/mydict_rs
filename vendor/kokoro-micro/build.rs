//! 与 crates/server/build.rs 同款：ort 预编译静态库（ORT_LIB_LOCATION）需要
//! isoc23 垫片（glibc<2.38 的 __isoc23_* 转发）。垫片以**目标文件**参与链接
//（不用静态库——CI 实测 aarch64 上归库成员不被拉取；rustc-link-arg 不跨包
//! 传播，本包自己的测试二进制需要单独挂一份）。
fn main() {
    let Ok(dir) = std::env::var("ORT_LIB_LOCATION") else {
        return;
    };
    if dir.is_empty() {
        return;
    }
    println!("cargo:rustc-link-search=native={dir}");
    println!("cargo:rerun-if-env-changed=ORT_LIB_LOCATION");

    // 垫片源在仓库根 scripts/（vendor/kokoro-micro 的 manifest 的上两级）
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
    }
}
