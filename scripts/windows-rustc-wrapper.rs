//! Cargo does not pass target RUSTFLAGS to host build scripts during a cross build.
//! Use Rust's bundled GNU CRT for both host helpers and the musl target.
use std::process::Command;
fn main() {
    let mut args = std::env::args_os().skip(1);
    let rustc = args.next().expect("Cargo supplies rustc as the first argument");
    let status = Command::new(rustc)
        .args(args)
        .args(["-C", "link-self-contained=yes"])
        .status()
        .expect("could not start rustc");
    std::process::exit(status.code().unwrap_or(1));
}
