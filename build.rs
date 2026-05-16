use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let external_dir = manifest_dir.join("external");

    println!("cargo:rerun-if-changed=external/CMakeLists.txt");
    println!("cargo:rerun-if-changed=wrapper.h");

    // Drive cmake on external/. First build runs FetchContent (needs network);
    // sources are cached under target/.../build/_deps for subsequent builds.
    let dst = cmake::Config::new(&external_dir)
        .build_target("libfuji")
        .build();

    let build_dir = dst.join("build");
    let deps_root = build_dir.join("_deps");
    let libfuji_build = deps_root.join("libfuji-build");
    let libpict_build = deps_root.join("libpict-build");
    let fp_build = deps_root.join("fp-build");

    println!("cargo:rustc-link-search=native={}", libfuji_build.display());
    println!("cargo:rustc-link-search=native={}", libpict_build.display());
    println!("cargo:rustc-link-search=native={}", fp_build.display());

    // CMake target `libfuji` -> file `liblibfuji.a`, link name `libfuji`.
    println!("cargo:rustc-link-lib=static=libfuji");
    // libpict has OUTPUT_NAME=pict -> file `libpict.a`, link name `pict`.
    println!("cargo:rustc-link-lib=static=pict");
    // fp -> `libfp.a`, link name `fp`.
    println!("cargo:rustc-link-lib=static=fp");

    // Dynamic deps. libpict's USB backend references libusb-1.0; fp pulls libxml2.
    probe_dynamic("libusb-1.0", "usb-1.0");
    probe_dynamic("libxml-2.0", "xml2");

    println!("cargo:rustc-link-lib=dylib=pthread");

    // Locate headers for bindgen. cmake-rs places fetched sources at
    // $OUT_DIR/build/_deps/<name>-src/.
    let libfuji_src = deps_root.join("libfuji-src").join("lib");
    let libpict_src = deps_root.join("libpict-src").join("src");

    let bindings = bindgen::Builder::default()
        .header(manifest_dir.join("wrapper.h").to_string_lossy())
        .clang_arg(format!("-I{}", libfuji_src.display()))
        .clang_arg(format!("-I{}", libpict_src.display()))
        .allowlist_function("fuji_.*")
        .allowlist_function("ptp_.*")
        .allowlist_function("ptpip_.*")
        .allowlist_type("Ptp.*")
        .allowlist_type("Fuji.*")
        .allowlist_var("FUJI_.*")
        .allowlist_var("PTP_.*")
        .default_enum_style(bindgen::EnumVariation::ModuleConsts)
        .derive_debug(true)
        .derive_default(true)
        .generate_comments(false)
        .layout_tests(false)
        .generate()
        .expect("bindgen failed");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_dir.join("bindings.rs"))
        .expect("failed to write bindings.rs");
}

fn probe_dynamic(pkg: &str, fallback_link_name: &str) {
    let mut cfg = pkg_config::Config::new();
    cfg.cargo_metadata(true);
    if cfg.probe(pkg).is_err() {
        println!("cargo:rustc-link-lib=dylib={fallback_link_name}");
    }
}
