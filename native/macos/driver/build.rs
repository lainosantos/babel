use std::{env, fs, path::PathBuf, process::Command};
fn run(command: &mut Command) {
    let status = command.status().expect("run native SDK compiler");
    assert!(status.success(), "native SDK compiler failed: {command:?}");
}
fn main() {
    println!("cargo:rerun-if-changed=src/properties.def");
    println!("cargo:rerun-if-changed=src/abi.c");
    println!("cargo:rerun-if-changed=src/abi.h");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let definitions = fs::read_to_string("src/properties.def").expect("property table");
    let mut generated = String::new();
    for (index, line) in definitions
        .lines()
        .filter(|line| line.starts_with("P("))
        .enumerate()
    {
        let name = line[2..].split(',').next().expect("property name");
        generated.push_str(&format!("pub const {name}: u32 = {};\n", index + 1));
    }
    fs::write(out.join("properties.rs"), generated).expect("write property constants");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let arch = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86_64") => "x86_64",
        _ => panic!("Babel HAL supports arm64 and x86_64 macOS"),
    };
    let object = out.join("babel_hal_abi.o");
    run(Command::new("xcrun")
        .args([
            "--sdk",
            "macosx",
            "clang",
            "-std=c11",
            "-O2",
            "-fPIC",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-Wno-unused-parameter",
            "-arch",
            arch,
            "-mmacosx-version-min=11.0",
            "-c",
            "src/abi.c",
            "-o",
        ])
        .arg(&object));
    run(Command::new("xcrun")
        .args(["ar", "crs"])
        .arg(out.join("libbabel_hal_abi.a"))
        .arg(object));
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=babel_hal_abi");
    println!("cargo:rustc-link-lib=framework=CoreAudio");
    println!("cargo:rustc-link-lib=framework=CoreFoundation");
}
