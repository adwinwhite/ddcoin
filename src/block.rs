use std::{
    fmt::{Display, Formatter},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use ed25519_dalek::{SigningKey, Verifier, VerifyingKey, ed25519::signature::SignerMut};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    CoinAddress, Transaction,
    serdes::hashsig,
    transaction::Signature,
    util::{fmt_hex, hex_to_bytes, num_of_zeros_in_sha256},
};

pub type BlockId = Sha256Hash;

impl BlockId {
    // TODO: store genesis_id as a constant to avoid runtime computation.
    pub fn is_genesis(&self) -> bool {
        *self == Block::GENESIS.id()
    }
}

pub type SequenceNo = u64;

// nanoseconds.
// TODO: consider a more approriate type. u128 doesn't port well to other languages?
pub type Timestamp = u128;

#[derive(Eq, PartialEq, Clone, Copy, Hash, Debug, Serialize, Deserialize)]
pub struct Sha256Hash([u8; 32]);

impl Display for Sha256Hash {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        fmt_hex(&self.0, f)
    }
}

impl From<[u8; 32]> for Sha256Hash {
    fn from(value: [u8; 32]) -> Self {
        Self(value)
    }
}

// FIXME: fixate the serialize order and layout.
#[derive(Eq, PartialEq, Clone, Debug, Serialize, Deserialize)]
struct BlockInner {
    sequence_no: SequenceNo,
    prev_id: BlockId,
    transactions: Vec<Transaction>,
    miner: CoinAddress,
    timestamp: Timestamp,
    nonce: u64,
}

impl BlockInner {
    fn verify_nonce(&self) -> bool {
        // panic risk: How can this serialization fail?
        let bytes = hashsig::encode(self).unwrap();
        let hash = Sha256::digest(&bytes);
        let num_of_zeros = num_of_zeros_in_sha256(hash.as_ref());
        let required_zeros = self.sequence_no / Block::NUM_OF_BLOCKS_BEFORE_INCREMENT_ZEROS + 1;
        (num_of_zeros as u64) >= required_zeros
    }
}

#[derive(Eq, PartialEq, Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "BlockValidator")]
pub struct Block {
    inner: BlockInner,
    signature: Signature,
}

// TODO: Enforce more validation at construction.
// Note: require further validation.
// 0. can this block trace back to genesis?
// 1. is timestamp greater than previous block's and smaller than received time?
// 2. does previous block's hash match? Yes, we use content addressing now.
// 3. does previous block's seqno match?
// [serde validation trick here](https://github.com/serde-rs/serde-rs.github.io/pull/148/files).
// Perhaps I should make a macro to duplicate the struct.
#[derive(Deserialize)]
struct BlockValidator {
    inner: BlockInner,
    signature: Signature,
}

impl std::convert::TryFrom<BlockValidator> for Block {
    type Error = anyhow::Error;
    fn try_from(value: BlockValidator) -> Result<Self, Self::Error> {
        if !value.inner.verify_nonce() {
            return Err(anyhow::anyhow!("Invalid nonce"));
        }
        let msg = crate::serdes::hashsig::encode(&value.inner).unwrap();
        let verifying_key = VerifyingKey::from_bytes(&value.inner.miner.pub_key)?;
        if let Ok(()) = verifying_key.verify(
            &msg,
            &ed25519_dalek::Signature::from_bytes(&value.signature.sig),
        ) {
            Ok(Block {
                inner: value.inner,
                signature: value.signature,
            })
        } else {
            Err(anyhow::anyhow!("Invalid signature"))
        }
    }
}

impl Block {
    pub const BLOCK_TXN_LIMIT: usize = 20;

    // num_of_zeros = seq_no / THIS_CONST + 1;
    pub const NUM_OF_BLOCKS_BEFORE_INCREMENT_ZEROS: u64 = 10;

    pub const GENESIS: Block = Block {
        inner: BlockInner {
            sequence_no: 0,
            prev_id: Sha256Hash([0; 32]),
            transactions: Vec::new(),
            miner: CoinAddress {
                pub_key: hex_to_bytes(
                    "3acee5b5591717dfbb2f773823e916d01e586de6695bb07b9665428cf88df30d",
                ),
            },
            timestamp: Duration::from_secs(1737430342).as_nanos(),
            nonce: 0,
        },
        signature: Signature {
            sig: hex_to_bytes(
                "e55aa435764adddf0eb9ea47f3836caf0a12d8d12c05e552a15d3cf5f1469c6adc90d16db5654983f1580caad1304ef67874a4e85682c91cb47aa1db82898701",
            ),
        },
    };

    pub fn id(&self) -> BlockId {
        self.sha256()
    }

    pub fn prev_id(&self) -> BlockId {
        self.inner.prev_id
    }

    pub fn seqno(&self) -> SequenceNo {
        self.inner.sequence_no
    }

    pub fn sha256(&self) -> Sha256Hash {
        let bytes = hashsig::encode(&self).unwrap();
        let array: [u8; 32] = Sha256::digest(&bytes).into();
        array.into()
    }

