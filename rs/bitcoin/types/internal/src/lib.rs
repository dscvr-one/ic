//! The Rust types in this crate are used by consensus as part of the
//! BatchPayload and by execution as part of the ReplicatedState. The rust
//! types need to be serializable only because of the BatchPayload.
//! Their protobuf counterpart (defined in rs/protobuf/def/bitcoin) are used
//! only for serialization/deserialization of the ReplicatedState.

use candid::CandidType;
use ic_btc_types::NetworkSnakeCase as Network;
use ic_protobuf::{
    bitcoin::v1,
    proxy::{try_from_option_field, ProxyDecodeError},
};
use serde::{Deserialize, Serialize};
use std::convert::{TryFrom, TryInto};
use std::mem::size_of_val;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GetSuccessorsRequest {
    pub processed_block_hashes: Vec<Vec<u8>>,
    pub anchor: Vec<u8>,
}

impl From<&GetSuccessorsRequest> for v1::GetSuccessorsRequest {
    fn from(request: &GetSuccessorsRequest) -> Self {
        v1::GetSuccessorsRequest {
            processed_block_hashes: request.processed_block_hashes.clone(),
            anchor: request.anchor.clone(),
        }
    }
}

impl From<v1::GetSuccessorsRequest> for GetSuccessorsRequest {
    fn from(request: v1::GetSuccessorsRequest) -> Self {
        GetSuccessorsRequest {
            processed_block_hashes: request.processed_block_hashes,
            anchor: request.anchor,
        }
    }
}

#[derive(CandidType, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanisterSendTransactionRequest {
    pub network: Network,
    #[serde(with = "serde_bytes")]
    pub transaction: Vec<u8>,
}

impl From<&CanisterSendTransactionRequest> for v1::CanisterSendTransactionRequest {
    fn from(request: &CanisterSendTransactionRequest) -> Self {
        Self {
            network: match request.network {
                Network::Testnet => 1,
                Network::Mainnet => 2,
                Network::Regtest => 3,
            },
            transaction: request.transaction.clone(),
        }
    }
}

