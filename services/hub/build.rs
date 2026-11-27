//! Tests that replay the migration chain itself (src/pg_tests_chain.rs) are built where that file exists: then the cfg
//! `chain_tests` is on, and every build of the tests (cargo test, cargo clippy --all-targets) compiles and lints them. A tree
//! without the chain has no file and no cfg, with any set of features.
//!
//! The migration chain itself (the directory migrations) is embedded where that directory exists: then the cfg `chain` is on
//! and HUB_MIGRATIONS=chain works (src/migrations.rs). A tree with only the baseline has neither, and the baseline is the only
//! source.

fn main() {
    println!("cargo::rustc-check-cfg=cfg(chain_tests)");
    println!("cargo::rustc-check-cfg=cfg(chain)");
    // The directory, not the file: a missing file would rerun this script, and rebuild the crate, on every build.
    println!("cargo::rerun-if-changed=src");
    if std::path::Path::new("src/pg_tests_chain.rs").exists() {
        println!("cargo::rustc-cfg=chain_tests");
    }
    println!("cargo::rerun-if-changed=migrations");
    if std::path::Path::new("migrations").is_dir() {
        println!("cargo::rustc-cfg=chain");
    }
}
