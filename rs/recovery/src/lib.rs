//! The recovery library contains several functions and wrappers of tools useful
//! to subnet recovery, such as `ic-admin` proposals, state up- and download,
//! state replay, restart of nodes, etc. The library is designed to be usable by
//! command line interfaces. Therefore, input arguments are first captured and
//! returned in form of a recovery [Step], holding the human-readable (and
//! reproducable) description of the step, as well as its potential automatic
//! execution.
use admin_helper::{AdminHelper, IcAdmin, RegistryParams};
use command_helper::exec_cmd;
use error::{RecoveryError, RecoveryResult};
use file_sync_helper::{create_dir, download_binary, read_dir, write_bytes};
use futures::future::join_all;
use ic_base_types::{CanisterId, NodeId, PrincipalId};
use ic_crypto_utils_threshold_sig_der::{parse_threshold_sig_key, public_key_to_der};
use ic_cup_explorer::get_catchup_content;
use ic_logger::ReplicaLogger;
use ic_protobuf::registry::crypto::v1::PublicKey;
use ic_protobuf::registry::subnet::v1::SubnetListRecord;
use ic_registry_client::client::{RegistryClient, RegistryClientImpl, ThresholdSigPublicKey};
use ic_registry_client_helpers::{node::NodeRegistry, subnet::SubnetRegistry};
use ic_registry_keys::{make_crypto_threshold_signing_pubkey_key, make_subnet_list_record_key};
use ic_registry_local_store::LocalStoreImpl;
use ic_registry_nns_data_provider::registry::RegistryCanister;
use ic_registry_replicator::RegistryReplicator;
use ic_registry_subnet_features::EcdsaConfig;
use ic_replay::cmd::{AddAndBlessReplicaVersionCmd, AddRegistryContentCmd, SubCommand};
use ic_replay::player::StateParams;
use ic_types::messages::HttpStatusResponse;
use ic_types::{Height, ReplicaVersion, SubnetId};
use prost::Message;
use slog::{info, warn, Logger};
use ssh_helper::SshHelper;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use std::{thread, time};
use steps::*;
use url::Url;
use util::block_on;

use crate::cli::wait_for_confirmation;
use crate::file_sync_helper::read_file;

pub mod admin_helper;
pub mod app_subnet_recovery;
pub mod cli;
pub mod cmd;
pub mod command_helper;
pub mod error;
pub mod file_sync_helper;
pub mod nns_recovery_failover_nodes;
pub mod nns_recovery_same_nodes;
pub mod recovery_iterator;
pub mod replay_helper;
pub(crate) mod ssh_helper;
pub mod steps;
pub mod util;

pub const IC_DATA_PATH: &str = "/var/lib/ic/data";
pub const IC_STATE_DIR: &str = "data/ic_state";
pub const IC_CHECKPOINTS_PATH: &str = "ic_state/checkpoints";
pub const IC_CERTIFICATIONS_PATH: &str = "ic_consensus_pool/certification";
pub const IC_JSON5_PATH: &str = "/run/ic-node/config/ic.json5";
pub const IC_STATE_EXCLUDES: &[&str] = &[
    "images",
    "tip",
    "backups",
    "fs_tmp",
    "cups",
    "recovery",
    IC_REGISTRY_LOCAL_STORE,
];
pub const IC_STATE: &str = "ic_state";
pub const NEW_IC_STATE: &str = "new_ic_state";
pub const IC_REGISTRY_LOCAL_STORE: &str = "ic_registry_local_store";
pub const CHECKPOINTS: &str = "checkpoints";
pub const ADMIN: &str = "admin";
pub const READONLY: &str = "readonly";

#[derive(Clone, Debug)]
pub struct NeuronArgs {
    dfx_hsm_pin: String,
    slot: String,
    neuron_id: String,
    key_id: String,
}

#[derive(Debug)]
pub struct NodeMetrics {
    _ip: IpAddr,
    pub finalization_height: Height,
    certification_height: Height,
}

pub struct RecoveryArgs {
    pub dir: PathBuf,
    pub nns_url: Url,
    pub replica_version: Option<ReplicaVersion>,
    pub key_file: Option<PathBuf>,
}

/// The recovery struct comprises working directories for the recovery of a
/// given replica version and NNS. It offers several functions useful for subnet
/// recovery, by providing an interface to tools such as `ic-replay` and
/// `ic-recovery`, as well as ssh and rsync procedures.
/// Although operations on subnets and the downloaded state are idempotent, certain
/// orders of execution will naturally lead to errors (i.e. replaying the state
/// before downloading it).
#[derive(Clone)]
pub struct Recovery {
    pub recovery_dir: PathBuf,
    pub binary_dir: PathBuf,
    pub data_dir: PathBuf,
    pub work_dir: PathBuf,
    pub local_store_path: PathBuf,
    pub nns_pem: PathBuf,