impl TryFrom<v1::CanisterSendTransactionRequest> for CanisterSendTransactionRequest {
    type Error = ProxyDecodeError;
    fn try_from(request: v1::CanisterSendTransactionRequest) -> Result<Self, Self::Error> {
        Ok(CanisterSendTransactionRequest {
            network: match request.network {
                1 => Network::Testnet,
                2 => Network::Mainnet,
                3 => Network::Regtest,
                _ => {
                    return Err(ProxyDecodeError::MissingField(
                        "CanisterSendTransactionRequest::network",
                    ))
                }
            },
            transaction: request.transaction,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendTransactionRequest {
    pub transaction: Vec<u8>,
}

impl From<&SendTransactionRequest> for v1::SendTransactionRequest {
    fn from(request: &SendTransactionRequest) -> Self {
        v1::SendTransactionRequest {
            transaction: request.transaction.clone(),
        }
    }
}

impl From<v1::SendTransactionRequest> for SendTransactionRequest {
    fn from(request: v1::SendTransactionRequest) -> Self {
        SendTransactionRequest {
            transaction: request.transaction,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BitcoinAdapterRequestWrapper {
    GetSuccessorsRequest(GetSuccessorsRequest),
    SendTransactionRequest(SendTransactionRequest),
    // A request from the bitcoin wasm canister.
    // This supersedes `GetSuccessorsRequest`, which will be deleted
    // once the Bitcoin replica canister is phased out.
    CanisterGetSuccessorsRequest(CanisterGetSuccessorsRequestInitial),
    CanisterSendTransactionRequest(CanisterSendTransactionRequest),
}

impl BitcoinAdapterRequestWrapper {
    pub fn to_request_type_label(&self) -> &str {
        match self {
            BitcoinAdapterRequestWrapper::GetSuccessorsRequest(_) => "get_successors",
            BitcoinAdapterRequestWrapper::SendTransactionRequest(_) => "send_transaction",
            BitcoinAdapterRequestWrapper::CanisterGetSuccessorsRequest(_) => "get_successors",
            BitcoinAdapterRequestWrapper::CanisterSendTransactionRequest(_) => "send_transaction",
        }
    }

    /// Returns which bitcoin network the request is for.
    pub fn network(&self) -> Network {
        match self {
            BitcoinAdapterRequestWrapper::GetSuccessorsRequest(_)
            | BitcoinAdapterRequestWrapper::SendTransactionRequest(_) => {
                // NOTE: These are the deprecated replica requests that do not contain a network
                // parameter. These requests do not trigger this code path and they'll be
                // deleted in EXC-1239. For now, return a stub response.
                Network::Testnet
            }
            BitcoinAdapterRequestWrapper::CanisterGetSuccessorsRequest(
                CanisterGetSuccessorsRequestInitial { network, .. },
            ) => *network,
            BitcoinAdapterRequestWrapper::CanisterSendTransactionRequest(
                CanisterSendTransactionRequest { network, .. },
            ) => *network,
        }
    }
}

impl From<&BitcoinAdapterRequestWrapper> for v1::BitcoinAdapterRequestWrapper {
    fn from(request_wrapper: &BitcoinAdapterRequestWrapper) -> Self {
        match request_wrapper {
            BitcoinAdapterRequestWrapper::GetSuccessorsRequest(request) => {
                v1::BitcoinAdapterRequestWrapper {
                    r: Some(
                        v1::bitcoin_adapter_request_wrapper::R::GetSuccessorsRequest(
                            request.into(),
                        ),
                    ),
                }
            }
            BitcoinAdapterRequestWrapper::SendTransactionRequest(request) => {
                v1::BitcoinAdapterRequestWrapper {
                    r: Some(
                        v1::bitcoin_adapter_request_wrapper::R::SendTransactionRequest(
                            request.into(),
                        ),
                    ),
                }
            }
            BitcoinAdapterRequestWrapper::CanisterGetSuccessorsRequest(request) => {
                v1::BitcoinAdapterRequestWrapper {
                    r: Some(
                        v1::bitcoin_adapter_request_wrapper::R::CanisterGetSuccessorsRequest(
                            request.into(),
                        ),
                    ),
                }
            }
            BitcoinAdapterRequestWrapper::CanisterSendTransactionRequest(request) => {
                v1::BitcoinAdapterRequestWrapper {
                    r: Some(
                        v1::bitcoin_adapter_request_wrapper::R::CanisterSendTransactionRequest(
                            request.into(),
                        ),
                    ),
                }
            }
        }
    }
}

impl TryFrom<v1::BitcoinAdapterRequestWrapper> for BitcoinAdapterRequestWrapper {
    type Error = ProxyDecodeError;
    fn try_from(request_wrapper: v1::BitcoinAdapterRequestWrapper) -> Result<Self, Self::Error> {
        match request_wrapper.r.ok_or(ProxyDecodeError::MissingField(
            "BitcoinAdapterRequestWrapper::r",
        ))? {
            v1::bitcoin_adapter_request_wrapper::R::GetSuccessorsRequest(r) => Ok(
                BitcoinAdapterRequestWrapper::GetSuccessorsRequest(r.try_into()?),
            ),
            v1::bitcoin_adapter_request_wrapper::R::SendTransactionRequest(r) => Ok(
                BitcoinAdapterRequestWrapper::SendTransactionRequest(r.try_into()?),
            ),
            v1::bitcoin_adapter_request_wrapper::R::CanisterGetSuccessorsRequest(r) => Ok(
                BitcoinAdapterRequestWrapper::CanisterGetSuccessorsRequest(r.try_into()?),
            ),
            v1::bitcoin_adapter_request_wrapper::R::CanisterSendTransactionRequest(r) => {
                Ok(BitcoinAdapterRequestWrapper::CanisterSendTransactionRequest(r.try_into()?))
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BitcoinAdapterRequest {
    pub request: BitcoinAdapterRequestWrapper,
    pub callback_id: u64,
}

impl From<&BitcoinAdapterRequest> for v1::BitcoinAdapterRequest {
    fn from(request: &BitcoinAdapterRequest) -> Self {
        v1::BitcoinAdapterRequest {
            request: Some((&request.request).into()),
            callback_id: request.callback_id,
        }
    }
}

impl TryFrom<v1::BitcoinAdapterRequest> for BitcoinAdapterRequest {
    type Error = ProxyDecodeError;
    fn try_from(request: v1::BitcoinAdapterRequest) -> Result<Self, Self::Error> {
        let wrapped_request =
            try_from_option_field(request.request, "BitcoinAdapterRequest::request")?;
        Ok(BitcoinAdapterRequest {
            request: wrapped_request,
            callback_id: request.callback_id,
        })
    }
}
//--------------------------- BLOCK HEADER ------------------------------------
#[derive(Clone, Debug, Default, Hash, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlockHeader {
    pub version: i32,
    #[serde(with = "serde_bytes")]
    pub prev_blockhash: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub merkle_root: Vec<u8>,
    pub time: u32,
    pub bits: u32,
    pub nonce: u32,
}

impl BlockHeader {
    /// Returns the size of this `BlockHeader` in bytes.
    pub fn count_bytes(&self) -> usize {
        size_of_val(&self.version)
            + self.prev_blockhash.len()
            + self.merkle_root.len()
            + size_of_val(&self.time)
            + size_of_val(&self.bits)
            + size_of_val(&self.nonce)
    }
}

impl From<&BlockHeader> for v1::BlockHeader {
    fn from(header: &BlockHeader) -> Self {
        v1::BlockHeader {
            version: header.version,
            prev_blockhash: header.prev_blockhash.clone(),
            merkle_root: header.merkle_root.clone(),
            time: header.time,
            bits: header.bits,
            nonce: header.nonce,
        }
    }
}

impl From<v1::BlockHeader> for BlockHeader {
    fn from(header: v1::BlockHeader) -> Self {
        BlockHeader {
            version: header.version,
            prev_blockhash: header.prev_blockhash,
            merkle_root: header.merkle_root,
            time: header.time,
            bits: header.bits,
            nonce: header.nonce,
        }
    }
}

//--------------------------- BLOCK -------------------------------------------
#[derive(Clone, Debug, Default, Hash, Serialize, Deserialize, Eq, PartialEq)]
pub struct Block {
    pub header: BlockHeader,
    pub txdata: Vec<Transaction>,
}

impl Block {
    pub fn count_bytes(&self) -> usize {
        self.header.count_bytes() + self.txdata.iter().map(|tx| tx.count_bytes()).sum::<usize>()
    }
}

impl From<&Block> for v1::Block {
    fn from(block: &Block) -> v1::Block {
        v1::Block {
            header: Some(v1::BlockHeader::from(&block.header)),
            txdata: block
                .txdata
                .iter()
                .map(|x| v1::Transaction {
                    version: x.version,
                    lock_time: x.lock_time,
                    input: x.input.iter().map(v1::TxIn::from).collect(),
                    output: x
                        .output
                        .iter()
                        .map(|x| v1::TxOut {
                            value: x.value,
                            script_pubkey: x.script_pubkey.clone(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

fn validate_hash(bytes: Vec<u8>) -> Result<Vec<u8>, ProxyDecodeError> {
    if bytes.len() != HASH_LEN {
        Err(ProxyDecodeError::InvalidDigestLength {
            expected: HASH_LEN,
            actual: bytes.len(),
        })
    } else {
        Ok(bytes)
    }
}

impl TryFrom<v1::Block> for Block {
    type Error = ProxyDecodeError;

    fn try_from(block: v1::Block) -> Result<Block, ProxyDecodeError> {
        let header = match block.header {
            Some(header) => BlockHeader {
                version: header.version,
                prev_blockhash: validate_hash(header.prev_blockhash)?,
                merkle_root: validate_hash(header.merkle_root)?,
                time: header.time,
                bits: header.bits,
                nonce: header.nonce,
            },
            None => return Err(ProxyDecodeError::MissingField("Bitcoin::BlockHeader")),
        };

        let mut txdata: Vec<Transaction> = vec![];
        for tx in block.txdata.into_iter() {
            let mut input = vec![];
            for txin in tx.input.into_iter() {
                input.push(TxIn::try_from(txin)?);
            }
            txdata.push(Transaction {
                version: tx.version,
                lock_time: tx.lock_time,
                input,
                output: tx
                    .output
                    .into_iter()
                    .map(|x| TxOut {
                        value: x.value,
                        script_pubkey: x.script_pubkey,
                    })
                    .collect(),
            });
        }

        Ok(Block { header, txdata })
    }
}

#[derive(Clone, Debug, Default, Hash, Serialize, Deserialize, PartialEq, Eq)]
pub struct Transaction {
    pub version: i32,
    pub lock_time: u32,
    pub input: Vec<TxIn>,
    pub output: Vec<TxOut>,
}

impl Transaction {
    pub fn count_bytes(&self) -> usize {
        size_of_val(&self.version)
            + size_of_val(&self.lock_time)
            + self.input.iter().map(|i| i.count_bytes()).sum::<usize>()
            + self.output.iter().map(|o| o.count_bytes()).sum::<usize>()
    }
}

pub const HASH_LEN: usize = 32;

pub type Hash = [u8; HASH_LEN];
pub type Txid = Hash;

#[derive(Clone, Debug, Default, Hash, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutPoint {
    pub txid: Txid,
    pub vout: u32,
}

impl OutPoint {
    pub fn count_bytes(&self) -> usize {
        size_of_val(&self.txid) + size_of_val(&self.vout)
    }
}
//--------------------------- TxIn --------------------------------------------

#[derive(Clone, Debug, Default, Hash, Serialize, Deserialize, PartialEq, Eq)]
pub struct TxIn {
    pub previous_output: OutPoint,
    #[serde(with = "serde_bytes")]
    pub script_sig: Vec<u8>,
    pub sequence: u32,
    pub witness: Vec<serde_bytes::ByteBuf>,
}

impl TxIn {
    pub fn count_bytes(&self) -> usize {
        self.previous_output.count_bytes()
            + self.script_sig.len()
            + size_of_val(&self.sequence)
            + self.witness.iter().map(|w| w.len()).sum::<usize>()
    }
}

impl From<&TxIn> for v1::TxIn {
    fn from(txin: &TxIn) -> v1::TxIn {
        v1::TxIn {
            previous_output: Some(v1::OutPoint {
                txid: txin.previous_output.txid.to_vec(),
                vout: txin.previous_output.vout,
            }),
            script_sig: txin.script_sig.to_vec(),
            sequence: txin.sequence,
            witness: txin.witness.iter().map(|v| v.to_vec()).collect(),
        }
    }
}

impl TryFrom<v1::TxIn> for TxIn {
    type Error = ProxyDecodeError;
    fn try_from(txin: v1::TxIn) -> Result<TxIn, ProxyDecodeError> {
        let previous_output = match txin.previous_output {
            Some(previous_output) => previous_output,
            None => return Err(ProxyDecodeError::MissingField("Bitcoin::OutPoint")),
        };
        Ok(TxIn {
            previous_output: OutPoint {
                txid: Txid::try_from(&previous_output.txid[..])
                    .map_err(|e| ProxyDecodeError::Other(e.to_string()))?,
                vout: previous_output.vout,
            },
            script_sig: txin.script_sig,
            sequence: txin.sequence,
            witness: txin
                .witness
                .into_iter()
                .map(serde_bytes::ByteBuf::from)
                .collect(),
        })
    }
}

#[derive(Clone, Debug, Default, Hash, Serialize, Deserialize, PartialEq, Eq)]
pub struct TxOut {
    pub value: u64,
    #[serde(with = "serde_bytes")]
    pub script_pubkey: Vec<u8>,
}

impl TxOut {
    pub fn count_bytes(&self) -> usize {
        size_of_val(&self.value) + self.script_pubkey.len()
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, Hash, PartialEq, Eq)]
pub struct GetSuccessorsResponse {
    pub blocks: Vec<Block>,
    pub next: Vec<BlockHeader>,
}

impl GetSuccessorsResponse {
    /// Returns the size of this `GetSuccessorsResponse` in bytes.
    pub fn count_bytes(&self) -> usize {
        self.blocks.iter().map(|x| x.count_bytes()).sum::<usize>()
            + self.next.iter().map(|x| x.count_bytes()).sum::<usize>()
    }
}

impl From<&GetSuccessorsResponse> for v1::GetSuccessorsResponse {
    fn from(response: &GetSuccessorsResponse) -> Self {
        v1::GetSuccessorsResponse {
            blocks: response.blocks.iter().map(v1::Block::from).collect(),
            next: response.next.iter().map(v1::BlockHeader::from).collect(),
        }
    }
}

impl TryFrom<v1::GetSuccessorsResponse> for GetSuccessorsResponse {
    type Error = ProxyDecodeError;
    fn try_from(response: v1::GetSuccessorsResponse) -> Result<Self, Self::Error> {
        let mut blocks = vec![];
        for b in response.blocks.into_iter() {
            blocks.push(Block::try_from(b)?);
        }
        Ok(GetSuccessorsResponse {
            blocks,
            next: response.next.into_iter().map(BlockHeader::from).collect(),
        })
    }
}

#[derive(Clone, Debug, Default, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendTransactionResponse {}

impl From<&SendTransactionResponse> for v1::SendTransactionResponse {
    fn from(_response: &SendTransactionResponse) -> Self {
        v1::SendTransactionResponse {}
    }
}

impl From<v1::SendTransactionResponse> for SendTransactionResponse {
    fn from(_response: v1::SendTransactionResponse) -> Self {
        SendTransactionResponse {}
    }
}

impl SendTransactionResponse {
    /// Returns the size of this `SendTransactionResponse` in bytes.
    pub fn count_bytes(&self) -> usize {
        0
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub enum BitcoinAdapterResponseWrapper {
    GetSuccessorsResponse(GetSuccessorsResponse),
    SendTransactionResponse(SendTransactionResponse),
    // Responses for the bitcoin wasm canister.
    // These supersede the above responses, which will be deleted
    // once the Bitcoin replica canister is phased out.
    CanisterGetSuccessorsResponse(CanisterGetSuccessorsResponseComplete),
    CanisterSendTransactionResponse(SendTransactionResponse),
}

impl BitcoinAdapterResponseWrapper {
    /// Returns the size of this `BitcoinAdapterResponseWrapper` in bytes.
    fn count_bytes(&self) -> usize {
        match self {
            BitcoinAdapterResponseWrapper::GetSuccessorsResponse(r) => r.count_bytes(),
            BitcoinAdapterResponseWrapper::SendTransactionResponse(r) => r.count_bytes(),
            BitcoinAdapterResponseWrapper::CanisterGetSuccessorsResponse(r) => r.count_bytes(),
            BitcoinAdapterResponseWrapper::CanisterSendTransactionResponse(r) => r.count_bytes(),
        }
    }
}

impl From<&BitcoinAdapterResponseWrapper> for v1::BitcoinAdapterResponseWrapper {
    fn from(response_wrapper: &BitcoinAdapterResponseWrapper) -> Self {
        match response_wrapper {
            BitcoinAdapterResponseWrapper::GetSuccessorsResponse(response) => {
                v1::BitcoinAdapterResponseWrapper {
                    r: Some(
                        v1::bitcoin_adapter_response_wrapper::R::GetSuccessorsResponse(
                            response.into(),
                        ),
                    ),
                }
            }
            BitcoinAdapterResponseWrapper::SendTransactionResponse(response) => {
                v1::BitcoinAdapterResponseWrapper {
                    r: Some(
                        v1::bitcoin_adapter_response_wrapper::R::SendTransactionResponse(
                            response.into(),
                        ),
                    ),
                }
            }
            BitcoinAdapterResponseWrapper::CanisterGetSuccessorsResponse(response) => {
                v1::BitcoinAdapterResponseWrapper {
                    r: Some(
                        v1::bitcoin_adapter_response_wrapper::R::CanisterGetSuccessorsResponse(
                            response.into(),
                        ),
                    ),
                }
            }
            BitcoinAdapterResponseWrapper::CanisterSendTransactionResponse(response) => {
                v1::BitcoinAdapterResponseWrapper {
                    r: Some(
                        v1::bitcoin_adapter_response_wrapper::R::CanisterSendTransactionResponse(
                            response.into(),
                        ),
                    ),
                }
            }
        }
    }
}

impl TryFrom<v1::BitcoinAdapterResponseWrapper> for BitcoinAdapterResponseWrapper {
    type Error = ProxyDecodeError;
    fn try_from(response_wrapper: v1::BitcoinAdapterResponseWrapper) -> Result<Self, Self::Error> {
        match response_wrapper.r.ok_or(ProxyDecodeError::MissingField(
            "BitcoinAdapterResponseWrapper::r",
        ))? {
            v1::bitcoin_adapter_response_wrapper::R::GetSuccessorsResponse(r) => Ok(
                BitcoinAdapterResponseWrapper::GetSuccessorsResponse(r.try_into()?),
            ),
            v1::bitcoin_adapter_response_wrapper::R::SendTransactionResponse(r) => Ok(
                BitcoinAdapterResponseWrapper::SendTransactionResponse(r.try_into()?),
            ),
            v1::bitcoin_adapter_response_wrapper::R::CanisterGetSuccessorsResponse(r) => {
                Ok(BitcoinAdapterResponseWrapper::CanisterGetSuccessorsResponse(r.try_into()?))
            }
            v1::bitcoin_adapter_response_wrapper::R::CanisterSendTransactionResponse(r) => {
                Ok(BitcoinAdapterResponseWrapper::CanisterSendTransactionResponse(r.try_into()?))
            }
        }
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct BitcoinAdapterResponse {
    pub response: BitcoinAdapterResponseWrapper,
    pub callback_id: u64,
}

impl From<&BitcoinAdapterResponse> for v1::BitcoinAdapterResponse {
    fn from(response: &BitcoinAdapterResponse) -> Self {
        v1::BitcoinAdapterResponse {
            response: Some((&response.response).into()),
            callback_id: response.callback_id,
        }
    }
}

impl TryFrom<v1::BitcoinAdapterResponse> for BitcoinAdapterResponse {
    type Error = ProxyDecodeError;
    fn try_from(response: v1::BitcoinAdapterResponse) -> Result<Self, Self::Error> {
        let wrapped_response =
            try_from_option_field(response.response, "BitcoinAdapterResponse::response")?;
        Ok(BitcoinAdapterResponse {
            response: wrapped_response,
            callback_id: response.callback_id,
        })
    }
}

impl BitcoinAdapterResponse {
    /// Returns the size of this `BitcoinAdapterResponse` in bytes.
    pub fn count_bytes(&self) -> usize {
        self.response.count_bytes() + std::mem::size_of::<u64>()
    }
}

// A blob representing a block in the standard bitcoin format.
pub type BlockBlob = Vec<u8>;

// A blob representing a block header in the standard bitcoin format.
type BlockHeaderBlob = Vec<u8>;

type BlockHash = Vec<u8>;

type PageNumber = u8;

/// A request to retrieve more blocks from the Bitcoin network.
///
/// ```text
/// variant {
///   initial : record {
///     network: network;
///     anchor: blob;
///     processed_block_hashes: vec blob;
///   };
///   follow_up : nat8;
/// };
/// ```
#[derive(CandidType, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CanisterGetSuccessorsRequest {
    /// A request containing the hashes of blocks we'd like to retrieve succeessors for.
    #[serde(rename = "initial")]
    Initial(CanisterGetSuccessorsRequestInitial),

    /// A follow-up request to retrieve the `FollowUp` response associated with the given page.
    #[serde(rename = "follow_up")]
    FollowUp(PageNumber),
}

#[derive(CandidType, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanisterGetSuccessorsRequestInitial {
    pub network: Network,
    pub anchor: BlockHash,
    pub processed_block_hashes: Vec<BlockHash>,
}

impl From<&CanisterGetSuccessorsRequestInitial> for v1::CanisterGetSuccessorsRequestInitial {
    fn from(request: &CanisterGetSuccessorsRequestInitial) -> Self {
        Self {
            network: match request.network {
                Network::Testnet => 1,
                Network::Mainnet => 2,
                Network::Regtest => 3,
            },
            anchor: request.anchor.clone(),
            processed_block_hashes: request.processed_block_hashes.clone(),
        }
    }
}

impl TryFrom<v1::CanisterGetSuccessorsRequestInitial> for CanisterGetSuccessorsRequestInitial {
    type Error = ProxyDecodeError;
    fn try_from(request: v1::CanisterGetSuccessorsRequestInitial) -> Result<Self, Self::Error> {
        Ok(CanisterGetSuccessorsRequestInitial {
            network: match request.network {
                1 => Network::Testnet,
                2 => Network::Mainnet,
                3 => Network::Regtest,
                _ => {
                    return Err(ProxyDecodeError::MissingField(
                        "CanisterGetSuccessorsRequestInitial::network",
                    ))
                }
            },
            anchor: request.anchor,
            processed_block_hashes: request.processed_block_hashes,
        })
    }
}

/// A response containing new successor blocks from the Bitcoin network.
///
/// ```text
/// variant {
///   complete : record {
///     blocks: vec blob;
///     next: vec blob;
///   };
///
///   partial : record {
///     partial_block: blob;
///     next: vec blob;
///     remaining_follow_ups: nat8;
///   };
///
///   follow_up : blob;
/// };
/// ```
#[derive(CandidType, Clone, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub enum CanisterGetSuccessorsResponse {
    /// A complete response that doesn't require pagination.
    #[serde(rename = "complete")]
    Complete(CanisterGetSuccessorsResponseComplete),

    /// A partial response that requires `FollowUp` responses to get the rest of it.
    #[serde(rename = "partial")]
    Partial(CanisterGetSuccessorsResponsePartial),

    /// A follow-up response containing a blob of bytes to be appended to the partial response.
    #[serde(rename = "follow_up")]
    FollowUp(BlockBlob),
}

#[derive(CandidType, Clone, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanisterGetSuccessorsResponseComplete {
    pub blocks: Vec<BlockBlob>,
    pub next: Vec<BlockHeaderBlob>,
}

impl CanisterGetSuccessorsResponseComplete {
    /// Returns the size of this `SendTransactionResponse` in bytes.
    pub fn count_bytes(&self) -> usize {
        self.count_blocks_bytes() + self.count_next_bytes()
    }

    pub fn count_blocks_bytes(&self) -> usize {
        self.blocks.iter().map(|b| b.len()).sum::<usize>()
    }

    pub fn count_next_bytes(&self) -> usize {
        self.next.iter().map(|n| n.len()).sum::<usize>()
    }
}

impl From<&CanisterGetSuccessorsResponseComplete> for v1::CanisterGetSuccessorsResponseComplete {
    fn from(request: &CanisterGetSuccessorsResponseComplete) -> Self {
        v1::CanisterGetSuccessorsResponseComplete {
            blocks: request.blocks.clone(),
            next: request.next.clone(),
        }
    }
}

impl TryFrom<v1::CanisterGetSuccessorsResponseComplete> for CanisterGetSuccessorsResponseComplete {
    type Error = ProxyDecodeError;
    fn try_from(response: v1::CanisterGetSuccessorsResponseComplete) -> Result<Self, Self::Error> {
        Ok(CanisterGetSuccessorsResponseComplete {
            blocks: response.blocks,
            next: response.next,
        })
    }
}

#[derive(CandidType, Clone, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanisterGetSuccessorsResponsePartial {
    /// A block that is partial (i.e. the full blob has not been sent).
    pub partial_block: BlockBlob,

    /// Hashes of next block headers.
    pub next: Vec<BlockHeaderBlob>,

    /// The remaining number of follow ups to this response, which can retrieved
    /// via `FollowUp` requests.
    pub remaining_follow_ups: u8,
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn get_successors_response_complete_count_bytes() {
        assert_eq!(
            CanisterGetSuccessorsResponseComplete {
                blocks: vec![],
                next: vec![],
            }
            .count_bytes(),
            0
        );

        assert_eq!(
            CanisterGetSuccessorsResponseComplete {
                blocks: vec![vec![1, 2, 3], vec![4, 5, 6]],
                next: vec![vec![7, 8, 9, 10], vec![11, 12]],
            }
            .count_bytes(),
            12
        );
    }
}
