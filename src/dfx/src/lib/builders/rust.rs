use crate::lib::builders::{
    BuildConfig, BuildOutput, CanisterBuilder, IdlBuildOutput, WasmBuildOutput,
};
use crate::lib::canister_info::rust::RustCanisterInfo;
use crate::lib::canister_info::CanisterInfo;
use crate::lib::environment::Environment;
use crate::lib::error::DfxResult;
use crate::lib::models::canister::CanisterPool;
use crate::util::with_suspend_all_spinners;
use anyhow::{anyhow, bail, Context};
use candid::Principal as CanisterId;
use fn_error_context::context;
use slog::{info, o};
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;

pub struct RustBuilder {
    logger: slog::Logger,
}

impl RustBuilder {
    #[context("Failed to create RustBuilder.")]
    pub fn new(env: &dyn Environment) -> DfxResult<Self> {
        Ok(RustBuilder {
            logger: env.get_logger().new(o! {
                "module" => "rust"
            }),
        })
    }
}

impl CanisterBuilder for RustBuilder {
    #[context("Failed to get dependencies for canister '{}'.", info.get_name())]
    fn get_dependencies(
        &self,
        _: &dyn Environment,
        pool: &CanisterPool,
        info: &CanisterInfo,
    ) -> DfxResult<Vec<CanisterId>> {
        let dependencies = info.get_dependencies()
            .iter()
            .map(|name| {
                pool.get_first_canister_with_name(name)
                    .map(|c| c.canister_id())
                    .map_or_else(
                        || Err(anyhow!("A canister with the name '{}' was not found in the current project.", name.clone())),
                        DfxResult::Ok,
                    )
            })
            .collect::<DfxResult<Vec<CanisterId>>>().with_context(|| format!("Failed to collect dependencies (canister ids) for canister {}.", info.get_name()))?;
        Ok(dependencies)
    }

    #[context("Failed to build Rust canister '{}'.", canister_info.get_name())]
    fn build(
        &self,
        env: &dyn Environment,
        pool: &CanisterPool,
        canister_info: &CanisterInfo,
        config: &BuildConfig,
    ) -> DfxResult<BuildOutput> {
        let rust_info = canister_info.as_info::<RustCanisterInfo>()?;
        let package = rust_info.get_package();

        let mut cargo = Command::new("cargo");
        cargo
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .current_dir(canister_info.get_workspace_root())
            .arg("build")
            .arg("--target")
            .arg("wasm32-unknown-unknown")
            .arg("--release")
            .arg("-p")
            .arg(package)
            .arg("--locked");

        let dependencies = self
            .get_dependencies(env, pool, canister_info)
            .unwrap_or_default();
        let vars = super::get_and_write_environment_variables(
            canister_info,
            &config.network_name,
            pool,
            &dependencies,
            config.env_file.as_deref(),
        )?;
        for (key, val) in vars {
            cargo.env(key.as_ref(), val);
        }

        info!(
            self.logger,
            "Executing: cargo build --target wasm32-unknown-unknown --release -p {} --locked",
            package
        );

        let output = with_suspend_all_spinners(env, || {
            cargo.output().context("Failed to run 'cargo build'. You might need to run `cargo update` (or a similar command like `cargo vendor`) if you have updated `Cargo.toml`, because `dfx build` uses the --locked flag with Cargo.")
        })?;

        if !output.status.success() {
            bail!("Failed to compile the rust package: {}", package);
        }

        Ok(BuildOutput {
            wasm: WasmBuildOutput::File(rust_info.get_output_wasm_path().to_path_buf()),
            idl: IdlBuildOutput::File(canister_info.get_output_idl_path().to_path_buf()),
        })
    }

    fn get_candid_path(
        &self,
        _: &dyn Environment,
        _pool: &CanisterPool,
        info: &CanisterInfo,
        _config: &BuildConfig,
    ) -> DfxResult<PathBuf> {
        Ok(info.get_output_idl_path().to_path_buf())
    }
}
