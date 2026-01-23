use std::{env, fs, path::PathBuf};

fn main() {

    let out_dir = env::var_os("OUT_DIR").unwrap();
    let mut src_path = PathBuf::from("src");
    src_path.push("lib");
    src_path.push("protos");
    src_path.push("RadarMessage.proto");
    let mut dest_path = PathBuf::from(&out_dir);
    dest_path.push("lib");
    dest_path.push("protos");
    fs::create_dir_all(&dest_path).unwrap();
    let mut dest_path = PathBuf::from(&out_dir);
    dest_path.push("bin");
    dest_path.push("web");
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

    dest_path.push("RadarMessage.proto");
    fs::copy(&src_path, &dest_path).unwrap();

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
    dest_path.push("protobuf.js");
    fs::write(&dest_path, body).unwrap();

    println!("cargo::rerun-if-changed=build.rs");
}
