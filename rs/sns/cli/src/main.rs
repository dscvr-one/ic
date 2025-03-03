//! A command-line tool to initialize, deploy and interact with a SNS (Service Nervous System)

mod deploy;
mod init_config_file;
mod unit_helpers;

use crate::deploy::{DirectSnsDeployerForTests, SnsWasmSnsDeployer};
use crate::init_config_file::{
    InitConfigFileArgs, SnsCliInitConfig, SnsInitialTokenDistributionConfig,
};
use anyhow::anyhow;
use candid::{CandidType, Encode, IDLArgs};
use clap::Parser;
use ic_base_types::PrincipalId;
use ic_crypto_sha::Sha256;
use ic_nns_constants::SNS_WASM_CANISTER_ID;
use ic_sns_init::pb::v1::sns_init_payload::InitialTokenDistribution;
use ic_sns_init::pb::v1::{
    AirdropDistribution, DeveloperDistribution, FractionalDeveloperVotingPower, NeuronDistribution,
    SnsInitPayload, SwapDistribution, TreasuryDistribution,
};
use ic_sns_wasm::pb::v1::{AddWasmRequest, SnsCanisterType, SnsWasm};
use icp_ledger::{AccountIdentifier, BinaryAccountBalanceArgs};
use std::convert::TryFrom;
use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{exit, Command, Output};
use std::str::FromStr;
use tempfile::NamedTempFile;

#[derive(Debug, Parser)]
#[clap(
    name = "sns-cli",
    about = "Initialize, deploy and interact with an SNS.",
    version
)]
struct CliArgs {
    #[clap(subcommand)]
    sub_command: SubCommand,
}

#[derive(Debug, Parser)]
enum SubCommand {
    /// Deploy an sns through the sns-wasms canister.
    Deploy(DeployArgs),
    /// Deploy an sns directly to a subnet, skipping the sns-wasms canister.
    /// For use in tests only.
    DeploySkippingSnsWasmsForTests(DeployArgs),
    /// Deploy an sns directly to a subnet, skipping the sns-wasms canister.
    /// The SNS canisters remain controlled by the developer after deployment.
    /// For use in tests only.
    DeployTestflight(DeployTestflightArgs),
    /// Add a wasms for one of the SNS canisters, skipping the NNS proposal,
    /// for tests.
    AddSnsWasmForTests(AddSnsWasmForTestsArgs),
    /// Display the balance of a given account.
    AccountBalance(AccountBalanceArgs),
    /// Manage the config file where the initial sns parameters are set.
    InitConfigFile(InitConfigFileArgs),
}

/// The arguments used to configure a SNS deployment
#[derive(Debug, Parser)]
pub struct DeployArgs {
    /// Print all error and info messages.
    #[structopt(long)]
    verbose: bool,

    /// The network to deploy to. This can be "local", "ic", or the URL of an IC network.
    #[structopt(default_value = "local", long)]
    network: String,

    /// The initial config file, this file should have all the necessary parameters to deploy an SNS.
    /// See command "init-config-file"
    #[clap(long, parse(from_os_str))]
    pub init_config_file: PathBuf,

    /// The canister ID or name (via dfx.json) of SNS-WASMS to use instead of the default
    ///
    /// This is useful for testing CLI commands against local replicas without fully deployed NNS
    #[clap(long)]
    pub override_sns_wasm_canister_id_for_tests: Option<String>,

    /// The canister ID of the wallet to use on this subnet.
    ///
    /// This is needed in testnet scenarios or if you want to use a different wallet than the one
    /// currently set for your dfx identity.
    #[clap(long)]
    pub wallet_canister_override: Option<PrincipalId>,

    /// The SNS-WASMS canister candid, used to parse canister IDs from the response.
    ///
    /// Corresponds to the dfx canister call --candid flag.
    #[clap(long)]
    pub candid: Option<String>,

    /// The amount of cycles to initialize each SNS canister with. This can be omitted when
    /// deploying locally.
    #[structopt(long)]
    initial_cycles_per_canister: Option<u64>,

    /// Saves the returned canister IDs in the specified json file.
    ///
    /// Default: canister_ids.json
    #[structopt(default_value = "canister_ids.json", long)]
    save_to: PathBuf,

    /// Saves the SNS canister IDs in the specified json file for sns-quill.
    ///
    /// Default: sns_canister_ids.json
    #[structopt(default_value = "sns_canister_ids.json", long)]
    sns_canister_ids_save_to: PathBuf,

