//! The replay tool is to help recover a broken subnet by replaying past blocks
//! and create a checkpoint of the latest state, which can then be used to
//! create recovery CatchUpPackage. It is also used to replay the artifacts
//! stored as backup, to recover a state at any height.
//!
//! It requires the same replica config file as used on the replica. It will use
//! it to locate the relevant consensus pool, state, etc. according to the
//! config file and starts replaying past finalized block, if any of them have
//! not already been executed.
//!
//! It also supports sub-commands that allows direct modifications to canister
//! state (after all past blocks have been executed). All of them are meant to
//! help recover NNS subnet where the registry canister resides.
//!
//! Use `ic-replay --help` to find out more.

use crate::cmd::{ReplayToolArgs, SubCommand};
use crate::ingress::*;
use crate::player::{Player, ReplayResult};

use cmd::RestoreFromBackupCmd;
use ic_canister_client::{Agent, Sender};
use ic_config::registry_client::DataProviderConfig;
use ic_config::{Config, ConfigSource};
use ic_nns_constants::GOVERNANCE_CANISTER_ID;
use ic_protobuf::registry::subnet::v1::InitialNiDkgTranscriptRecord;
use ic_types::ReplicaVersion;
use prost::Message;
use std::cell::RefCell;
use std::convert::{TryFrom, TryInto};
use std::error::Error;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::rc::Rc;

mod backup;
pub mod cmd;
pub mod ingress;
mod mocks;
pub mod player;
mod validator;

