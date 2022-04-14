use crate::lib::error::DfxResult;
use crate::lib::identity::identity_utils::{call_sender, CallSender};
use crate::lib::operations::canister::deploy_canisters;
use crate::lib::provider::create_agent_environment;
use crate::lib::root_key::fetch_root_key_if_needed;
use crate::lib::{environment::Environment, identity::Identity, named_canister};
use crate::util::clap::validators::cycle_amount_validator;
use crate::util::expiry_duration;
use std::collections::BTreeMap;

use crate::lib::canister_info::CanisterInfo;
use crate::lib::models::canister_id_store::CanisterIdStore;
use crate::lib::network::network_descriptor::NetworkDescriptor;
use anyhow::{anyhow, bail};
use clap::Parser;
use console::Style;
use ic_types::Principal;
use ic_utils::interfaces::management_canister::builders::InstallMode;
use slog::info;
use std::str::FromStr;
use tokio::runtime::Runtime;
use url::Host::Domain;
use url::Url;

const MAINNET_CANDID_INTERFACE_PRINCIPAL: &str = "a4gq6-oaaaa-aaaab-qaa4q-cai";

/// Deploys all or a specific canister from the code in your project. By default, all canisters are deployed.
#[derive(Parser)]
pub struct DeployOpts {
    /// Specifies the name of the canister you want to deploy.
    /// If you don’t specify a canister name, all canisters defined in the dfx.json file are deployed.
    canister_name: Option<String>,

    /// Specifies the argument to pass to the method.
    #[clap(long)]
    argument: Option<String>,

    /// Specifies the data type for the argument when making the call using an argument.
    #[clap(long, requires("argument"), possible_values(&["idl", "raw"]))]
    argument_type: Option<String>,

    /// Force the type of deployment to be reinstall, which overwrites the module.
    /// In other words, this erases all data in the canister.
    /// By default, upgrade will be chosen automatically if the module already exists,
    /// or install if it does not.
    #[clap(long, short('m'),
    possible_values(&["reinstall"]))]
    mode: Option<String>,

    /// Upgrade the canister even if the .wasm did not change.
    #[clap(long)]
    upgrade_unchanged: bool,

    /// Override the compute network to connect to. By default, the local network is used.
    /// A valid URL (starting with `http:` or `https:`) can be used here, and a special
    /// ephemeral network will be created specifically for this request. E.g.
    /// "http://localhost:12345/" is a valid network name.
    #[clap(long)]
    network: Option<String>,

    /// Specifies the initial cycle balance to deposit into the newly created canister.
    /// The specified amount needs to take the canister create fee into account.
    /// This amount is deducted from the wallet's cycle balance.
    #[clap(long, validator(cycle_amount_validator))]
    with_cycles: Option<String>,

    /// Specify a wallet canister id to perform the call.
    /// If none specified, defaults to use the selected Identity's wallet canister.
    #[clap(long)]
    wallet: Option<String>,

    /// Performs the create call with the user Identity as the Sender of messages.
    /// Bypasses the Wallet canister.
    #[clap(long, conflicts_with("wallet"))]
    no_wallet: bool,
}

