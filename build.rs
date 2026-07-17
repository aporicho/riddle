fn main() {
    if std::env::var("CARGO_FEATURE_TAKEOVER").is_ok() {
        // libquill.so + libqsgepaper.so from the quill project.
        let quill = std::env::var("QUILL_DIR")
            .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../quill").into());
        println!("cargo:rerun-if-env-changed=QUILL_DIR");
        println!("cargo:rustc-link-search=native={quill}/build");
        println!("cargo:rustc-link-search=native={quill}/vendor");
        println!("cargo:rustc-link-lib=dylib=quill");
        println!("cargo:rustc-link-lib=dylib=qsgepaper");
        println!("cargo:rustc-link-arg=-Wl,-rpath,/home/root/quill:/usr/lib/plugins/scenegraph");
        // Resolve libquill's transitive Qt deps at link time from the SDK
        // sysroot. rpath-link only (NOT link-search: the SDK's libc/libm are
        // linker scripts with absolute paths that break outside --sysroot).
        let sysroot = std::env::var("SDKTARGETSYSROOT").or_else(|_| {
            let sdk = std::env::var("RM_SDK")
                .or_else(|_| std::env::var("HOME").map(|home| format!("{home}/rm-sdk-3.26")))?;
            Ok::<_, std::env::VarError>(format!("{sdk}/sysroots/cortexa53-crypto-remarkable-linux"))
        });
        println!("cargo:rerun-if-env-changed=RM_SDK");
        println!("cargo:rerun-if-env-changed=SDKTARGETSYSROOT");
        if let Ok(sysroot) = sysroot {
            println!("cargo:rustc-link-arg=-Wl,-rpath-link,{sysroot}/usr/lib");
        }
    }
}
