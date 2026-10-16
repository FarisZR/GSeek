use std::{env, path::PathBuf, process::Command};

fn main() {
    if env::var_os("CARGO_FEATURE_GUI").is_none() {
        return;
    }
    println!("cargo:rerun-if-changed=data/gseek.gresource.xml");
    println!("cargo:rerun-if-changed=data/icons");
    let target = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("gseek.gresource");
    let status = Command::new("glib-compile-resources")
        .args(["--sourcedir=data", "--target"])
        .arg(target)
        .arg("data/gseek.gresource.xml")
        .status()
        .expect("Install GLib development tools to compile GSeek's GTK resources");
    assert!(status.success(), "Cannot compile GSeek's GTK resources");
}