    pub admin_helper: AdminHelper,
    pub registry_client: Arc<RegistryClientImpl>,
    pub local_store: Arc<LocalStoreImpl>,

    pub key_file: Option<PathBuf>,
    ssh_confirmation: bool,

    logger: Logger,
}

impl Recovery {
    /// Start new recovery instance by creating directories and downloading
    /// binaries.
    pub fn new(
        logger: Logger,
        args: RecoveryArgs,
        neuron_args: Option<NeuronArgs>,
        ssh_confirmation: bool,
    ) -> RecoveryResult<Self> {
        let recovery_dir = args.dir.join("recovery");
        let binary_dir = recovery_dir.join("binaries");
        let data_dir = recovery_dir.join("original_data");
        let work_dir = recovery_dir.join("working_dir");
        let local_store_path = work_dir.join("data").join(IC_REGISTRY_LOCAL_STORE);
        let nns_pem = recovery_dir.join("nns.pem");
        let local_store = Arc::new(LocalStoreImpl::new(local_store_path.clone()));
        let registry_client = Arc::new(RegistryClientImpl::new(local_store.clone(), None));
        let r = Self {
            recovery_dir,
            binary_dir: binary_dir.clone(),
            data_dir,
            work_dir,
            local_store_path,
            nns_pem,
            admin_helper: AdminHelper::new(binary_dir.clone(), args.nns_url, neuron_args),
            registry_client,
            local_store,
            key_file: args.key_file,
            ssh_confirmation,
            logger,
        };

        r.create_dirs()?;

        if !binary_dir.join("ic-admin").exists() {
            if let Some(version) = args.replica_version {
                block_on(download_binary(
                    &r.logger,
                    version,
                    String::from("ic-admin"),
                    r.binary_dir.clone(),
                ))?;
            } else {
                info!(r.logger, "No ic-admin version provided, skipping download.");
            }
        } else {
            info!(r.logger, "ic-admin exists, skipping download.");
        }

        Ok(r)
    }

    /// Construct a [Url] for the NNS endpoint of the given node IP
    pub fn get_nns_endpoint(node_ip: IpAddr) -> RecoveryResult<Url> {
        Url::parse(&format!("http://[{}]:8080", node_ip)).map_err(|e| {
            RecoveryError::invalid_output_error(format!("Failed to parse NNS URL: {}", e))
        })
    }

    /// Set recovery to a different NNS by creating a new [AdminHelper].
    pub fn set_nns(&mut self, nns_url: Url, neuron_args: Option<NeuronArgs>) {
        self.admin_helper = AdminHelper::new(self.binary_dir.clone(), nns_url, neuron_args);
    }

    // Create directories used to store downloaded states, binaries and results
    fn create_dirs(&self) -> RecoveryResult<()> {
        create_dir(&self.binary_dir)?;
        create_dir(&self.data_dir)?;
        create_dir(&self.work_dir)?;
        create_dir(&self.local_store_path)
    }

    pub fn init_registry_local_store(&self) {
        self.init_registry_local_store_with_url(&self.admin_helper.nns_url);
    }

    pub fn init_registry_local_store_with_url(&self, nns_url: &Url) {
        if !self.nns_pem.exists() {
            if let Err(e) = self.download_nns_pem(nns_url) {
                warn!(self.logger, "Failed to download NNS public key: {:?}", e);
                return;
            }
        } else {
            info!(
                self.logger,
                "nns.pem exists, skipping download of NNS public key"
            );
        }

        info!(self.logger, "Syncing registry local store");
        let key = match parse_threshold_sig_key(&self.nns_pem) {
            Ok(k) => k,
            Err(e) => {
                warn!(self.logger, "Failed to read nns.pem: {:?}", e);
                return;
            }
        };

        match read_file(&self.nns_pem) {
            Ok(s) => {
                info!(self.logger, "Continuing with public key:\n{}", s);
                let k2 = include_str!("../ic_public_key.pem");
                if k2 == s {
                    info!(
                        self.logger,
                        "Downloaded key and included NNS public key are equal!"
                    )
                } else {
                    warn!(
                        self.logger,
                        "Downloaded key is NOT equal to included NNS public key"
                    )
                }
            }
            Err(e) => {
                warn!(self.logger, "Failed to read nns.pem: {:?}", e);
            }
        }
        if self.ssh_confirmation {
            wait_for_confirmation(&self.logger);
        }

        let replica_logger = ReplicaLogger::from(self.logger.clone());
        let replicator = RegistryReplicator::new_with_clients(
            replica_logger,
            self.local_store.clone(),
            self.registry_client.clone(),
            Duration::from_secs(10),
        );

        block_on(replicator.initialize_local_store(vec![nns_url.clone()], Some(key)));
    }

