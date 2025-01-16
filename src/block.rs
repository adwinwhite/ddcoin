use std::fmt::{Display, Formatter};

use ed25519_dalek::{SigningKey, Verifier, VerifyingKey, ed25519::signature::SignerMut};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    CoinAddress, Transaction,
    serdes::hashsig,
    transaction::Signature,
    util::{fmt_hex, hex_to_bytes, num_of_zeros_in_sha256},
};

pub const BLOCK_SIZE_LIMIT: usize = 20;
// num_of_zeros = seq_no / THIS_CONST + 1;
pub const NUM_OF_BLOCKS_BEFORE_INCREMENT_ZEROS: u64 = 10;

#[derive(Eq, PartialEq, Clone, Hash, Copy, Debug, Serialize, Deserialize)]
pub struct BlockId {
    id: Uuid,
}

impl From<Uuid> for BlockId {
    fn from(id: Uuid) -> Self {
        Self { id }
    }
}

impl BlockId {
    pub const GENESIS: BlockId = BlockId { id: Uuid::nil() };
}

pub type SequenceNo = u64;

#[derive(Eq, PartialEq, Clone, Copy, Debug, Serialize, Deserialize)]
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

impl Display for BlockId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.id)
    }
}

// FIXME: fixate the serialize order and layout.
#[derive(Eq, PartialEq, Clone, Debug, Serialize, Deserialize)]
struct BlockInner {
    sequence_no: SequenceNo,
    id: BlockId,
    prev_id: BlockId,
    prev_sha256: Sha256Hash,
    transactions: Vec<Transaction>,
    miner: CoinAddress,
    nonce: u64,
}

impl BlockInner {
    fn verify_nonce(&self) -> bool {
        // panic risk: How can this serialization fail?
        let bytes = hashsig::encode(self).unwrap();
        let hash = Sha256::digest(&bytes);
        let num_of_zeros = num_of_zeros_in_sha256(hash.as_ref());
        let required_zeros = self.sequence_no / NUM_OF_BLOCKS_BEFORE_INCREMENT_ZEROS + 1;
        (num_of_zeros as u64) >= required_zeros
    }
}

#[derive(Eq, PartialEq, Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "BlockValidator")]
pub struct Block {
    inner: BlockInner,
    signature: Signature,
}

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
    pub const GENESIS: Block = Block {
        inner: BlockInner {
            sequence_no: 0,
            id: BlockId::GENESIS,
            prev_id: BlockId::GENESIS,
            prev_sha256: Sha256Hash(hex_to_bytes(
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            )),
            transactions: Vec::new(),
            miner: CoinAddress {
                pub_key: hex_to_bytes(
                    "3acee5b5591717dfbb2f773823e916d01e586de6695bb07b9665428cf88df30d",
                ),
            },
            nonce: 0,
        },
        signature: Signature {
            sig: hex_to_bytes(
                "e55aa435764adddf0eb9ea47f3836caf0a12d8d12c05e552a15d3cf5f1469c6adc90d16db5654983f1580caad1304ef67874a4e85682c91cb47aa1db82898701",
            ),
        },
    };
    pub fn id(&self) -> BlockId {
        self.inner.id
    }

    pub fn prev_id(&self) -> BlockId {
        self.inner.prev_id
    }

    pub fn seqno(&self) -> SequenceNo {
        self.inner.sequence_no
    }

    pub fn prev_sha256(&self) -> Sha256Hash {
        self.inner.prev_sha256
    }

    pub fn sha256(&self) -> Sha256Hash {
        let bytes = hashsig::encode(&self).unwrap();
        let array: [u8; 32] = Sha256::digest(&bytes).into();
        array.into()
    }
}

impl Display for Block {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Block: seqno: {}, id: {}, prev_id: {}, miner: {}, nouce: {}, ",
            self.inner.sequence_no,
            self.inner.id,
            self.inner.prev_id,
            self.inner.miner,
            self.inner.nonce
        )?;
        write!(f, "prev_sha256: ")?;
        fmt_hex(&self.inner.prev_sha256.0, f)?;
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
    pub sequence_no: u64,
    pub id: BlockId,
    pub prev_id: BlockId,
    pub prev_sha256: Sha256Hash,
    pub transactions: Vec<Transaction>,
    pub miner: CoinAddress,
}

#[derive(Debug)]
pub enum BlockValidationError {
    BlockSizeExceeded,
    MinerNotMatchSigner,
    InvalidNonce,
}

impl UnconfirmedBlock {
    pub fn with_nonce(
        self,
        signing_key: &mut SigningKey,
        nonce: u64,
    ) -> Result<Block, BlockValidationError> {
        let inner = BlockInner {
            sequence_no: self.sequence_no,
            id: self.id,
            prev_id: self.prev_id,
            prev_sha256: self.prev_sha256,
            transactions: self.transactions,
            miner: self.miner,
            nonce,
        };
        if inner.transactions.len() > BLOCK_SIZE_LIMIT {
            return Err(BlockValidationError::BlockSizeExceeded);
        }
        if inner.miner != signing_key.verifying_key().into() {
            return Err(BlockValidationError::MinerNotMatchSigner);
        }
        // panic risk: How can this serialization fail?
        let bytes = hashsig::encode(&inner).unwrap();
        let hash = Sha256::digest(&bytes);
        let num_of_zeros = num_of_zeros_in_sha256(hash.as_ref());
        let required_zeros = self.sequence_no / NUM_OF_BLOCKS_BEFORE_INCREMENT_ZEROS + 1;
        if (num_of_zeros as u64) < required_zeros {
            return Err(BlockValidationError::InvalidNonce);
        }

        let signature = signing_key.sign(&bytes).into();
        let block = Block { inner, signature };
        Ok(block)
    }

    // FIXME: should I keep this method here? Or move it to test util?
    pub fn find_nouce(&self) -> u64 {
        let mut bytes = hashsig::encode(self).unwrap();
        let len = bytes.len();
        bytes.extend_from_slice(&[0; 8]);
        let mut valid_nouce = 0;
        for nouce in 0_u64.. {
            bytes[len..].copy_from_slice(&nouce.to_le_bytes()[..]);
            let hash = Sha256::digest(&bytes);
            let num_of_zeros = num_of_zeros_in_sha256(hash.as_ref());
            if num_of_zeros >= 1 {
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
    use sha2::{Digest, Sha256};

    use crate::{UnconfirmedBlock, block::BlockId};

    #[test]
    fn generate_genesis_block() {
        let mut csprng = rand::rngs::OsRng;
        let mut signing_key = SigningKey::generate(&mut csprng);
        let miner = signing_key.verifying_key().into();
        let unconfirmed = UnconfirmedBlock {
            sequence_no: 0,
            id: BlockId::GENESIS,
            prev_id: BlockId::GENESIS,
            prev_sha256: std::convert::Into::<[u8; 32]>::into(Sha256::digest([])).into(),
            transactions: Vec::new(),
            miner,
        };

        let genesis_block = unconfirmed.try_confirm(&mut signing_key).unwrap();
        println!("Genesis block: {}", genesis_block);
        println!("PrevSha256: {}", genesis_block.inner.prev_sha256);
        println!("Miner: {}", genesis_block.inner.miner);
        println!("Signature: {}", genesis_block.signature);
    }
}