    /// The fee to provide
    #[structopt(default_value = "180000000000000", long)]
    with_cycles: u64,

    /// The directory with SNS canister WASMs.
    #[structopt(default_value = ".", long)]
    wasms_dir: PathBuf,
}

/// The arguments used to configure a SNS testflight deployment
#[derive(Debug, Parser)]
pub struct DeployTestflightArgs {
    /// Print all error and info messages.
    #[structopt(long)]
    verbose: bool,

    /// The network to deploy to. This can be "local", "ic", or the URL of an IC network.
    #[structopt(default_value = "local", long)]
    network: String,

    /// The initial config file, this file should have all the necessary parameters to deploy an SNS.
    /// See command "init-config-file"
    #[clap(long, parse(from_os_str))]
    pub init_config_file: Option<PathBuf>,

    /// The canister ID of the wallet to use on this subnet.
    ///
    /// This is needed in testnet scenarios or if you want to use a different wallet than the one
    /// currently set for your dfx identity.
    #[clap(long)]
    pub wallet_canister_override: Option<PrincipalId>,

    /// The amount of cycles to initialize each SNS canister with. This can be omitted when
    /// deploying locally.
    #[structopt(default_value = "500000000000", long)]
    initial_cycles_per_canister: u64,

    /// Saves the SNS canister IDs in the specified json file for sns-quill.
    ///
    /// Default: sns_canister_ids.json
    #[structopt(default_value = "sns_canister_ids.json", long)]
    sns_canister_ids_save_to: PathBuf,

    /// The directory with SNS canister WASMs.
    #[structopt(default_value = ".", long)]
    wasms_dir: PathBuf,
}

/// The arguments used to display the account balance of a user
#[derive(Debug, Parser)]
struct AccountBalanceArgs {
    /// The principal ID of the account owner to display their main account balance (note that
    /// subaccounts are not yet supported). If not specified, the principal of the current dfx
    /// identity is used.
    #[clap(long)]
    pub principal_id: Option<String>,

    /// The network to deploy to. This can be "local", "ic", or the URL of an IC network.
    #[structopt(default_value = "local", long)]
    network: String,
}

#[derive(Debug, Parser)]
struct AddSnsWasmForTestsArgs {
    #[clap(long, parse(from_os_str))]
    wasm_file: PathBuf,

    canister_type: String,

    /// The canister ID of SNS-WASMS to use instead of the default
    ///
    /// This is useful for testing CLI commands against local replicas without fully deployed NNS
    #[clap(long)]
    pub override_sns_wasm_canister_id_for_tests: Option<String>,

    #[structopt(default_value = "local", long)]
    network: String,
}

impl DeployArgs {
    /// panic! if any args are invalid
    pub fn validate(&self) {
        if self.network == "ic" {
            // TODO(NNS1-1511) For sns-subnet deploys, we have set fee, and will not need this
            // parameter, but will need to ensure user intends to pay the fee
            assert!(
                self.initial_cycles_per_canister.is_some(),
                "When deploying to the ic network, initial_cycles_per_canister must be set"
            );
        }
    }

    pub fn generate_sns_init_payload(&self) -> anyhow::Result<SnsInitPayload> {
        generate_sns_init_payload(&self.init_config_file)
    }
}

pub fn generate_sns_init_payload(init_config_file: &PathBuf) -> anyhow::Result<SnsInitPayload> {
    let file = File::open(init_config_file).map_err(|err| {
        anyhow!(
            "Couldn't open initial parameters file ({:?}): {}",
            init_config_file,
            err
        )
    })?;

    let sns_cli_init_config: SnsCliInitConfig = serde_yaml::from_reader(file).map_err(|err| {
        anyhow!(
            "Couldn't parse the initial parameters file ({:?}): {}",
            init_config_file,
            err
        )
    })?;

    let sns_init_payload = SnsInitPayload::try_from(sns_cli_init_config).map_err(|err| {
        anyhow!(
            "Error encountered when building the SnsInitPayload from the config file: {}",
            err
        )
    })?;

    sns_init_payload
        .validate()
        .map_err(|err| anyhow!("Initial parameters file failed validation: {}", err))?;

    Ok(sns_init_payload)
}