    fn download_nns_pem(&self, nns_url: &Url) -> RecoveryResult<()> {
        info!(self.logger, "Downloading NNS public key...");
        let registry = RegistryCanister::new(vec![nns_url.clone()]);

        let subnet_list_key = make_subnet_list_record_key().as_bytes().to_vec();
        let list = match block_on(registry.get_value(subnet_list_key, None)) {
            Ok((bytes, _)) => SubnetListRecord::decode(&bytes[..]).map_err(|e| {
                RecoveryError::UnexpectedError(format!(
                    "Error decoding subnet list from registry: {:?}",
                    e
                ))
            })?,
            Err(error) => {
                return Err(RecoveryError::UnexpectedError(format!(
                    "Error getting value from registry: {:?}",
                    error
                )))
            }
        };

        let maybe_id = list.subnets.get(0).map(|x| {
            SubnetId::from(
                PrincipalId::try_from(x.clone().as_slice()).expect("failed parsing principal id"),
            )
        });

        let k = match maybe_id {
            Some(nns_subnet_id) => make_crypto_threshold_signing_pubkey_key(nns_subnet_id)
                .as_bytes()
                .to_vec(),
            None => {
                return Err(RecoveryError::UnexpectedError(
                    "No subnets in list".to_string(),
                ))
            }
        };

        let pk = match block_on(registry.get_value(k, None)) {
            Ok((bytes, _)) => PublicKey::decode(&bytes[..]).map_err(|e| {
                RecoveryError::UnexpectedError(format!(
                    "Error decoding PublicKey from registry: {:?}",
                    e
                ))
            })?,
            Err(error) => {
                return Err(RecoveryError::UnexpectedError(format!(
                    "Error getting value from registry: {:?}",
                    error
                )))
            }
        };

        let key = ThresholdSigPublicKey::try_from(pk).map_err(|e| {
            RecoveryError::UnexpectedError(format!(
                "failed to parse threshold signature PK from protobuf: {:?}",
                e
            ))
        })?;
        let der_bytes = public_key_to_der(&key.into_bytes()).map_err(|e| {
            RecoveryError::UnexpectedError(format!(
                "failed to encode threshold signature PK into DER: {:?}",
                e
            ))
        })?;

        let mut bytes = vec![];
        bytes.extend_from_slice(b"-----BEGIN PUBLIC KEY-----\n");
        for chunk in base64::encode(&der_bytes[..]).as_bytes().chunks(64) {
            bytes.extend_from_slice(chunk);
            bytes.extend_from_slice(b"\n");
        }
        bytes.extend_from_slice(b"-----END PUBLIC KEY-----\n");

        let path = self.nns_pem.as_ref();
        write_bytes(path, bytes)
    }

    /// Return a recovery [AdminStep] to halt or unhalt the given subnet
    pub fn halt_subnet(&self, subnet_id: SubnetId, is_halted: bool, keys: &[String]) -> impl Step {
        AdminStep {
            logger: self.logger.clone(),
            ic_admin_cmd: self
                .admin_helper
                .get_halt_subnet_command(subnet_id, is_halted, keys),
        }
    }

    /// Executes the given SSH command.
    pub fn execute_ssh_command(
        &self,
        account: &str,
        node_ip: IpAddr,
        commands: &str,
    ) -> RecoveryResult<Option<String>> {
        let ssh_helper = SshHelper::new(
            self.logger.clone(),
            account.to_string(),
            node_ip,
            self.ssh_confirmation,
            self.key_file.clone(),
        );
        ssh_helper.ssh(commands.to_string())
    }

    /// Returns true if ssh access to the given account and ip exists.
    pub fn check_ssh_access(&self, account: &str, node_ip: IpAddr) -> bool {
        let ssh_helper = SshHelper::new(
            self.logger.clone(),
            account.to_string(),
            node_ip,
            self.ssh_confirmation,
            self.key_file.clone(),
        );
        ssh_helper.can_connect()
    }

    // Execute an `ic-admin` command, log the output.
    fn exec_admin_cmd(logger: &Logger, ic_admin_cmd: &IcAdmin) -> RecoveryResult<()> {
        let mut cmd = AdminHelper::to_system_command(ic_admin_cmd);
        if let Some(res) = exec_cmd(&mut cmd)? {
            info!(logger, "{}", res);
        }
        Ok(())
    }

