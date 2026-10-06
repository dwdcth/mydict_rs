//! ort 预编译静态库（ORT_LIB_LOCATION 指向 .cache/onnxruntime-123）的链接补充：
//! pyke 构建的 onnxruntime 引用 glibc>=2.38 的 __isoc23_* 符号，本仓库附带
//! 垫片静态库（isoc23_shim.c → libisoc23shim.a）转发到普通 strtol 族。
fn main() {
    if let Ok(dir) = std::env::var("ORT_LIB_LOCATION") {
        if !dir.is_empty() {
            println!("cargo:rustc-link-search=native={dir}");
            println!("cargo:rustc-link-lib=static=isoc23shim");
            println!("cargo:rerun-if-env-changed=ORT_LIB_LOCATION");
        }
    }
}