impl DeployTestflightArgs {
    pub fn generate_sns_init_payload(&self) -> anyhow::Result<SnsInitPayload> {
        match &self.init_config_file {
            Some(init_config_file) => generate_sns_init_payload(init_config_file),
            None => {
                let developer_identity = get_identity("get-principal", &self.network);
                let developer_neuron = NeuronDistribution {
                    controller: Some(developer_identity),
                    stake_e8s: 1500000000,
                    memo: 0,
                    dissolve_delay_seconds: 15780000,
                    vesting_period_seconds: None,
                };
                let developer_dist = DeveloperDistribution {
                    developer_neurons: vec![developer_neuron],
                };
                let treasury_dist = TreasuryDistribution {
                    total_e8s: 5000000000,
                };
                let swap_dist = SwapDistribution {
                    total_e8s: 6000000000,
                    initial_swap_amount_e8s: 3000000000,
                };
                let airdrop_dist = AirdropDistribution {
                    airdrop_neurons: vec![],
                };
                let dists = FractionalDeveloperVotingPower {
                    developer_distribution: Some(developer_dist),
                    treasury_distribution: Some(treasury_dist),
                    swap_distribution: Some(swap_dist),
                    airdrop_distribution: Some(airdrop_dist),
                };
                let initial_token_distribution = SnsInitialTokenDistributionConfig {
                    initial_token_distribution: Some(
                        InitialTokenDistribution::FractionalDeveloperVotingPower(dists),
                    ),
                };
                let mut sns_init_config = SnsCliInitConfig {
                    initial_token_distribution,
                    ..Default::default()
                };
                sns_init_config.sns_ledger.token_name = Some("MyTestToken".to_string());
                sns_init_config.sns_ledger.token_symbol = Some("MTT".to_string());
                sns_init_config
                    .sns_governance
                    .fallback_controller_principal_ids = vec![developer_identity.to_string()];
                sns_init_config.sns_governance.url = Some("https://example.com".to_string());
                sns_init_config.sns_governance.name = Some("My_Test_Token".to_string());
                sns_init_config.sns_governance.description =
                    Some("MyTestTokenDescription".to_string());
                sns_init_config.try_into()
            }
        }
    }
}

impl AddSnsWasmForTestsArgs {
    pub fn get_wasm_file_bytes(&self) -> Vec<u8> {
        let mut file = File::open(&self.wasm_file).expect("Couldn't open wasm file");
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).expect("Couldn't read wasm file");
        buf
    }
}

fn main() {
    let args = match CliArgs::try_parse_from(std::env::args()) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    };

    match args.sub_command {
        SubCommand::Deploy(args) => deploy(args),
        SubCommand::DeploySkippingSnsWasmsForTests(args) => {
            deploy_skipping_sns_wasms_for_tests(args)
        }
        SubCommand::DeployTestflight(args) => deploy_testflight(args),
        SubCommand::AddSnsWasmForTests(args) => add_sns_wasm_for_tests(args),
        SubCommand::AccountBalance(args) => print_account_balance(args),
        SubCommand::InitConfigFile(args) => init_config_file::exec(args),
    }
}

/// Deploy via SNS-WASM canister.
fn deploy(args: DeployArgs) {
    args.validate();
    let sns_init_payload = args.generate_sns_init_payload().unwrap_or_else(|err| {
        eprintln!(
            "Error encountered when generating the SnsInitPayload: {}",
            err
        );
        exit(1);
    });
    SnsWasmSnsDeployer::new(args, sns_init_payload).deploy();
}

/// Deploy an SNS with the given DeployArgs, skipping sns-wasm.
fn deploy_skipping_sns_wasms_for_tests(args: DeployArgs) {
    args.validate();
    let sns_init_payload = args.generate_sns_init_payload().unwrap_or_else(|err| {
        eprintln!(
            "Error encountered when generating the SnsInitPayload: {}",
            err
        );
        exit(1);
    });
    DirectSnsDeployerForTests::new(args, sns_init_payload).deploy()
}

/// Deploy an SNS with the given DeployTestflightArgs, skipping sns-wasm.
/// The SNS canisters remain controlled by the developer after deployment.
fn deploy_testflight(args: DeployTestflightArgs) {
    let sns_init_payload = args.generate_sns_init_payload().unwrap_or_else(|err| {
        eprintln!(
            "Error encountered when generating the SnsInitPayload: {}",
            err
        );
        exit(1);
    });
    DirectSnsDeployerForTests::new_testflight(args, sns_init_payload).deploy()
}