    /// Return a [DownloadCertificationsStep] downloading the certification pools of all reachable
    /// nodes in the given subnet to the recovery data directory using the readonly account.
    pub fn get_download_certs_step(&self, subnet_id: SubnetId, admin: bool) -> impl Step {
        DownloadCertificationsStep {
            logger: self.logger.clone(),
            subnet_id,
            registry_client: self.registry_client.clone(),
            work_dir: self.work_dir.clone(),
            require_confirmation: self.ssh_confirmation,
            key_file: self.key_file.clone(),
            admin,
        }
    }

    /// Return a [MergeCertificationPoolsStep] moving certifications and share from all
    /// downloaded pools into a new pool to be used during replay.
    pub fn get_merge_certification_pools_step(&self) -> impl Step {
        MergeCertificationPoolsStep {
            logger: self.logger.clone(),
            work_dir: self.work_dir.clone(),
        }
    }

    /// Return a [DownloadIcStateStep] downloading the ic_state of the given
    /// node to the recovery data directory using the given account.
    pub fn get_download_state_step(
        &self,
        node_ip: IpAddr,
        try_readonly: bool,
        keep_downloaded_state: bool,
    ) -> impl Step {
        DownloadIcStateStep {
            logger: self.logger.clone(),
            try_readonly,
            node_ip,
            target: self.data_dir.display().to_string(),
            keep_downloaded_state,
            working_dir: self.work_dir.display().to_string(),
            require_confirmation: self.ssh_confirmation,
            key_file: self.key_file.clone(),
        }
    }

    /// Return a [ReplayStep] to replay the downloaded state of the given
    /// subnet.
    pub fn get_replay_step(
        &self,
        subnet_id: SubnetId,
        subcmd: Option<ReplaySubCmd>,
        canister_caller_id: Option<CanisterId>,
    ) -> impl Step {
        ReplayStep {
            logger: self.logger.clone(),
            subnet_id,
            work_dir: self.work_dir.clone(),
            config: self.work_dir.join("ic.json5"),
            subcmd,
            canister_caller_id,
            result: self.work_dir.join(replay_helper::OUTPUT_FILE_NAME),
        }
    }

    /// Return a [ReplayStep] to replay the downloaded state of the given
    /// subnet and execute [SubCommand::AddAndBlessReplicaVersion].
    pub fn get_replay_with_upgrade_step(
        &self,
        subnet_id: SubnetId,
        upgrade_version: ReplicaVersion,
    ) -> RecoveryResult<impl Step> {
        let (upgrade_url, sha256) = Recovery::get_img_url_and_sha(&upgrade_version)?;
        let version_record = format!(
            r#"{{ "release_package_sha256_hex": "{}", "release_package_urls": ["{}"] }}"#,
            sha256, upgrade_url
        );
        Ok(self.get_replay_step(
            subnet_id,
            Some(ReplaySubCmd {
                cmd: SubCommand::AddAndBlessReplicaVersion(AddAndBlessReplicaVersionCmd {
                    replica_version_id: upgrade_version.to_string(),
                    replica_version_value: version_record.clone(),
                    update_subnet_record: true,
                }),
                descr: format!(
                    r#" add-and-bless-replica-version --update-subnet-record "{}" {}"#,
                    upgrade_version, version_record
                ),
            }),
            None,
        ))
    }

    /// Return a [ReplayStep] to replay the downloaded state of the given
    /// subnet and execute [SubCommand::AddRegistryContent].
    pub fn get_replay_with_registry_content_step(
        &self,
        subnet_id: SubnetId,
        new_registry_local_store: PathBuf,
        canister_caller_id: &str,
    ) -> RecoveryResult<impl Step> {
        let canister_id = CanisterId::from_str(canister_caller_id).map_err(|e| {
            RecoveryError::invalid_output_error(format!("Failed to parse canister id: {}", e))
        })?;
        Ok(self.get_replay_step(
            subnet_id,
            Some(ReplaySubCmd {
                cmd: SubCommand::AddRegistryContent(AddRegistryContentCmd {
                    registry_local_store_dir: new_registry_local_store.clone(),
                    verbose: true,
                    allowed_mutation_key_prefixes:
                        "crypto_,node_,catch_up_package_,subnet_record_,replica_version_"
                            .to_string(),
                }),
                descr: format!(
                    r#" --canister-caller-id {} add-registry-content "{}" --verbose"#,
                    canister_id,
                    new_registry_local_store.display()
                ),
            }),
            Some(canister_id),
        ))
    }

    /// Get names of all checkpoints currently on disk
    pub fn get_checkpoint_names(path: &Path) -> RecoveryResult<Vec<String>> {
        let res = read_dir(path)?
            .flatten()
            .filter_map(|e| {
                e.path()
                    .file_name()
                    .and_then(|n| n.to_str().map(String::from))
            })
            .collect::<Vec<String>>();
        Ok(res)
    }