/// Replays the past blocks and creates a checkpoint of the latest state.
/// # An example of how to set the arguments
/// ```
/// use ic_replay::cmd::ClapSubnetId;
/// use ic_replay::cmd::RestoreFromBackupCmd;
/// use ic_replay::cmd::{ReplayToolArgs, SubCommand};
/// use ic_replay::replay;
/// use std::path::PathBuf;
/// use std::str::FromStr;
///
/// let args = ReplayToolArgs {
///     subnet_id: Some(ClapSubnetId::from_str(
///         "z4uqq-mbj6v-dxsuk-7a4wc-f6vta-cv7qg-25cqh-4jwi3-heaw3-l6b33-uae",
///     )
///     .unwrap()),
///     config: Some(PathBuf::from("/path/to/ic.json5")),
///     canister_caller_id: None,
///     replay_until_height: None,
///     data_root: None,
///     subcmd: Some(SubCommand::RestoreFromBackup(RestoreFromBackupCmd {
///         registry_local_store_path: PathBuf::from("/path/to/ic_registry_local_store"),
///         backup_spool_path: PathBuf::from("/path/to/spool"),
///         replica_version: "8b91ab7c6807a6e842d9e3bb943eadfaf856e082d1094c07852aef09f8cd0c93"
///             .to_string(),
///         start_height: 0,
///     })),
/// };
/// // Once the arguments are set well, the local store and spool directories are populated;
/// // replay function could be called as follows:
/// // replay(args);
/// ```
pub fn replay(args: ReplayToolArgs) -> ReplayResult {
    let rt = tokio::runtime::Runtime::new().expect("Could not create tokio runtime.");
    let result: Rc<RefCell<ReplayResult>> = Rc::new(RefCell::new(Ok(Default::default())));
    let res_clone = Rc::clone(&result);
    Config::run_with_temp_config(|default_config| {
        let subcmd = &args.subcmd;
        if let Some(SubCommand::VerifySubnetCUP(cmd)) = subcmd {
            if let Err(err) = verify_cup_signature(&cmd.cup_file, &cmd.public_key_file) {
                println!("CUP signature verification failed: {}", err);
                std::process::exit(1);
            } else {
                println!("CUP signature verification succeeded!");
                return;
            }
        }

        let source = ConfigSource::File(args.config.unwrap_or_else(|| {
            println!("Config file is required!");
            std::process::exit(1);
        }));
        let mut cfg = Config::load_with_default(&source, default_config).unwrap_or_else(|err| {
            println!("Failed to load config:\n  {}", err);
            std::process::exit(1);
        });

        // Override config
        if let Some(path) = args.data_root {
            cfg.registry_client.data_provider = Some(DataProviderConfig::LocalStore(
                path.join("ic_registry_local_store"),
            ));
            cfg.state_manager = ic_config::state_manager::Config::new(path.join("ic_state"));
            cfg.artifact_pool.consensus_pool_path = path.join("ic_consensus_pool");
        }

        let canister_caller_id = args.canister_caller_id.unwrap_or(GOVERNANCE_CANISTER_ID);
        let subnet_id = args
            .subnet_id
            .unwrap_or_else(|| {
                println!("Subnet is required!");
                std::process::exit(1);
            })
            .0;

        let target_height = args.replay_until_height;
        if let Some(h) = target_height {
            let question = format!("The checkpoint created at height {} ", h)
                + "cannot be used for deterministic state computation if it is not a CUP height.\n"
                + "Continue?";
            if !consent_given(&question) {
                return;
            }
        }

        if let (Some(cmd), is_new) = match subcmd {
            Some(SubCommand::RestoreFromBackup(cmd)) => (Some(cmd.clone()), false),
            Some(SubCommand::RestoreFromBackup2(cmd2)) => {
                let cmd = RestoreFromBackupCmd {
                    registry_local_store_path: cmd2.registry_local_store_path.clone(),
                    backup_spool_path: cmd2.backup_spool_path.clone(),
                    replica_version: cmd2.replica_version.clone(),
                    start_height: cmd2.start_height,
                };
                (Some(cmd), true)
            }
            _ => (None, false),
        } {
            let _enter_guard = rt.enter();

            let mut player = Player::new_for_backup(
                cfg,
                ReplicaVersion::try_from(cmd.replica_version.as_str())
                    .expect("Couldn't parse the replica version"),
                &cmd.backup_spool_path,
                &cmd.registry_local_store_path,
                subnet_id,
                cmd.start_height,
                is_new,
            )
            .with_replay_target_height(target_height);
            *res_clone.borrow_mut() = player.restore(cmd.start_height + 1);
            return;
        }

        {
            let _enter_guard = rt.enter();
            let player = match (subcmd.as_ref(), target_height) {
                (Some(_), Some(_)) => {
                    panic!(
                    "Target height cannot be used with any sub-command in subnet-recovery mode."
                );
                }
                (_, target_height) => {
                    Player::new(cfg, subnet_id).with_replay_target_height(target_height)
                }
            };

            if let Some(SubCommand::GetRecoveryCup(cmd)) = subcmd {
                cmd_get_recovery_cup(&player, cmd).unwrap();
                return;
            }

            let extra = move |player: &Player, time| -> Vec<IngressWithPrinter> {
                // Use a dummy URL here because we don't send any outgoing ingress.
                // The agent is only used to construct ingress messages.
                let agent = &Agent::new(
                    url::Url::parse("http://localhost").unwrap(),
                    Sender::PrincipalId(canister_caller_id.into()),
                );
                match subcmd {
                    Some(SubCommand::AddAndBlessReplicaVersion(cmd)) => {
                        cmd_add_and_bless_replica_version(agent, player, cmd, time)
                            .unwrap()
                            .into_iter()
                            .map(|ingress| ingress.into())
                            .collect()
                    }
                    Some(SubCommand::AddRegistryContent(cmd)) => {
                        cmd_add_registry_content(agent, cmd, player.subnet_id, time)
                            .unwrap()
                            .into_iter()
                            .map(|ingress| ingress.into())
                            .collect()
                    }
                    Some(SubCommand::RemoveSubnetNodes) => {
                        if let Some(msg) = cmd_remove_subnet(agent, player, time).unwrap() {
                            vec![msg]
                                .into_iter()
                                .map(|ingress| ingress.into())
                                .collect()
                        } else {
                            Vec::new()
                        }
                    }
                    Some(SubCommand::WithNeuronForTests(cmd)) => cmd_add_neuron(time, cmd).unwrap(),
                    Some(SubCommand::WithLedgerAccountForTests(cmd)) => {
                        cmd_add_ledger_account(time, cmd)
                            .unwrap()
                            .into_iter()
                            .map(|ingress| ingress.into())
                            .collect()
                    }
                    Some(SubCommand::WithTrustedNeuronsFollowingNeuronForTests(cmd)) => {
                        cmd_make_trusted_neurons_follow_neuron(time, cmd)
                            .unwrap()
                            .into_iter()
                            .map(|ingress| ingress.into())
                            .collect()
                    }
                    _ => Vec::new(),
                }
            };

            *res_clone.borrow_mut() = match player.replay(extra) {
                Ok(state_params) => {
                    if let Some(SubCommand::UpdateRegistryLocalStore) = subcmd {
                        player.update_registry_local_store();
                        Ok(player.get_latest_state_params(None, Vec::new()))
                    } else {
                        Ok(state_params)
                    }
                }
                err => err,
            }
        }
    });
    let ret = result.borrow().clone();
    ret
}