fn add_sns_wasm_for_tests(args: AddSnsWasmForTestsArgs) {
    let sns_wasm_bytes = args.get_wasm_file_bytes();
    let sns_wasm_hash = {
        let mut state = Sha256::new();
        state.write(&sns_wasm_bytes);
        state.finish()
    };

    let sns_canister_type = match args.canister_type.as_str() {
        "archive" => SnsCanisterType::Archive,
        "root" => SnsCanisterType::Root,
        "governance" => SnsCanisterType::Governance,
        "ledger" => SnsCanisterType::Ledger,
        "swap" => SnsCanisterType::Swap,
        "index" => SnsCanisterType::Index,
        _ => panic!("Uknown canister type."),
    };

    let add_sns_wasm_request = AddWasmRequest {
        wasm: Some(SnsWasm {
            wasm: sns_wasm_bytes,
            canister_type: sns_canister_type as i32,
        }),
        hash: sns_wasm_hash.to_vec(),
    };

    let sns_wasms_canister_id = args
        .override_sns_wasm_canister_id_for_tests
        .as_ref()
        .map(|principal| PrincipalId::from_str(principal).unwrap())
        .unwrap_or_else(|| SNS_WASM_CANISTER_ID.get());

    let idl = IDLArgs::from_bytes(&Encode!(&add_sns_wasm_request).unwrap()).unwrap();
    let mut argument_file = NamedTempFile::new().expect("Could not open temp file");
    argument_file
        .write_all(format!("{}", idl).as_bytes())
        .expect("Could not write wasm to temp file");
    let argument_path = argument_file.path().as_os_str().to_str().unwrap();

    call_dfx(&[
        "canister",
        "--network",
        &args.network,
        "call",
        "--argument-file",
        argument_path,
        &sns_wasms_canister_id.to_string(),
        "add_wasm",
    ]);
}

/// Print the Ledger account balance of the principal in `AccountBalanceArgs` if given, else
/// print the account balance of the principal of the current dfx identity.
fn print_account_balance(args: AccountBalanceArgs) {
    let principal_id = if let Some(principal_str) = args.principal_id {
        PrincipalId::from_str(&principal_str)
            .unwrap_or_else(|_| panic!("Could not parse {} as a PrincipalId", principal_str))
    } else {
        get_identity("get-principal", &args.network)
    };

    let account: AccountIdentifier = principal_id.into();
    let account_balance_args = BinaryAccountBalanceArgs {
        account: account.to_address(),
    };

    let idl = IDLArgs::from_bytes(&Encode!(&account_balance_args).unwrap()).unwrap();

    call_dfx(&[
        "canister",
        "--network",
        &args.network,
        "call",
        "sns_ledger",
        "account_balance",
        &format!("{}", idl),
    ]);
}

/// Return the `PrincipalId` of the given dfx identity
pub fn get_identity(identity: &str, network: &str) -> PrincipalId {
    println!("dfx identity {}", identity);
    let output = call_dfx(&["identity", "--network", network, identity]);

    let canister_id = String::from_utf8(output.stdout).unwrap_or_else(|_| {
        panic!(
            "Could not parse the output of 'dfx identity {}' as a string",
            identity
        )
    });

    PrincipalId::from_str(canister_id.trim()).unwrap_or_else(|_| {
        panic!(
            "Could not parse the output of 'dfx identity {}' as a PrincipalId",
            identity
        )
    })
}

/// Calls `dfx` with the given args
fn call_dfx(args: &[&str]) -> Output {
    let output = Command::new("dfx")
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("dfx failed when called with args: {:?}: {}", args, e));

    // Some dfx commands output stderr instead of stdout, so we assign it for use in both
    // success and error cases below.
    let std_err = String::from_utf8_lossy(&output.stderr);

    if output.status.success() {
        print!("{}", String::from_utf8_lossy(&output.stdout));
        print!("{}", std_err);
    } else {
        println!(
            "dfx failed when called with args: {:?}, error: {}",
            args, std_err
        );
    }

    output
}

/// Given a `CandidType`, return the hex encoding of this object.
pub fn hex_encode_candid(candid: impl CandidType) -> String {
    let bytes = Encode!(&candid).unwrap();
    hex::encode(bytes)
}