    /// Parse and return the output of the replay step.
    pub fn get_replay_output(&self) -> RecoveryResult<StateParams> {
        replay_helper::read_output(self.work_dir.join(replay_helper::OUTPUT_FILE_NAME))
    }

    /// Calculate the next recovery height from the given height
    pub fn get_recovery_height(replay_height: Height) -> Height {
        (replay_height / 1000 + Height::from(1)) * 1000
    }

    pub fn get_validate_replay_step(&self, subnet_id: SubnetId, extra_batches: u64) -> impl Step {
        ValidateReplayStep {
            logger: self.logger.clone(),
            subnet_id,
            registry_client: self.registry_client.clone(),
            work_dir: self.work_dir.clone(),
            extra_batches,
        }
    }

    /// Return an [UploadAndRestartStep] to upload the current recovery state to
    /// a node and restart it.
    pub fn get_upload_and_restart_step(&self, node_ip: IpAddr) -> impl Step {
        self.get_upload_and_restart_step_with_data_src(node_ip, self.work_dir.join(IC_STATE_DIR))
    }

    /// Return an [UploadAndRestartStep] to upload the current recovery state to
    /// a node and restart it.
    pub fn get_upload_and_restart_step_with_data_src(
        &self,
        node_ip: IpAddr,
        data_src: PathBuf,
    ) -> impl Step {
        UploadAndRestartStep {
            logger: self.logger.clone(),
            node_ip,
            work_dir: self.work_dir.clone(),
            data_src,
            require_confirmation: self.ssh_confirmation,
            key_file: self.key_file.clone(),
        }
    }

    /// Lookup the image [Url] and sha hash of the given [ReplicaVersion]
    pub fn get_img_url_and_sha(version: &ReplicaVersion) -> RecoveryResult<(Url, String)> {
        let mut version_string = version.to_string();
        let mut test_version = false;
        let parts: Vec<_> = version_string.split('-').collect();
        if parts.len() > 1 && parts[parts.len() - 1] == "test" {
            test_version = true;
            version_string = parts[..parts.len() - 1].join("-");
        }
        let url_base = format!(
            "https://download.dfinity.systems/ic/{}/guest-os/update-img/",
            version_string
        );

        let image_name = format!(
            "update-img{}.tar.zst",
            if test_version { "-test" } else { "" }
        );
        let upgrade_url_string = format!("{}{}", url_base, image_name);
        let invalid_url = |url, e| {
            RecoveryError::invalid_output_error(format!("Invalid Url string: {}, {}", url, e))
        };
        let upgrade_url =
            Url::parse(&upgrade_url_string).map_err(|e| invalid_url(upgrade_url_string, e))?;

        let sha_url_string = format!("{}SHA256SUMS", url_base);
        let sha_url = Url::parse(&sha_url_string).map_err(|e| invalid_url(sha_url_string, e))?;

        // fetch the `SHA256SUMS` file
        let mut curl = Command::new("curl");
        curl.arg(sha_url.to_string());
        let output = exec_cmd(&mut curl)?.unwrap_or_default();

        // split the content into lines, then split each line into a pair (<hash>, <image_name>)
        let hashes = output
            .split('\n')
            .map(|line| line.split(" *").collect::<Vec<_>>())
            .collect::<Vec<_>>();

        // return the hash for the selected image name
        for pair in hashes.iter() {
            match pair.as_slice() {
                &[sha256, name] if name == image_name => {
                    return Ok((upgrade_url, sha256.to_string()));
                }
                _ => {}
            }
        }

        Err(RecoveryError::invalid_output_error(
            "No hash found in the SHA256SUMS file".to_string(),
        ))
    }

    /// Return an [AdminStep] step blessing the given [ReplicaVersion].
    /// Existence of artifacts for the given version is checked beforehand, thus
    /// generation of this step may fail if the version is invalid.
    pub fn bless_replica_version(
        &self,
        upgrade_version: &ReplicaVersion,
    ) -> RecoveryResult<impl Step> {
        let (upgrade_url, sha256) = Recovery::get_img_url_and_sha(upgrade_version)?;
        Ok(AdminStep {
            logger: self.logger.clone(),
            ic_admin_cmd: self
                .admin_helper
                .get_propose_to_bless_replica_version_flexible_command(
                    upgrade_version,
                    &upgrade_url,
                    sha256,
                ),
        })
    }

    /// Return an [AdminStep] step upgrading the given subnet to the given
    /// replica version.
    pub fn update_subnet_replica_version(
        &self,
        subnet_id: SubnetId,
        upgrade_version: &ReplicaVersion,
    ) -> impl Step {
        AdminStep {
            logger: self.logger.clone(),
            ic_admin_cmd: self
                .admin_helper
                .get_propose_to_update_subnet_replica_version_command(subnet_id, upgrade_version),
        }
    }

