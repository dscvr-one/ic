use crate::api::{CspCreateMEGaKeyError, CspThresholdSignError};
use crate::key_id::{KeyId, KeyIdInstantiationError};
use crate::types::CspPublicCoefficients;
use crate::types::{CspPop, CspPublicKey, CspSignature};
use crate::ExternalPublicKeys;
use ic_crypto_internal_logmon::metrics::KeyCounts;
use ic_crypto_internal_seed::Seed;
use ic_crypto_internal_threshold_sig_bls12381::api::ni_dkg_errors;
use ic_crypto_internal_threshold_sig_ecdsa::{
    CommitmentOpening, IDkgComplaintInternal, IDkgDealingInternal, IDkgTranscriptInternal,
    IDkgTranscriptOperationInternal, MEGaPublicKey, ThresholdEcdsaSigShareInternal,
};
use ic_crypto_internal_types::encrypt::forward_secure::{
    CspFsEncryptionPop, CspFsEncryptionPublicKey,
};
use ic_crypto_internal_types::sign::threshold_sig::ni_dkg::{
    CspNiDkgDealing, CspNiDkgTranscript, Epoch,
};
use ic_crypto_node_key_validation::ValidNodePublicKeys;
use ic_crypto_tls_interfaces::TlsPublicKeyCert;
use ic_types::crypto::canister_threshold_sig::error::{
    IDkgCreateDealingError, IDkgLoadTranscriptError, IDkgOpenTranscriptError, IDkgRetainKeysError,
    IDkgVerifyDealingPrivateError, ThresholdEcdsaSignShareError,
};
use ic_types::crypto::canister_threshold_sig::ExtendedDerivationPath;
use ic_types::crypto::{AlgorithmId, CryptoError, CurrentNodePublicKeys};
use ic_types::{NodeId, NodeIndex, NumberOfNodes, Randomness};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CspBasicSignatureError {
    SecretKeyNotFound {
        algorithm: AlgorithmId,
        key_id: KeyId,
    },
    UnsupportedAlgorithm {
        algorithm: AlgorithmId,
    },
    WrongSecretKeyType {
        algorithm: AlgorithmId,
        secret_key_variant: String,
    },
    MalformedSecretKey {
        algorithm: AlgorithmId,
    },
    InternalError {
        internal_error: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CspBasicSignatureKeygenError {
    InternalError { internal_error: String },
    DuplicateKeyId { key_id: KeyId },
    TransientInternalError { internal_error: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CspMultiSignatureError {
    SecretKeyNotFound {
        algorithm: AlgorithmId,
        key_id: KeyId,
    },
    UnsupportedAlgorithm {
        algorithm: AlgorithmId,
    },
    WrongSecretKeyType {
        algorithm: AlgorithmId,
        secret_key_variant: String,
    },
    InternalError {
        internal_error: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CspMultiSignatureKeygenError {
    MalformedPublicKey {
        algorithm: AlgorithmId,
        key_bytes: Option<Vec<u8>>,
        internal_error: String,
    },
    InternalError {
        internal_error: String,
    },
    DuplicateKeyId {
        key_id: KeyId,
    },
    TransientInternalError {
        internal_error: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CspThresholdSignatureKeygenError {
    UnsupportedAlgorithm { algorithm: AlgorithmId },
    InvalidArgument { message: String },
    InternalError { internal_error: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CspSecretKeyStoreContainsError {
    InternalError { internal_error: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CspPublicKeyStoreError {
    // TODO: CRP-1719 add more error variants if necessary
    TransientInternalError(String),
}

impl From<CspPublicKeyStoreError> for CryptoError {
    fn from(e: CspPublicKeyStoreError) -> CryptoError {
        match e {
            CspPublicKeyStoreError::TransientInternalError(details) => {
                CryptoError::TransientInternalError {
                    internal_error: format!("Error retrieving public keys: {:?}", details),
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CspTlsKeygenError {
    InvalidNotAfterDate { message: String, not_after: String },
    InternalError { internal_error: String },
    DuplicateKeyId { key_id: KeyId },
    TransientInternalError { internal_error: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CspTlsSignError {
    SecretKeyNotFound {
        key_id: KeyId,
    },
    WrongSecretKeyType {
        algorithm: AlgorithmId,
        secret_key_variant: String,
    },
    MalformedSecretKey {
        error: String,
    },
    SigningFailed {
        error: String,
    },
    InternalError {
        internal_error: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeKeysErrors {
    pub node_signing_key_error: Option<NodeKeysError>,
    pub committee_signing_key_error: Option<NodeKeysError>,
    pub tls_certificate_error: Option<NodeKeysError>,
    pub dkg_dealing_encryption_key_error: Option<NodeKeysError>,
    pub idkg_dealing_encryption_key_error: Option<NodeKeysError>,
}

impl NodeKeysErrors {
    pub fn no_error() -> Self {
        NodeKeysErrors {
            node_signing_key_error: None,
            committee_signing_key_error: None,
            tls_certificate_error: None,
            dkg_dealing_encryption_key_error: None,
            idkg_dealing_encryption_key_error: None,
        }
    }

    pub fn keys_in_registry_missing_locally(&self) -> bool {
        self.node_signing_key_error.as_ref().map_or(false, |err| {
            err.external_public_key_error.is_none()
                && err.contains_local_public_or_secret_key_error()
        }) || self
            .committee_signing_key_error
            .as_ref()
            .map_or(false, |err| {
                err.external_public_key_error.is_none()
                    && err.contains_local_public_or_secret_key_error()
            })
            || self.tls_certificate_error.as_ref().map_or(false, |err| {
                err.external_public_key_error.is_none()
                    && err.contains_local_public_or_secret_key_error()
            })
            || self
                .dkg_dealing_encryption_key_error
                .as_ref()
                .map_or(false, |err| {
                    err.external_public_key_error.is_none()
                        && err.contains_local_public_or_secret_key_error()
                })
            || self
                .idkg_dealing_encryption_key_error
                .as_ref()
                .map_or(false, |err| {
                    err.external_public_key_error.is_none()
                        && err.contains_local_public_or_secret_key_error()
                })
    }
}

impl From<&NodeKeysErrors> for KeyCounts {
    fn from(err: &NodeKeysErrors) -> Self {
        KeyCounts::ZERO
            + err
                .node_signing_key_error
                .as_ref()
                .map_or(KeyCounts::ONE, KeyCounts::from)
            + err
                .committee_signing_key_error
                .as_ref()
                .map_or(KeyCounts::ONE, KeyCounts::from)
            + err
                .tls_certificate_error
                .as_ref()
                .map_or(KeyCounts::ONE, KeyCounts::from)
            + err
                .dkg_dealing_encryption_key_error
                .as_ref()
                .map_or(KeyCounts::ONE, KeyCounts::from)
            + err
                .idkg_dealing_encryption_key_error
                .as_ref()
                .map_or(KeyCounts::ONE, KeyCounts::from)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeKeysError {
    pub external_public_key_error: Option<ExternalPublicKeyError>,
    pub local_public_key_error: Option<LocalPublicKeyError>,
    pub secret_key_error: Option<SecretKeyError>,
}

impl NodeKeysError {
    pub fn no_error() -> Self {
        NodeKeysError {
            external_public_key_error: None,
            local_public_key_error: None,
            secret_key_error: None,
        }
    }

    fn contains_local_public_or_secret_key_error(&self) -> bool {
        self.local_public_key_error.is_some() || self.secret_key_error.is_some()
    }
}

/// Calculates a [`KeyCount`] from provided [`NodeKeysError`].
///
/// If there is no error for a particular key, it is deemed to be present, and the count is 1.
/// If there is an error for a particular key, it is deemed to not be present, and the count is 0.
///
/// # Example
///```
///  # use ic_crypto_internal_csp::vault::api::{ExternalPublicKeyError, LocalPublicKeyError, NodeKeysError, SecretKeyError};
///  # use ic_crypto_internal_logmon::metrics::KeyCounts;
///  let all_ok = NodeKeysError {
///      external_public_key_error: None,
///      local_public_key_error: None,
///      secret_key_error: None
///  };
///  let ok_key_counts = KeyCounts::from(&all_ok);
///  assert_eq!(
///      ok_key_counts,
///      KeyCounts::ONE
///  );
///  let no_keys_present = NodeKeysError {
///      external_public_key_error: Some(ExternalPublicKeyError(Box::new("malformed external public key".to_string()))),
///      local_public_key_error: Some(LocalPublicKeyError::Mismatch),
///      secret_key_error: Some(SecretKeyError::CannotComputeKeyId)
///  };
///  let empty_key_counts = KeyCounts::from(&no_keys_present);
///  assert_eq!(
///      empty_key_counts,
///      KeyCounts::new(0, 0, 0)
///  );
///  let local_keys_missing = NodeKeysError {
///      external_public_key_error: None,
///      local_public_key_error: Some(LocalPublicKeyError::NotFound),
///      secret_key_error: Some(SecretKeyError::NotFound)
///  };
///  let partial_key_count = KeyCounts::from(&local_keys_missing);
///  assert_eq!(
///      partial_key_count,
///      KeyCounts::new(1, 0, 0)
///  );
///```
impl From<&NodeKeysError> for KeyCounts {
    fn from(err: &NodeKeysError) -> Self {
        KeyCounts::new(
            err.external_public_key_error.as_ref().map_or(1, |_err| 0),
            err.local_public_key_error.as_ref().map_or(1, |_err| 0),
            err.secret_key_error.as_ref().map_or(1, |_err| 0),
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExternalPublicKeyError(pub Box<String>);

impl From<KeyIdInstantiationError> for ExternalPublicKeyError {
    fn from(error: KeyIdInstantiationError) -> Self {
        ExternalPublicKeyError(Box::new(format!("Cannot instantiate KeyId: {:?}", error)))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LocalPublicKeyError {
    /// No local public key exists.
    NotFound,
    /// A local public key exists, but it is not the same as the external key passed in.
    Mismatch,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SecretKeyError {
    /// Unable to compute the key ID using the externally provided public key.
    CannotComputeKeyId,
    /// A local secret key matching the externally provided public key does not exist
    NotFound,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PksAndSksContainsErrors {
    /// If one or more keys were missing, or were malformed, or did not match the corresponding
    /// external public key.
    NodeKeysErrors(NodeKeysErrors),
    /// If a transient internal error occurs, e.g., an RPC error communicating with the remote vault
    TransientInternalError(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PksAndSksCompleteError {
    /// Public key store does not contain any public key
    EmptyPublicKeyStore,
    /// Error checking node signing public key and secret key
    NodeSigningKeyError(PksAndSksCompleteKeyPairError),
    /// Error checking committee signing public key and secret key
    CommitteeSigningKeyError(PksAndSksCompleteKeyPairError),
    /// Error checking TLS certificate and secret key
    TlsCertificateError(PksAndSksCompleteKeyPairError),
    /// Error checking dkg dealing encryption public key and secret key
    DkgDealingEncryptionKeyError(PksAndSksCompleteKeyPairError),
    /// Error checking one of IDKG dealing encryption public and secret key pairs
    IdkgDealingEncryptionKeyError(PksAndSksCompleteKeyPairError),
    /// If a transient internal error occurs, e.g., an RPC error communicating with the remote vault
    TransientInternalError(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PksAndSksCompleteKeyPairError {
    /// Expected public key is missing
    PublicKeyNotFound,
    /// Public key is somehow malformed or a key ID cannot be computed from it.
    PublicKeyInvalid,
    /// Secret key is missing
    SecretKeyNotFound,
}

/// `CspVault` offers a selection of operations that involve
/// secret keys managed by the vault.
pub trait CspVault:
    BasicSignatureCspVault
    + MultiSignatureCspVault
    + ThresholdSignatureCspVault
    + NiDkgCspVault
    + IDkgProtocolCspVault
    + ThresholdEcdsaSignerCspVault
    + SecretKeyStoreCspVault
    + TlsHandshakeCspVault
    + PublicRandomSeedGenerator
    + PublicAndSecretKeyStoreCspVault
    + PublicKeyStoreCspVault
{
}

// Blanket implementation of `CspVault` for all types that fulfill the
// requirements.
impl<T> CspVault for T where
    T: BasicSignatureCspVault
        + MultiSignatureCspVault
        + ThresholdSignatureCspVault
        + NiDkgCspVault
        + IDkgProtocolCspVault
        + ThresholdEcdsaSignerCspVault
        + SecretKeyStoreCspVault
        + TlsHandshakeCspVault
        + PublicRandomSeedGenerator
        + PublicAndSecretKeyStoreCspVault
        + PublicKeyStoreCspVault
{
}

/// Operations of `CspVault` related to basic signatures
/// (cf. `CspSigner` and `CspKeyGenerator`).
pub trait BasicSignatureCspVault {
    /// Signs the given message using the specified algorithm and key ID.
    ///
    /// # Arguments
    /// * `algorithm_id` specifies the signature algorithm
    /// * `message` is the message to be signed
    /// * `key_id` determines the private key to sign with
    /// # Returns
    /// The computed signature.
    /// # Note
    /// `sign`-method of basic signatures takes the full message as an argument
    /// (rather than just message digest).
    /// The reason for this "inefficiency" is the fact that in
    /// Ed25519-signatures (that this trait has to support) the computation
    /// of the message digest uses secret key data as an input, and so
    /// cannot be computed outside of the CspVault (cf. PureEdDSA in
    /// [RFC 8032](https://tools.ietf.org/html/rfc8032#section-5.1.6))
    fn sign(
        &self,
        algorithm_id: AlgorithmId,
        message: &[u8],
        key_id: KeyId,
    ) -> Result<CspSignature, CspBasicSignatureError>;

    /// Generates a node signing public/private key pair.
    ///
    /// # Returns
    /// Generated public key.
    ///
    /// # Errors
    /// * `CspBasicSignatureKeygenError::InternalError` if there is an internal
    ///   error (e.g., the public key in the public key store is already set).
    /// * `CspBasicSignatureKeygenError::DuplicateKeyId` if there already
    ///   exists a secret key in the store for the secret key ID derived from
    ///   the public part of the randomly generated key pair. This error
    ///   most likely indicates a bad randomness source.
    /// * `CspBasicSignatureKeygenError::TransientInternalError` if there is a
    ///   transient internal error, e.g., an IO error when writing a key to
    ///   disk, or an RPC error when calling a remote CSP vault.
    fn gen_node_signing_key_pair(&self) -> Result<CspPublicKey, CspBasicSignatureKeygenError>;
}

/// Operations of `CspVault` related to multi-signatures
/// (cf. `CspSigner` and `CspKeyGenerator`).
pub trait MultiSignatureCspVault {
    /// Signs the given message using the specified algorithm and key ID.
    ///
    /// # Arguments
    /// * `algorithm_id` specifies the signature algorithm
    /// * `message` is the message to be signed
    /// * `key_id` determines the private key to sign with
    /// # Returns
    /// The computed signature.
    ///
    /// # Note
    /// `multi_sign`-method takes the full message as an argument (rather than
    /// just message digest) to be consistent with
    /// `BasicSignatureCspVault::sign()`-method.
    fn multi_sign(
        &self,
        algorithm_id: AlgorithmId,
        message: &[u8],
        key_id: KeyId,
    ) -> Result<CspSignature, CspMultiSignatureError>;

    /// Generates a public/private key pair, with a proof of possession.
    ///
    /// # Returns
    /// The public key of the keypair and the proof of possession.
    ///
    /// # Errors
    /// * `CspMultiignatureKeygenError::InternalError` if there is an internal
    ///   error (e.g., the public key in the public key store is already set).
    /// * `CspMultiSignatureKeygenError::DuplicateKeyId` if there already
    ///   exists a secret key in the store for the secret key ID derived from
    ///   the public part of the randomly generated key pair. This error
    ///   most likely indicates a bad randomness source.
    /// * `CspMultiSignatureKeygenError::TransientInternalError` if there is a
    ///   transient internal error, e.g,. an IO error when writing a key to
    ///   disk, or an RPC error when calling a remote CSP vault.
    fn gen_committee_signing_key_pair(
        &self,
    ) -> Result<(CspPublicKey, CspPop), CspMultiSignatureKeygenError>;
}

/// Operations of `CspVault` related to threshold signatures
/// (cf. `ThresholdSignatureCspClient`).
pub trait ThresholdSignatureCspVault {
    /// Generates threshold keys.
    ///
    /// This interface is primarily of interest for testing and demos.
    ///
    /// # Arguments
    /// * `algorithm_id` indicates the algorithms to be used in the key
    ///   generation.
    /// * `threshold` is the minimum number of signatures that can be combined
    ///   to make a valid threshold signature.
    /// * `receivers` is the total number of receivers
    /// # Returns
    /// * `CspPublicCoefficients` can be used by the caller to verify
    ///   signatures.
    /// * `Vec<KeyId>` contains key identifiers.  The vector has the
    ///   same length as the number of `receivers`.
    /// # Panics
    /// * An implementation MAY panic if it is unable to access the secret key
    ///   store to save keys or if it cannot access a suitable random number
    ///   generator.
    /// # Errors
    /// * If `threshold > receivers` then it is impossible for
    ///   the signatories to create a valid combined signature, so
    ///   implementations MUST return an error.
    /// * An implementation MAY return an error if it is temporarily unable to
    ///   generate and store keys.
    fn threshold_keygen_for_test(
        &self,
        algorithm_id: AlgorithmId,
        threshold: NumberOfNodes,
        receivers: NumberOfNodes,
    ) -> Result<(CspPublicCoefficients, Vec<KeyId>), CspThresholdSignatureKeygenError>;

    /// Signs the given message using the specified algorithm and key ID.
    ///
    /// # Arguments
    /// * `algorithm_id` specifies the signature algorithm
    /// * `message` is the message to be signed
    /// * `key_id` determines the private key to sign with
    /// # Returns
    /// The computed threshold signature.
    ///
    /// # Note
    /// `threshold_sign`-method takes the full message as an argument (rather
    /// than just message digest) to be consistent with
    /// `BasicSignatureCspVault::sign()`-method.
    fn threshold_sign(
        &self,
        algorithm_id: AlgorithmId,
        message: &[u8],
        key_id: KeyId,
    ) -> Result<CspSignature, CspThresholdSignError>;
}

/// Operations of `CspVault` related to NI-DKG (cf. `NiDkgCspClient`).
pub trait NiDkgCspVault {
    /// Generates a forward-secure dealing encryption key pair used to encrypt threshold key shares
    /// in transmission.
    ///
    /// # Returns
    /// The public key and the corresponding proof-of-possession.
    /// # Errors
    /// * `ni_dkg_errors::CspDkgCreateFsKeyError::InternalError` if there is an internal
    ///   error (e.g., the public key in the public key store is already set).
    /// * `ni_dkg_errors::CspDkgCreateFsKeyError::TransientInternalError` if there is a transient
    ///   internal error, e.g., an IO error when writing a key to disk, or an
    ///   RPC error when calling a remote CSP vault.
    fn gen_dealing_encryption_key_pair(
        &self,
        node_id: NodeId,
    ) -> Result<(CspFsEncryptionPublicKey, CspFsEncryptionPop), ni_dkg_errors::CspDkgCreateFsKeyError>;

    /// Updates the forward-secure secret key determined by the key id,
    /// so that it cannot be used to decrypt data at epochs that are smaller
    /// (older) than the given epoch.
    ///
    /// # Arguments
    /// * `key_id` identifies the forward-secure secret key.
    /// * `epoch` is the epoch to be deleted, together with all smaller epochs.
    fn update_forward_secure_epoch(
        &self,
        algorithm_id: AlgorithmId,
        key_id: KeyId,
        epoch: Epoch,
    ) -> Result<(), ni_dkg_errors::CspDkgUpdateFsEpochError>;

    /// Generates a dealing which contains a share for each eligible receiver.
    /// If `reshared_secret` is `None`, then the dealing is a sharing of a
    /// fresh random value, otherwise it is a re-sharing of the secret
    /// identified by `reshared_secret`.
    ///
    /// # Arguments
    /// * `algorithm_id` selects the algorithm suite to use for the scheme.
    /// * `dealer_index` the index associated with the dealer.
    /// * `threshold` is the minimum number of nodes required to generate a
    ///   valid threshold signature.
    /// * `epoch` is a monotonic increasing counter used to select forward
    ///   secure keys.
    /// * `receiver_keys` is a map storing a forward-secure public key for each
    ///   receiver, indexed by their corresponding NodeIndex.
    /// * 'maybe_resharing_secret' if `Some`, identifies the secret to be
    ///   reshared.
    /// # Returns
    /// A new dealing.
    fn create_dealing(
        &self,
        algorithm_id: AlgorithmId,
        dealer_index: NodeIndex,
        threshold: NumberOfNodes,
        epoch: Epoch,
        receiver_keys: &BTreeMap<NodeIndex, CspFsEncryptionPublicKey>,
        maybe_resharing_secret: Option<KeyId>,
    ) -> Result<CspNiDkgDealing, ni_dkg_errors::CspDkgCreateReshareDealingError>;

    /// Computes a threshold signing key and stores it in the secret key store.
    ///
    /// After calling this method the threshold signature API can be used
    /// to generate signature shares.
    /// # Arguments
    /// * `algorithm_id` selects the algorithm suite to use for the scheme.
    /// * `dkg_id` is the identifier for the distributed key being generated.
    /// * `epoch` is a monotonic increasing counter used to select forward
    ///   secure keys.
    /// * `csp_transcript_for_node` is a summary of the key generation,
    ///   containing the transcript parts relevant for the current node.
    /// * `fs_key_id` identifies the forward-secure key that is used to decrypt
    ///   shares.
    /// * `receiver_index` is the index of the current node in the list of
    ///   receivers.
    fn load_threshold_signing_key(
        &self,
        algorithm_id: AlgorithmId,
        epoch: Epoch,
        csp_transcript: CspNiDkgTranscript,
        fs_key_id: KeyId,
        receiver_index: NodeIndex,
    ) -> Result<(), ni_dkg_errors::CspDkgLoadPrivateKeyError>;

    /// Keeps the specified NiDKG threshold keys.
    ///
    /// A threshold key in the secret key store with an id specified in
    /// `active_key_ids` will be kept; other threshold keys will be deleted.
    ///
    /// There is no guarantee that there are secret keys matching all the key
    /// ids. If this method is requested to retain a key that is not in the
    /// secret key store, that key will be ignored.
    /// # Arguments
    /// * `active_key_ids` identifies threshold keys that should be retained
    fn retain_threshold_keys_if_present(
        &self,
        active_key_ids: BTreeSet<KeyId>,
    ) -> Result<(), ni_dkg_errors::CspDkgRetainThresholdKeysError>;
}

/// Operations of `CspVault` related to querying the secret key store (cf.
/// `CspSecretKeyStoreChecker`).
pub trait SecretKeyStoreCspVault {
    /// Checks whether the secret key store contains a key with the given
    /// `key_id`.
    ///
    /// # Arguments
    /// * `key_id` identifies the key whose presence should be checked.
    fn sks_contains(&self, key_id: &KeyId) -> Result<bool, CspSecretKeyStoreContainsError>;
}

/// Operations of `CspVault` related to querying the public key store.
pub trait PublicKeyStoreCspVault {
    /// Returns the node's current public keys where generation timestamps are stripped.
    ///
    /// For keys that are periodically rotated (such as the iDKG dealing encryption key pair) only
    /// the latest public key locally available will be returned.
    ///
    /// # Errors
    /// * if a transient error (e.g., RPC timeout) occurs when accessing the public key store
    fn current_node_public_keys(&self) -> Result<CurrentNodePublicKeys, CspPublicKeyStoreError>;

    /// Returns the node's current public keys with its generation timestamps.
    ///
    /// If timestamps are not needed, you should use [`Self::current_node_public_keys`].
    /// For keys that are periodically rotated (such as the iDKG dealing encryption key pair) only
    /// the latest public key locally available will be returned.
    ///
    /// # Errors
    /// * if a transient error (e.g., RPC timeout) occurs when accessing the public key store
    fn current_node_public_keys_with_timestamps(
        &self,
    ) -> Result<CurrentNodePublicKeys, CspPublicKeyStoreError>;

    /// Returns the number of iDKG dealing encryption public keys stored locally.
    ///
    /// # Errors
    /// * if a transient error (e.g., RPC timeout) occurs when accessing the public key store
    fn idkg_dealing_encryption_pubkeys_count(&self) -> Result<usize, CspPublicKeyStoreError>;
}

/// Operations of `CspVault` related to querying both the public and private key stores.
pub trait PublicAndSecretKeyStoreCspVault {
    /// Checks whether the keys corresponding to the provided external public keys exist locally.
    /// In particular, this means the provided public keys themselves are stored locally, as well
    /// as the corresponding secret keys. Key comparisons will not take timestamps into account.
    ///
    /// # Parameters
    /// The current external node public keys and TLS certificate.
    ///
    /// # Returns
    /// An empty result if all the external public keys, and the corresponding secret keys, were
    /// all found locally.
    ///
    /// # Errors
    /// * `PksAndSksContainsErrors::NodeKeysErrors` if local public or secret keys were not
    ///   consistent with the provided external keys.
    /// * `PksAndSksContainsErrors::TransientInternalError` if a transient internal error, e.g., an RPC
    ///   error, occurred.
    fn pks_and_sks_contains(
        &self,
        external_public_keys: ExternalPublicKeys,
    ) -> Result<(), PksAndSksContainsErrors>;

    /// Checks whether the public key store and secret key store are complete:
    /// * all required public keys are present,
    /// * all public keys in the public key store (corresponding to all required public keys
    ///   and potentially additionally stored public keys, like rotated IDKG dealing encryption public keys)
    ///   have a corresponding secret key in the secret key store,
    /// * all public keys are valid.
    /// If all check passes, the current node public keys in validated form is returned.
    ///
    /// # Errors
    /// The method return on the first encountered error and will not check further any other key pairs.
    /// The order in which checks are performed and keys are checked is not part of the API and should not be relied upon.
    /// * [`PksAndSksCompleteError::EmptyPublicKeyStore`] if there are no public keys
    /// * [`PksAndSksCompleteError::NodeSigningKeyError`] if there is a problem with the node signing key pair
    /// * [`PksAndSksCompleteError::CommitteeSigningKeyError`] if there is a problem with the committee signing key pair
    /// * [`PksAndSksCompleteError::TlsCertificateError`] if there is a problem with the TLS key material
    /// * [`PksAndSksCompleteError::DkgDealingEncryptionKeyError`] if there is a problem with the DKG dealing encryption key pair
    /// * [`PksAndSksCompleteError::IdkgDealingEncryptionKeyError`] if there is a problem with any of the IDKG dealing encryption key pairs
    /// * [`PksAndSksCompleteError::TransientInternalError`] if a transient internal error, e.g., an RPC error, occurred.
    fn pks_and_sks_complete(&self) -> Result<ValidNodePublicKeys, PksAndSksCompleteError>;
}

/// Operations of `CspVault` related to TLS handshakes.
pub trait TlsHandshakeCspVault: Send + Sync {
    /// Generates TLS key material for node with ID `node_id`.
    ///
    /// The secret key is stored in the key store and used to create a
    /// self-signed X.509 public key certificate with
    /// * a random serial,
    /// * the common name (CN) of both subject and issuer being the `ToString`
    ///   form of the given `node_id`,
    /// * validity starting at the time of calling this method, and
    /// * validity ending at `not_after`, which must be specified according to
    ///   section 4.1.2.5 in RFC 5280.
    ///
    /// Returns the key ID of the secret key, and the public key certificate.
    ///
    /// # Errors
    /// * if `not_after` is not specified according to RFC 5280 or if
    /// `not_after` is in the past
    /// * if a malformed X509 certificate is generated
    fn gen_tls_key_pair(
        &self,
        node: NodeId,
        not_after: &str,
    ) -> Result<TlsPublicKeyCert, CspTlsKeygenError>;

    /// Signs the given message using the specified algorithm and key ID.
    ///
    /// # Arguments
    /// * `message` is the message to be signed
    /// * `key_id` determines the private key to sign with
    /// # Returns
    /// The computed signature to be used during a TLS handshake.
    ///
    /// # Note
    /// The method takes the full message as an argument (rather than
    /// just message digest) to be consistent with
    /// `BasicSignatureCspVault::sign()`-method.
    fn tls_sign(&self, message: &[u8], key_id: &KeyId) -> Result<CspSignature, CspTlsSignError>;
}

/// Operations of `CspVault` related to I-DKG (cf. `CspIDkgProtocol`).
pub trait IDkgProtocolCspVault {
    /// Generate an IDkg dealing.
    fn idkg_create_dealing(
        &self,
        algorithm_id: AlgorithmId,
        context_data: &[u8],
        dealer_index: NodeIndex,
        reconstruction_threshold: NumberOfNodes,
        receiver_keys: &[MEGaPublicKey],
        transcript_operation: &IDkgTranscriptOperationInternal,
    ) -> Result<IDkgDealingInternal, IDkgCreateDealingError>;

    /// See [`CspIDkgProtocol::idkg_verify_dealing_private`].
    fn idkg_verify_dealing_private(
        &self,
        algorithm_id: AlgorithmId,
        dealing: &IDkgDealingInternal,
        dealer_index: NodeIndex,
        receiver_index: NodeIndex,
        receiver_key_id: KeyId,
        context_data: &[u8],
    ) -> Result<(), IDkgVerifyDealingPrivateError>;

    /// Compute secret from transcript and store in SKS, generating complaints
    /// if necessary.
    fn idkg_load_transcript(
        &self,
        dealings: &BTreeMap<NodeIndex, IDkgDealingInternal>,
        context_data: &[u8],
        receiver_index: NodeIndex,
        key_id: &KeyId,
        transcript: &IDkgTranscriptInternal,
    ) -> Result<BTreeMap<NodeIndex, IDkgComplaintInternal>, IDkgLoadTranscriptError>;

    /// See [`crate::api::CspIDkgProtocol::idkg_load_transcript_with_openings`].
    fn idkg_load_transcript_with_openings(
        &self,
        dealings: &BTreeMap<NodeIndex, IDkgDealingInternal>,
        openings: &BTreeMap<NodeIndex, BTreeMap<NodeIndex, CommitmentOpening>>,
        context_data: &[u8],
        receiver_index: NodeIndex,
        key_id: &KeyId,
        transcript: &IDkgTranscriptInternal,
    ) -> Result<(), IDkgLoadTranscriptError>;

    /// Generate a MEGa keypair, for encrypting/decrypting IDkg dealing shares.
    ///
    /// See [`crate::api::CspIDkgProtocol::idkg_gen_dealing_encryption_key_pair`].
    fn idkg_gen_dealing_encryption_key_pair(&self) -> Result<MEGaPublicKey, CspCreateMEGaKeyError>;

    /// Opens the dealing from dealer specified by `dealer_index`.
    fn idkg_open_dealing(
        &self,
        dealing: IDkgDealingInternal,
        dealer_index: NodeIndex,
        context_data: &[u8],
        opener_index: NodeIndex,
        opener_key_id: &KeyId,
    ) -> Result<CommitmentOpening, IDkgOpenTranscriptError>;

    /// See [`crate::api::CspIDkgProtocol::idkg_retain_active_keys`].
    fn idkg_retain_active_keys(
        &self,
        active_key_ids: BTreeSet<KeyId>,
        oldest_public_key: MEGaPublicKey,
    ) -> Result<(), IDkgRetainKeysError>;
}

/// Operations of `CspVault` related to threshold-ECDSA (cf.
/// `CspThresholdEcdsaSigner`).
pub trait ThresholdEcdsaSignerCspVault {
    /// Generate a signature share.
    #[allow(clippy::too_many_arguments)]
    fn ecdsa_sign_share(
        &self,
        derivation_path: &ExtendedDerivationPath,
        hashed_message: &[u8],
        nonce: &Randomness,
        key: &IDkgTranscriptInternal,
        kappa_unmasked: &IDkgTranscriptInternal,
        lambda_masked: &IDkgTranscriptInternal,
        kappa_times_lambda: &IDkgTranscriptInternal,
        key_times_lambda: &IDkgTranscriptInternal,
        algorithm_id: AlgorithmId,
    ) -> Result<ThresholdEcdsaSigShareInternal, ThresholdEcdsaSignShareError>;
}

/// An error returned by failing to generate a public seed from [`CspVault`].
#[derive(Serialize, Deserialize, Debug)]
pub enum PublicRandomSeedGeneratorError {
    /// Internal error, e.g., an RPC error.
    InternalError { internal_error: String },
}

impl From<PublicRandomSeedGeneratorError> for CryptoError {
    fn from(error: PublicRandomSeedGeneratorError) -> CryptoError {
        match error {
            PublicRandomSeedGeneratorError::InternalError { internal_error } => {
                CryptoError::InternalError { internal_error }
            }
        }
    }
}

/// Operations of [`CspVault`] for generating public random seed.
pub trait PublicRandomSeedGenerator {
    /// Returns a public random [`Seed`].
    /// Public in this context means that the produced randomness MUST NOT be used in
    /// any use cases where the security relies on keeping the randomness secret, e.g.,
    /// generation of cryptographic keys.
    fn new_public_seed(&self) -> Result<Seed, PublicRandomSeedGeneratorError>;
}
