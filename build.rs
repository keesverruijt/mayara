use std::{env, fs, path::PathBuf, process::Command};

fn main() {

    let mut src_path = PathBuf::from("src");
    src_path.push("lib");
    src_path.push("protos");
    src_path.push("RadarMessage.proto");

    let out_dir = env::var_os("OUT_DIR").unwrap();
    let mut dest_path = PathBuf::from(&out_dir);
    dest_path.push("lib");
    dest_path.push("protos");
    fs::create_dir_all(&dest_path).unwrap();

    protobuf_codegen::Codegen::new()
        .pure()
        // All inputs and imports from the inputs must reside in `includes` directories.
        .includes(&["src/lib/protos"])
        // Inputs must reside in some of include paths.
        .input("src/lib/protos/RadarMessage.proto")
        // Specify output directory relative to Cargo output directory.
        .cargo_out_dir("lib/protos")
        .run_from_script();

    let mut v1_api = PathBuf::from("web");
    v1_api.push("v1");
    v1_api.push("api");
    fs::create_dir_all(&v1_api).unwrap();
    v1_api.push("RadarMessage.proto");
    fs::copy(&src_path, &v1_api).unwrap();

    let mut v3_api = PathBuf::from("web");
    v3_api.push("v3");
    v3_api.push("api");
    fs::create_dir_all(&v3_api).unwrap();
    v3_api.push("RadarMessage.proto");
    fs::copy(&src_path, &v3_api).unwrap();

    /*
    let body = reqwest::blocking::get(
        "https://cdn.rawgit.com/dcodeIO/protobuf.js/6.11.0/dist/protobuf.min.js",
    )
    .unwrap()
    .text()
    .unwrap();
    let out_dir = env::var_os("OUT_DIR").unwrap();
    let mut dest_path = PathBuf::from(&out_dir);
    dest_path.push("bin");
    dest_path.push("web");
    dest_path.push("imports");
    fs::create_dir_all(&dest_path).unwrap();
    dest_path.push("protobuf.min.js");
    fs::write(&dest_path, body).unwrap();

    let body = reqwest::blocking::get(
        "https://cdn.rawgit.com/dcodeIO/protobuf.js/6.11.0/dist/protobuf.js",
    )
    .unwrap()
    .text()
    .unwrap();
    let out_dir = env::var_os("OUT_DIR").unwrap();
    let mut dest_path = PathBuf::from(&out_dir);
    dest_path.push("bin");
    dest_path.push("web");
    dest_path.push("imports");
    dest_path.push("protobuf.js");
    fs::write(&dest_path, body).unwrap();
    */


    // Skip GUI download in dev mode - we serve from filesystem instead
    let is_dev = env::var("CARGO_FEATURE_DEV").is_ok();

    // Download GUI from npm if not present (skip in dev mode)
    let gui_dir = PathBuf::from("web").join("v3").join("gui");
    if !is_dev && !gui_dir.join("index.html").exists() {
        println!("cargo:warning=Downloading GUI from npm...");

        // Create temp dir for npm install
        let npm_dir = PathBuf::from(&out_dir).join("npm_temp");
        fs::create_dir_all(&npm_dir).unwrap();

        // Run npm install - use npm.cmd on Windows
        let npm_cmd = if cfg!(windows) { "npm.cmd" } else { "npm" };
        let status = Command::new(npm_cmd)
            .args(["install", "@marineyachtradar/mayara-gui@latest"])
            .current_dir(&npm_dir)
            .status()
            .expect("npm not found - please install Node.js");

        if !status.success() {
            panic!("Failed to download GUI from npm");
        }

        // Copy GUI files from node_modules to OUT_DIR/gui
        let src = npm_dir.join("node_modules/@marineyachtradar/mayara-gui");
        copy_gui_files(&src, &gui_dir);

        // Cleanup npm temp
        let _ = fs::remove_dir_all(&npm_dir);
    }

    println!("cargo:rustc-env=MAYARA_GUI_DIR={}", gui_dir.display());

    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=src/protos/RadarMessage.proto");
}

/// Copy GUI files from npm package to destination
/// Only copies relevant files (html, js, css, etc.), excludes package.json etc.
fn copy_gui_files(src: &PathBuf, dest: &PathBuf) {
    fs::create_dir_all(dest).unwrap();

    let extensions = [
        ".html", ".js", ".css", ".ico", ".svg", ".png", ".jpg", ".woff", ".woff2",
    ];
    let directories = ["assets", "proto", "protobuf"];

    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if path.is_dir() {
            // Copy known directories
            if directories.contains(&name_str.as_ref()) {
                copy_dir_recursive(&path, &dest.join(&name));
            }
        } else {
            // Copy files with known extensions
            if extensions.iter().any(|ext| name_str.ends_with(ext)) {
                fs::copy(&path, dest.join(&name)).unwrap();
            }
        }
    }
}

/// Recursively copy a directory
fn copy_dir_recursive(src: &PathBuf, dest: &PathBuf) {
    fs::create_dir_all(dest).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let dest_path = dest.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &dest_path);
        } else {
            fs::copy(&path, &dest_path).unwrap();
        }
    }
}