    pub fn get_ecdsa_config(&self, subnet_id: SubnetId) -> RecoveryResult<Option<EcdsaConfig>> {
        let rt = tokio::runtime::Runtime::new().expect("Could not create tokio runtime");
        rt.block_on(async {
            if let Err(err) = self.registry_client.poll_once() {
                return Err(format!("couldn't poll the registry: {:?}", err));
            };
            let version = self.registry_client.get_latest_version();
            self.registry_client
                .get_ecdsa_config(subnet_id, version)
                .map_err(|err| err.to_string())
        })
        .map_err(RecoveryError::UnexpectedError)
    }

    /// Return an [AdminStep] step updating the recovery CUP of the given
    /// subnet.
    pub fn update_recovery_cup(
        &self,
        subnet_id: SubnetId,
        checkpoint_height: Height,
        state_hash: String,
        replacement_nodes: &[NodeId],
        registry_params: Option<RegistryParams>,
        ecdsa_subnet_id: Option<SubnetId>,
    ) -> RecoveryResult<impl Step> {
        let key_ids = ecdsa_subnet_id
            .map(|id| match self.get_ecdsa_config(id) {
                Ok(Some(config)) => config.key_ids,
                Ok(None) => vec![],
                Err(err) => {
                    warn!(
                        self.logger,
                        "{}",
                        format!("Failed to get ECDSA config: {:?}", err)
                    );
                    vec![]
                }
            })
            .unwrap_or_default();
        Ok(AdminStep {
            logger: self.logger.clone(),
            ic_admin_cmd: self
                .admin_helper
                .get_propose_to_update_recovery_cup_command(
                    subnet_id,
                    checkpoint_height,
                    state_hash,
                    key_ids,
                    replacement_nodes,
                    registry_params,
                    ecdsa_subnet_id,
                ),
        })
    }

    /// Return an [UploadAndRestartStep] to upload the current recovery state to
    /// a node and restart it.
    pub fn get_wait_for_cup_step(&self, node_ip: IpAddr) -> impl Step {
        WaitForCUPStep {
            logger: self.logger.clone(),
            node_ip,
            work_dir: self.work_dir.clone(),
        }
    }

    /// Returns the status of a replica. It is requested from a public API.
    pub async fn get_replica_status(url: Url) -> RecoveryResult<HttpStatusResponse> {
        let joined_url = url.clone().join("api/v2/status").map_err(|e| {
            RecoveryError::invalid_output_error(format!("failed to join URLs: {}", e))
        })?;

        let response = reqwest::Client::builder()
            .timeout(time::Duration::from_secs(6))
            .build()
            .map_err(|e| {
                RecoveryError::invalid_output_error(format!("cannot build a reqwest client: {}", e))
            })?
            .get(joined_url)
            .send()
            .await
            .map_err(|err| {
                RecoveryError::invalid_output_error(format!("Failed to create request: {}", err))
            })?;

        let cbor_response = serde_cbor::from_slice(&response.bytes().await.map_err(|e| {
            RecoveryError::invalid_output_error(format!(
                "failed to convert a response to bytes: {}",
                e
            ))
        })?)
        .map_err(|e| {
            RecoveryError::invalid_output_error(format!("response is not encoded as cbor: {}", e))
        })?;

        serde_cbor::value::from_value::<HttpStatusResponse>(cbor_response).map_err(|e| {
            RecoveryError::invalid_output_error(format!(
                "failed to deserialize a response to HttpStatusResponse: {}",
                e
            ))
        })
    }

    /// Gets the replica version from the endpoint even if it is unhealthy.
    pub fn get_assigned_replica_version_any_health(url: Url) -> RecoveryResult<String> {
        let version = match block_on(Recovery::get_replica_status(url)) {
            Ok(status) => status,
            Err(err) => return Err(err),
        }
        .impl_version;
        match version {
            Some(ver) => Ok(ver),
            None => Err(RecoveryError::invalid_output_error(
                "No version found in status".to_string(),
            )),
        }
    }

