//! Canister Http related public interfaces.
use crate::{
    artifact_pool::UnvalidatedArtifact, consensus_pool::ConsensusPoolCache,
    validation::ValidationError,
};
use ic_base_types::{NumBytes, RegistryVersion};
use ic_types::{
    artifact::CanisterHttpResponseId,
    batch::{CanisterHttpPayload, ValidationContext},
    canister_http::{CanisterHttpResponse, CanisterHttpResponseShare},
    consensus::{Payload, Threshold},
    crypto::{CryptoError, CryptoHashOf},
    messages::CallbackId,
    registry::RegistryClientError,
    Height, NodeId, Time,
};

#[derive(Debug)]
pub enum CanisterHttpPermanentValidationError {
    /// The [`CanisterHttpPayload`] is too large
    PayloadTooBig {
        expected: usize,
        received: usize,
    },
    /// There are too many responses in the payload
    TooManyResponses {
        expected: usize,
        received: usize,
    },
    /// The signed metadata does not match the metadata of the content
    InvalidMetadata {
        metadata_id: CallbackId,
        content_id: CallbackId,
        metadata_timeout: Time,
        content_timeout: Time,
    },
    /// The content hash of the signed metadata does not match the actual hash of the content
    ContentHashMismatch {
        metadata_hash: CryptoHashOf<CanisterHttpResponse>,
        calculated_hash: CryptoHashOf<CanisterHttpResponse>,
    },
    /// The response has already timed out
    Timeout {
        timed_out_at: Time,
        validation_time: Time,
    },
    /// A timeout refers to a CallbackId that is unknown by the StateManager
    UnknownCallbackId(CallbackId),
    /// A CallbackId was included as a timeout, however the Request has not timed out at all
    NotTimedOut(CallbackId),
    /// The registry version of a response does not match the validation context
    RegistryVersionMismatch {
        expected: RegistryVersion,
        received: RegistryVersion,
    },
    /// There was an error with a signature calculation
    SignatureError(Box<CryptoError>),
    /// Some of the signatures in the canister http proof were not members of
    /// the canister http committee.
    SignersNotMembers {
        committee: Vec<NodeId>,
        invalid_signers: Vec<NodeId>,
        valid_signers: Vec<NodeId>,
    },
    /// There were not enough signers in the canister http response proof
    NotEnoughSigners {
        committee: Vec<NodeId>,
        signers: Vec<NodeId>,
        expected_threshold: Threshold,
    },
    /// The payload contains a duplicate response
    DuplicateResponse(CallbackId),
    DivergenceProofContainsMultipleCallbackIds,
    DivergenceProofDoesNotMeetDivergenceCriteria,
}

/// A transient error that can occur during validation of a [`CanisterHttpPayload`]
#[derive(Debug)]
pub enum CanisterHttpTransientValidationError {
    /// The registry for this subnet could not be retrieved
    RegistryUnavailable(RegistryClientError),
    /// The state was not available at the time of validation
    StateUnavailable,
    /// The consensus registry version could not be retreived from the summary
    ConsensusRegistryVersionUnavailable,
    /// The feature is not enabled
    Disabled,
    /// Membership Issue
    Membership,
}

pub type CanisterHttpPayloadValidationError =
    ValidationError<CanisterHttpPermanentValidationError, CanisterHttpTransientValidationError>;

pub enum CanisterHttpChangeAction {
    AddToValidated(CanisterHttpResponseShare, CanisterHttpResponse),
    MoveToValidated(CanisterHttpResponseId),
    RemoveValidated(CanisterHttpResponseId),
    RemoveUnvalidated(CanisterHttpResponseId),
    RemoveContent(CryptoHashOf<CanisterHttpResponse>),
    HandleInvalid(CanisterHttpResponseId, String),
}

pub type CanisterHttpChangeSet = Vec<CanisterHttpChangeAction>;

/// Artifact pool for the ECDSA messages (query interface)
pub trait CanisterHttpPool: Send + Sync {
    fn get_validated_shares(&self) -> Box<dyn Iterator<Item = &CanisterHttpResponseShare> + '_>;
    fn get_unvalidated_shares(&self) -> Box<dyn Iterator<Item = &CanisterHttpResponseShare> + '_>;
    // TODO: Likely not needed
    fn get_response_content_items(
        &self,
    ) -> Box<dyn Iterator<Item = (&CryptoHashOf<CanisterHttpResponse>, &CanisterHttpResponse)> + '_>;

    fn get_response_content_by_hash(
        &self,
        hash: &CryptoHashOf<CanisterHttpResponse>,
    ) -> Option<CanisterHttpResponse>;

    fn lookup_validated(
        &self,
        msg_id: &CanisterHttpResponseId,
    ) -> Option<CanisterHttpResponseShare>;

    fn lookup_unvalidated(
        &self,
        msg_id: &CanisterHttpResponseId,
    ) -> Option<CanisterHttpResponseShare>;
}

pub trait MutableCanisterHttpPool: CanisterHttpPool {
    /// Adds the entry to the unvalidated section of the artifact pool.
    fn insert(&mut self, msg: UnvalidatedArtifact<CanisterHttpResponseShare>);

    /// Mutates the artifact pool by applying the change set.
    fn apply_changes(&mut self, change_set: CanisterHttpChangeSet);
}

pub trait CanisterHttpPoolManager: Send {
    /// A function to be invoked every time the canister http pool is changed.
    fn on_state_change(
        &mut self,
        consensus_cache: &dyn ConsensusPoolCache,
        canister_http_pool: &dyn CanisterHttpPool,
    ) -> CanisterHttpChangeSet;
}

pub trait CanisterHttpPayloadBuilder: Send + Sync {
    fn get_canister_http_payload(
        &self,
        height: Height,
        validation_context: &ValidationContext,
        past_payloads: &[&CanisterHttpPayload],
        byte_limit: NumBytes,
    ) -> CanisterHttpPayload;

    fn validate_canister_http_payload(
        &self,
        height: Height,
        payload: &CanisterHttpPayload,
        validation_context: &ValidationContext,
        past_payloads: &[&CanisterHttpPayload],
    ) -> Result<NumBytes, CanisterHttpPayloadValidationError>;

    fn filter_past_payloads<'a>(
        &self,
        past_payloads: &'a [(Height, Time, Payload)],
    ) -> Vec<&'a CanisterHttpPayload> {
        past_payloads
            .iter()
            .filter_map(|(_, _, payload)| {
                if payload.is_summary() {
                    None
                } else {
                    Some(&payload.as_ref().as_data().batch.canister_http)
                }
            })
            .collect()
    }
}
