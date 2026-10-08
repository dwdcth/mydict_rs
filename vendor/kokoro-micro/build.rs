//! 与 crates/server/build.rs 同款：ort 预编译静态库（ORT_LIB_LOCATION）需要
//! isoc23 垫片（glibc<2.38 的 __isoc23_* 转发），测试/示例二进制也要链上。
fn main() {
    if let Ok(dir) = std::env::var("ORT_LIB_LOCATION") {
        if !dir.is_empty() {
            println!("cargo:rustc-link-search=native={dir}");
            println!("cargo:rustc-link-lib=static=isoc23shim");
            println!("cargo:rerun-if-env-changed=ORT_LIB_LOCATION");
        }
    }
}