    // Wait until the recovery CUP as specified in the replay output is present on the given node
    // and the node reports *some* replica version
    pub fn wait_for_recovery_cup(
        logger: &Logger,
        node_ip: IpAddr,
        recovery_height: Height,
        state_hash: String,
    ) -> RecoveryResult<()> {
        let node_url = Url::parse(&format!("http://[{}]:8080/", node_ip)).map_err(|err| {
            RecoveryError::invalid_output_error(format!(
                "Could not parse node URL for IP {}: {}",
                node_ip, err
            ))
        })?;

        let mut cup_present = false;
        for i in 0..100 {
            let maybe_cup = match block_on(get_catchup_content(&node_url)) {
                Ok(res) => res,
                Err(e) => {
                    info!(logger, "Try: {}. Could not fetch CUP: {}", i, e);
                    None
                }
            };

            if let Some(cup_content) = maybe_cup {
                let (cup_height, cup_hash) = (
                    Height::from(cup_content.random_beacon.unwrap().height),
                    hex::encode(&cup_content.state_hash),
                );

                info!(
                    logger,
                    "Try: {}. Found CUP at height {} and state hash {} on upload node",
                    i,
                    cup_height,
                    cup_hash
                );

                if cup_height == recovery_height && state_hash == cup_hash {
                    info!(logger, "Recovery CUP present!");

                    let repl_version =
                        Recovery::get_assigned_replica_version_any_health(node_url.clone());
                    info!(logger, "Status response: {:?}", repl_version);
                    if repl_version.is_ok() {
                        cup_present = true;
                        break;
                    } else {
                        info!(logger, "Replica not yet restarted");
                    }
                }
            }

            info!(logger, "Recovery CUP not yet present, retrying...");
            thread::sleep(time::Duration::from_secs(10));
        }

        if !cup_present {
            return Err(RecoveryError::invalid_output_error(
                "Did not find recovery CUP on upload node".to_string(),
            ));
        }

        Ok(())
    }

    /// Return a [CleanupStep] to remove the recovery directory and all of its contents
    pub fn get_cleanup_step(&self) -> impl Step {
        CleanupStep {
            recovery_dir: self.recovery_dir.clone(),
        }
    }

    /// Return a [StopReplicaStep] to stop the replica with the given IP
    pub fn get_stop_replica_step(&self, node_ip: IpAddr) -> impl Step {
        StopReplicaStep {
            logger: self.logger.clone(),
            node_ip,
            require_confirmation: self.ssh_confirmation,
            key_file: self.key_file.clone(),
        }
    }

    /// Return an [UpdateLocalStoreStep] to update the current local store using ic-replay
    pub fn get_update_local_store_step(&self, subnet_id: SubnetId) -> impl Step {
        UpdateLocalStoreStep {
            subnet_id,
            work_dir: self.work_dir.clone(),
        }
    }

    /// Return an [GetRecoveryCUPStep] to get the recovery CUP using ic-replay
    pub fn get_recovery_cup_step(&self, subnet_id: SubnetId) -> RecoveryResult<impl Step> {
        let state_params = self.get_replay_output()?;
        let recovery_height = Recovery::get_recovery_height(state_params.height);
        Ok(GetRecoveryCUPStep {
            subnet_id,
            config: self.work_dir.join("ic.json5"),
            result: self.work_dir.join("set_recovery_cup.txt"),
            state_hash: state_params.hash,
            work_dir: self.work_dir.clone(),
            recovery_height,
        })
    }

    /// Return a [CreateTarsStep] to create tar files of the current registry local store and ic state
    pub fn get_create_tars_step(&self) -> impl Step {
        let mut tar = Command::new("tar");
        tar.arg("-C")
            .arg(self.work_dir.join("data").join(IC_REGISTRY_LOCAL_STORE))
            .arg("-zcvf")
            .arg(
                self.work_dir
                    .join(format!("{}.tar.gz", IC_REGISTRY_LOCAL_STORE)),
            )
            .arg(".");

        CreateTarsStep {
            logger: self.logger.clone(),
            store_tar_cmd: tar,
        }
    }

    pub fn get_copy_ic_state(&self, new_state_dir: PathBuf) -> impl Step {
        CopyIcStateStep {
            logger: self.logger.clone(),
            work_dir: self.work_dir.join(IC_STATE_DIR),
            new_state_dir,
        }
    }

    /// Return an [UploadCUPAndTar] uploading tars and extracted CUP to subnet nodes
    pub fn get_upload_cup_and_tar_step(&self, subnet_id: SubnetId) -> impl Step {
        UploadCUPAndTar {
            logger: self.logger.clone(),
            registry_client: self.registry_client.clone(),
            subnet_id,
            work_dir: self.work_dir.clone(),
            require_confirmation: self.ssh_confirmation,
            key_file: self.key_file.clone(),
        }
    }

    /// Return an [AdminStep] proposing the creation of a new system subnet with testnet parameters
    pub fn get_propose_to_create_test_system_subnet_step(
        &self,
        subnet_id_override: SubnetId,
        replica_version: ReplicaVersion,
        node_ids: &[NodeId],
    ) -> impl Step {
        AdminStep {
            logger: self.logger.clone(),
            ic_admin_cmd: self.admin_helper.get_propose_to_create_test_system_subnet(
                subnet_id_override,
                replica_version,
                node_ids,
            ),
        }
    }