/// Prints a question to the user and returns `true`
/// if the user replied with a yes.
pub fn consent_given(question: &str) -> bool {
    use std::io::{stdin, stdout, Write};
    println!("{} [Y/n] ", question);
    let _ = stdout().flush();
    let mut s = String::new();
    stdin().read_line(&mut s).expect("Couldn't read user input");
    matches!(s.as_str(), "\n" | "y\n" | "Y\n")
}

// Creates a recovery CUP by using the latest CUP and overriding the height and
// the state hash.
fn cmd_get_recovery_cup(
    player: &crate::player::Player,
    cmd: &crate::cmd::GetRecoveryCupCmd,
) -> Result<(), String> {
    use ic_protobuf::registry::subnet::v1::{CatchUpPackageContents, RegistryStoreUri};
    use ic_types::consensus::{catchup::CUPWithOriginalProtobuf, HasHeight};
    use ic_types::crypto::threshold_sig::ni_dkg::NiDkgTag;

    let context_time = ic_types::time::current_time();
    let time = context_time + std::time::Duration::from_secs(60);
    let state_hash = hex::decode(&cmd.state_hash).map_err(|err| format!("{}", err))?;
    let cup = player.get_highest_catch_up_package();
    let payload = cup.content.block.as_ref().payload.as_ref();
    let summary = payload.as_summary();
    let low_threshold_transcript = summary
        .dkg
        .current_transcript(&NiDkgTag::LowThreshold)
        .clone();
    let high_threshold_transcript = summary
        .dkg
        .current_transcript(&NiDkgTag::HighThreshold)
        .clone();
    let initial_ni_dkg_transcript_low_threshold =
        Some(InitialNiDkgTranscriptRecord::from(low_threshold_transcript));
    let initial_ni_dkg_transcript_high_threshold = Some(InitialNiDkgTranscriptRecord::from(
        high_threshold_transcript,
    ));
    let registry_version = player.get_latest_registry_version(context_time)?;
    let cup_contents = CatchUpPackageContents {
        initial_ni_dkg_transcript_low_threshold,
        initial_ni_dkg_transcript_high_threshold,
        height: cmd.height,
        time: time.as_nanos_since_unix_epoch(),
        state_hash,
        registry_store_uri: Some(RegistryStoreUri {
            uri: cmd.registry_store_uri.clone().unwrap_or_default(),
            hash: cmd.registry_store_sha256.clone().unwrap_or_default(),
            registry_version: registry_version.get(),
        }),
        ecdsa_initializations: vec![],
    };

    let cup = ic_consensus::dkg::make_registry_cup_from_cup_contents(
        &*player.registry,
        player.subnet_id,
        cup_contents,
        registry_version,
        &player.log,
    )
    .ok_or_else(|| "couldn't create a registry CUP".to_string())?;

    println!(
        "height: {}, time: {}, state_hash: {:?}",
        cup.height(),
        cup.content.block.as_ref().context.time,
        cup.content.state_hash
    );

    let cup_proto = CUPWithOriginalProtobuf::from_cup(cup);
    let mut file =
        std::fs::File::create(&cmd.output_file).expect("Failed to open output file for write");
    let mut bytes = Vec::<u8>::new();
    cup_proto
        .protobuf
        .encode(&mut bytes)
        .expect("Failed to encode protobuf");
    use std::io::Write;
    file.write_all(&bytes)
        .expect("Failed to write to output file");
    Ok(())
}

fn verify_cup_signature(cup_file: &Path, public_key_file: &Path) -> Result<(), Box<dyn Error>> {
    let mut file = File::open(cup_file)?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)?;

    let cup: ic_types::consensus::CatchUpPackage =
        (&ic_protobuf::types::v1::CatchUpPackage::decode(buffer.as_slice())?).try_into()?;
    let pk = ic_crypto_utils_threshold_sig_der::parse_threshold_sig_key(public_key_file)?;

    use ic_types::consensus::HasHeight;
    if let Some((_, transcript)) = &cup
        .content
        .block
        .as_ref()
        .payload
        .as_ref()
        .as_summary()
        .dkg
        .current_transcripts()
        .iter()
        .next()
    {
        println!("Dealer subnet: {}", transcript.dkg_id.dealer_subnet);
    }
    println!("CUP height: {}", &cup.content.height());
    println!(
        "State hash: {}",
        hex::encode(cup.content.clone().state_hash.get().0)
    );
    println!();

    ic_crypto_utils_threshold_sig::verify_combined(&cup.content, &cup.signature.signature, &pk)?;

    Ok(())
}
