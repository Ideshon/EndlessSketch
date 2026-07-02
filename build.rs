use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=help");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo must set OUT_DIR"));
    let profile_dir = out_dir
        .ancestors()
        .nth(3)
        .expect("OUT_DIR must be inside the Cargo profile directory");
    copy_help_directory(Path::new("help"), &profile_dir.join("help"));
}

fn copy_help_directory(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("failed to create Help output directory");
    for entry in fs::read_dir(source).expect("failed to read Help source directory") {
        let entry = entry.expect("failed to read Help source entry");
        let source_path = entry.path();
        if source_path.is_file() {
            fs::copy(&source_path, destination.join(entry.file_name()))
                .expect("failed to copy Help resource beside executable");
        }
    }
}