    pub fn transactions(&self) -> &[Transaction] {
        &self.inner.transactions
    }
    pub fn timestamp(&self) -> Timestamp {
        self.inner.timestamp
    }
}

impl Display for Block {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Block: seqno: {}, id: {}, prev_id: {}, miner: {}, nouce: {}, ",
            self.inner.sequence_no,
            self.id(),
            self.inner.prev_id,
            self.inner.miner,
            self.inner.nonce
        )?;
        write!(f, ", transactions: ")?;
        write!(f, "[")?;
        for txn in &self.inner.transactions {
            write!(f, "{}, ", txn.id())?;
        }
        write!(f, "]")
    }
}

#[derive(Eq, PartialEq, Clone, Debug, Serialize, Deserialize)]
pub struct UnconfirmedBlock {
    sequence_no: u64,
    prev_id: BlockId,
    transactions: Vec<Transaction>,
    miner: CoinAddress,
    timestamp: Timestamp,
}

#[derive(Debug)]
pub enum BlockValidationError {
    BlockSizeExceeded,
    MinerNotMatchSigner,
    InvalidNonce,
}

impl Display for BlockValidationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            BlockValidationError::BlockSizeExceeded => write!(f, "Block size exceeded"),
            BlockValidationError::MinerNotMatchSigner => write!(f, "Miner not match signer"),
            BlockValidationError::InvalidNonce => write!(f, "Invalid nonce"),
        }
    }
}

impl std::error::Error for BlockValidationError {}

// TODO: eliminate the need of passing miner address and signer at different places.
impl UnconfirmedBlock {
    pub fn new(prev: &Block, miner: CoinAddress, txns: Vec<Transaction>) -> Self {
        Self {
            sequence_no: prev.seqno() + 1,
            prev_id: prev.id(),
            transactions: txns,
            miner,
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap(/* can't be ealier than epoch */)
                .as_nanos(),
        }
    }

    pub fn with_nonce(
        self,
        signing_key: &mut SigningKey,
        nonce: u64,
    ) -> Result<Block, BlockValidationError> {
        let inner = BlockInner {
            sequence_no: self.sequence_no,
            prev_id: self.prev_id,
            transactions: self.transactions,
            miner: self.miner,
            timestamp: self.timestamp,
            nonce,
        };
        if inner.transactions.len() > Block::BLOCK_TXN_LIMIT {
            return Err(BlockValidationError::BlockSizeExceeded);
        }
        if inner.miner != signing_key.verifying_key().into() {
            return Err(BlockValidationError::MinerNotMatchSigner);
        }
        // panic risk: How can this serialization fail?
        let bytes = hashsig::encode(&inner).unwrap();
        let hash = Sha256::digest(&bytes);
        let num_of_zeros = num_of_zeros_in_sha256(hash.as_ref());
        let required_zeros = self.sequence_no / Block::NUM_OF_BLOCKS_BEFORE_INCREMENT_ZEROS + 1;
        if (num_of_zeros as u64) < required_zeros {
            return Err(BlockValidationError::InvalidNonce);
        }

        let signature = signing_key.sign(&bytes).into();
        let block = Block { inner, signature };
        Ok(block)
    }

    // FIXME: should I keep this method here? Or move it to test util?
    pub fn find_nouce(&self) -> u64 {
        let required_zeros = self.sequence_no / Block::NUM_OF_BLOCKS_BEFORE_INCREMENT_ZEROS + 1;

        let mut bytes = hashsig::encode(self).unwrap();
        let len = bytes.len();
        bytes.extend_from_slice(&[0; 8]);
        let mut valid_nouce = 0;
        for nouce in 0_u64.. {
            bytes[len..].copy_from_slice(&nouce.to_le_bytes()[..]);
            let hash = Sha256::digest(&bytes);
            let num_of_zeros = num_of_zeros_in_sha256(hash.as_ref());
            if (num_of_zeros as u64) >= required_zeros {
                valid_nouce = nouce;
                break;
            }
        }
        valid_nouce
    }

    // FIXME: should I keep this method here? Or move it to test util?
    pub fn try_confirm(self, signing_key: &mut SigningKey) -> Result<Block, BlockValidationError> {
        let valid_nouce = self.find_nouce();
        self.with_nonce(signing_key, valid_nouce)
    }
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::SigningKey;

    use crate::{UnconfirmedBlock, block::Sha256Hash};

    #[test]
    fn generate_genesis_block() {
        let mut csprng = rand::rngs::OsRng;
        let mut signing_key = SigningKey::generate(&mut csprng);
        let miner = signing_key.verifying_key().into();
        let unconfirmed = UnconfirmedBlock {
            sequence_no: 0,
            prev_id: Sha256Hash([0; 32]),
            transactions: Vec::new(),
            miner,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        };

        let genesis_block = unconfirmed.try_confirm(&mut signing_key).unwrap();
        println!("Genesis block: {}", genesis_block);
        println!("Miner: {}", genesis_block.inner.miner);
        println!("Signature: {}", genesis_block.signature);
    }
}
