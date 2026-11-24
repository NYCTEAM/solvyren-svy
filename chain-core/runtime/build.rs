#[cfg(all(windows, feature = "std", feature = "incinerator-rehearsal"))]
fn main() {
    use std::{env, fs, path::PathBuf};
    let path = PathBuf::from(env::var_os("SVY_REHEARSAL_WASM").expect("rehearsal Wasm path"))
        .canonicalize().expect("built rehearsal Wasm");
    println!("cargo:rerun-if-env-changed=SVY_REHEARSAL_WASM");
    println!("cargo:rerun-if-changed={}", path.display());
    let bindings = format!(
        "pub const WASM_BINARY_PATH: Option<&str> = Some({path:?});\n\
         pub const WASM_BINARY: Option<&[u8]> = Some(include_bytes!({path:?}));\n\
         pub const WASM_BINARY_BLOATY: Option<&[u8]> = WASM_BINARY;\n"
    );
    fs::write(PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("wasm_binary.rs"), bindings)
        .expect("write rehearsal Wasm bindings");
}

#[cfg(all(feature = "std", feature = "svy-matrix-live"))]
fn main() {
    use std::{env, fs, path::PathBuf};
    let artifact = if cfg!(feature = "svy-transfer-fee-candidate") {
        "../../artifacts/solvyren-svy-transfer-fee-local.wasm"
    } else {
        "../../artifacts/solvyren-svy-semiannual-v5.wasm"
    };
    let wasm = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join(artifact).canonicalize().expect("candidate Wasm");
    println!("cargo:rerun-if-changed={}", wasm.display());
    let bindings = format!(
        "pub const WASM_BINARY_PATH: Option<&str> = Some({wasm:?});\n\
         pub const WASM_BINARY: Option<&[u8]> = Some(include_bytes!({wasm:?}));\n\
         pub const WASM_BINARY_BLOATY: Option<&[u8]> = WASM_BINARY;\n"
    );
    fs::write(PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("wasm_binary.rs"), bindings)
        .expect("write candidate Wasm bindings");
}

#[cfg(all(windows, feature = "std", not(feature = "svy-matrix-live"), not(feature = "incinerator-rehearsal")))]
fn main() {
    use std::{env, fs, path::PathBuf};

    // The upstream wasm-opt C++ wrapper does not compile with current MSVC.
    // Windows follower builds embed the exact Runtime artifacts already built
    // and accepted on Linux; consensus execution still uses the code stored in
    // the raw chain specification and synchronized chain state.
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    let wasm_dir = root.join("target/release/wbuild/eagle-runtime");
    let compact = wasm_dir.join("eagle_runtime.compact.compressed.wasm");
    let bloaty = wasm_dir.join("eagle_runtime.wasm");
    assert_eq!(fs::metadata(&compact).expect("accepted compact Runtime Wasm").len(), 485_128);
    assert_eq!(fs::metadata(&bloaty).expect("accepted bloaty Runtime Wasm").len(), 1_768_423);
    println!("cargo:rerun-if-changed={}", compact.display());
    println!("cargo:rerun-if-changed={}", bloaty.display());
    let generated = format!(
        "pub const WASM_BINARY_PATH: Option<&str> = Some({compact:?});\n\
         pub const WASM_BINARY: Option<&[u8]> = Some(include_bytes!({compact:?}));\n\
         pub const WASM_BINARY_BLOATY: Option<&[u8]> = Some(include_bytes!({bloaty:?}));\n",
        compact = compact.canonicalize().unwrap(),
        bloaty = bloaty.canonicalize().unwrap(),
    );
    fs::write(PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("wasm_binary.rs"), generated)
        .expect("write Windows Runtime Wasm bindings");
}

#[cfg(all(not(windows), feature = "std", not(feature = "svy-matrix-live"), feature = "metadata-hash"))]
fn main() {
    let token_symbol = if cfg!(feature = "svy-matrix-live") { "SVY" } else { "UNIT" };
    substrate_wasm_builder::WasmBuilder::init_with_defaults()
        .enable_metadata_hash(token_symbol, 12)
        .build();
}

#[cfg(all(not(windows), feature = "std", not(feature = "svy-matrix-live"), not(feature = "metadata-hash")))]
fn main() {
    substrate_wasm_builder::WasmBuilder::build_using_defaults();
}

/// The wasm builder is deactivated when compiling
/// this crate for wasm to speed up the compilation.
#[cfg(not(feature = "std"))]
fn main() {}
