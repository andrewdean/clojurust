//! Compiles the vendored dlmdb C sources (lmdb/, pinned in UPSTREAM_COMMIT).

fn main() {
    println!("cargo:rerun-if-changed=lmdb/mdb.c");
    println!("cargo:rerun-if-changed=lmdb/midl.c");
    println!("cargo:rerun-if-changed=lmdb/dlmdb.h");
    println!("cargo:rerun-if-changed=lmdb/midl.h");
    cc::Build::new()
        .file("lmdb/mdb.c")
        .file("lmdb/midl.c")
        .include("lmdb")
        // dlmdb carries benign unused/sign warnings on modern compilers;
        // the vendored source is pinned, not maintained here.
        .warnings(false)
        .compile("dlmdb");
}