pub fn exec(env: &dyn Environment, opts: DeployOpts) -> DfxResult {
    let env = create_agent_environment(env, opts.network)?;

    let timeout = expiry_duration();
    let canister_name = opts.canister_name.as_deref();
    let argument = opts.argument.as_deref();
    let argument_type = opts.argument_type.as_deref();
    let mode = opts
        .mode
        .as_deref()
        .map(InstallMode::from_str)
        .transpose()
        .map_err(|err| anyhow!(err))?;

    let with_cycles = opts.with_cycles.as_deref();

    let force_reinstall = match (mode, canister_name) {
        (None, _) => false,
        (Some(InstallMode::Reinstall), Some(_canister_name)) => true,
        (Some(InstallMode::Reinstall), None) => {
            bail!("The --mode=reinstall is only valid when deploying a single canister, because reinstallation destroys all data in the canister.");
        }
        (Some(_), _) => {
            unreachable!("The only valid option for --mode is --mode=reinstall");
        }
    };

    let runtime = Runtime::new().expect("Unable to create a runtime");

    let call_sender = runtime.block_on(call_sender(&env, &opts.wallet))?;
    let proxy_sender;
    let create_call_sender = if !opts.no_wallet && !matches!(call_sender, CallSender::Wallet(_)) {
        let wallet = runtime.block_on(Identity::get_or_create_wallet_canister(
            &env,
            env.get_network_descriptor()
                .expect("Couldn't get the network descriptor"),
            env.get_selected_identity().expect("No selected identity"),
            false,
        ))?;
        proxy_sender = CallSender::Wallet(*wallet.canister_id_());
        &proxy_sender
    } else {
        &call_sender
    };
    runtime.block_on(fetch_root_key_if_needed(&env))?;

    runtime.block_on(deploy_canisters(
        &env,
        canister_name,
        argument,
        argument_type,
        force_reinstall,
        opts.upgrade_unchanged,
        timeout,
        with_cycles,
        &call_sender,
        create_call_sender,
    ))?;

    display_urls(&env)
}

fn display_urls(env: &dyn Environment) -> DfxResult {
    let config = env.get_config_or_anyhow()?;
    let network: &NetworkDescriptor = env.get_network_descriptor().unwrap();
    let log = env.get_logger();
    let canister_id_store = CanisterIdStore::for_env(env)?;

    let mut frontend_urls = BTreeMap::new();
    let mut candid_urls: BTreeMap<&String, Url> = BTreeMap::new();

    let ui_canister_id = named_canister::get_ui_canister_id(network);

    if let Some(canisters) = &config.get_config().canisters {
        for (canister_name, canister_config) in canisters {
            if config
                .get_config()
                .is_remote_canister(canister_name, &network.name)?
            {
                continue;
            }
            let canister_id = match Principal::from_text(canister_name) {
                Ok(principal) => Some(principal),
                Err(_) => canister_id_store.find(canister_name),
            };
            if let Some(canister_id) = canister_id {
                let canister_info = CanisterInfo::load(&config, canister_name, Some(canister_id))?;
                let is_frontend = canister_config.extras.get("frontend").is_some();

                if is_frontend {
                    let mut url = Url::parse(&network.providers[0])?;

                    if let Some(Domain(domain)) = url.host() {
                        let host = format!("{}.{}", canister_id, domain);
                        url.set_host(Some(&host))?;
                    } else {
                        let query = format!("canisterId={}", canister_id);
                        url.set_query(Some(&query));
                    };
                    frontend_urls.insert(canister_name, url);
                }

                if canister_info.get_type() != "assets" {
                    if network.is_ic {
                        let url = format!(
                            "https://{}.raw.ic0.app/?id={}",
                            MAINNET_CANDID_INTERFACE_PRINCIPAL, canister_id
                        );
                        candid_urls.insert(canister_name, Url::parse(&url)?);
                    } else if let Some(ui_canister_id) = ui_canister_id {
                        let mut url = Url::parse(&network.providers[0])?;
                        if let Some(Domain(domain)) = url.host() {
                            let host = format!("{}.{}", ui_canister_id, domain);
                            let query = format!("id={}", canister_id);
                            url.set_host(Some(&host))?;
                            url.set_query(Some(&query));
                        } else {
                            let query = format!("canisterId={}&id={}", ui_canister_id, canister_id);
                            url.set_query(Some(&query));
                        }
                        candid_urls.insert(canister_name, url);
                    };
                }
            }
        }
    }

    if !frontend_urls.is_empty() || !candid_urls.is_empty() {
        info!(log, "URLs:");
        let green = Style::new().green();
        if !frontend_urls.is_empty() {
            info!(log, "  Frontend:");
            for (name, url) in frontend_urls {
                info!(log, "    {}: {}", name, green.apply_to(url));
            }
        }
        if !candid_urls.is_empty() {
            info!(log, "  Candid:");
            for (name, url) in candid_urls {
                info!(log, "    {}: {}", name, green.apply_to(url));
            }
        }
    }

    Ok(())
}