    /// Return a [DownloadRegistryStoreStep] to download the registry store containing entries for the given [SubnetId] from the given download node
    pub fn get_download_registry_store_step(
        &self,
        download_node: IpAddr,
        original_nns_id: SubnetId,
    ) -> impl Step {
        DownloadRegistryStoreStep {
            logger: self.logger.clone(),
            node_ip: download_node,
            original_nns_id,
            work_dir: self.work_dir.clone(),
            require_confirmation: self.ssh_confirmation,
            key_file: self.key_file.clone(),
        }
    }

    /// Return an [UploadAndHostTarStep] to upload and host a tar file on the given auxiliary host
    pub fn get_upload_and_host_tar(
        &self,
        aux_host: String,
        aux_ip: IpAddr,
        tar: PathBuf,
    ) -> impl Step {
        UploadAndHostTarStep {
            logger: self.logger.clone(),
            aux_host,
            aux_ip,
            tar,
            require_confirmation: self.ssh_confirmation,
            key_file: self.key_file.clone(),
        }
    }
}

pub async fn get_node_metrics(logger: &Logger, ip: &IpAddr) -> Option<NodeMetrics> {
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        reqwest::get(format!("http://[{}]:9090", ip)),
    )
    .await;
    let res = match response {
        Ok(Ok(res)) => res,
        e => {
            warn!(logger, "Http request failed: {:?}", e);
            return None;
        }
    };
    let body = match res.text().await {
        Ok(val) => val,
        Err(e) => {
            warn!(logger, "Http decode failed: {:?}", e);
            return None;
        }
    };
    let mut node_heights = NodeMetrics {
        finalization_height: Height::from(0),
        certification_height: Height::from(0),
        _ip: *ip,
    };
    for line in body.split('\n') {
        let mut parts = line.split(' ');
        if let (Some(prefix), Some(height)) = (parts.next(), parts.next()) {
            match prefix {
                "certification_last_certified_height" => match height.trim().parse::<u64>() {
                    Ok(val) => node_heights.certification_height = Height::from(val),
                    error => warn!(logger, "Couldn't parse height {}: {:?}", height, error),
                },
                r#"artifact_pool_consensus_height_stat{pool_type="validated",stat="max",type="finalization"}"# => {
                    match height.trim().parse::<u64>() {
                        Ok(val) => node_heights.finalization_height = Height::from(val),
                        error => {
                            warn!(logger, "Couldn't parse height {}: {:?}", height, error)
                        }
                    }
                }
                _ => continue,
            }
        }
    }
    Some(node_heights)
}

/// Grabs metrics from all nodes and greps for the certification and finalization heights.
pub fn get_node_heights_from_metrics(
    logger: &Logger,
    registry_client: Arc<RegistryClientImpl>,
    subnet_id: SubnetId,
) -> RecoveryResult<Vec<NodeMetrics>> {
    let ips = get_member_ips(registry_client, subnet_id)?;
    let metrics: Vec<NodeMetrics> =
        block_on(join_all(ips.iter().map(|ip| get_node_metrics(logger, ip))))
            .into_iter()
            .flatten()
            .collect();
    if ips.len() > metrics.len() {
        warn!(
            logger,
            "Failed to get metrics from {} nodes!",
            ips.len() - metrics.len()
        );
    }
    Ok(metrics)
}

/// Lookup IP addresses of all members of the given subnet
pub fn get_member_ips(
    registry_client: Arc<RegistryClientImpl>,
    subnet_id: SubnetId,
) -> RecoveryResult<Vec<IpAddr>> {
    let rt = tokio::runtime::Runtime::new().expect("Could not create tokio runtime");
    let result = rt
        .block_on(async {
            if let Err(err) = registry_client.poll_once() {
                return Err(format!("couldn't poll the registry: {:?}", err));
            };
            let version = registry_client.get_latest_version();
            match registry_client.get_node_ids_on_subnet(subnet_id, version) {
                Ok(Some(node_ids)) => Ok(node_ids
                    .into_iter()
                    .filter_map(|node_id| {
                        registry_client
                            .get_transport_info(node_id, version)
                            .unwrap_or_default()
                    })
                    .collect::<Vec<_>>()),
                other => Err(format!(
                    "no node ids found in the registry for subnet_id={}: {:?}",
                    subnet_id, other
                )),
            }
        })
        .map_err(RecoveryError::UnexpectedError)?;
    result
        .into_iter()
        .filter_map(|node_record| {
            node_record.http.map(|http| {
                http.ip_addr.parse().map_err(|err| {
                    RecoveryError::UnexpectedError(format!(
                        "couldn't parse ip address from the registry: {:?}",
                        err
                    ))
                })
            })
        })
        .collect()
}
