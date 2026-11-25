use substrate_build_script_utils::{generate_cargo_keys, rerun_if_git_head_changed};

fn main() {
    if std::env::var_os("CARGO_FEATURE_MAINNET_RELEASE").is_some() {
        println!("cargo:rustc-env=SUBSTRATE_CLI_IMPL_VERSION=mainnet-retarget-24121");
    } else {
        generate_cargo_keys();
    }

    rerun_if_git_head_changed();
}
