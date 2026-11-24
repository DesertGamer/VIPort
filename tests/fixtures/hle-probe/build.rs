fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu") {
        println!("cargo:rustc-link-arg-cdylib=-Wl,--delayload=victory-probe-delayed.dll");
    } else {
        println!("cargo:rustc-link-arg-cdylib=/DELAYLOAD:victory-probe-delayed.dll");
    }
    println!("cargo:rustc-link-lib=delayimp");
}
