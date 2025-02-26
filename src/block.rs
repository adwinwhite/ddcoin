use std::fmt::{Display, Formatter};

use anyhow::{Result, bail};
use ed25519_dalek::{SigningKey, Verifier, VerifyingKey, ed25519::signature::SignerMut};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    CoinAddress, Config, Transaction,
    serdes::hashsig,
    transaction::{Cash, Signature},
    util::{Difficulty, Sha256Hash, Timestamp, TimestampExt, hex_to_bytes},
};

pub type BlockId = Sha256Hash;

pub type SequenceNo = u64;

// FIXME: fixate the serialize order and layout.
#[derive(Eq, PartialEq, Clone, Debug, Serialize, Deserialize)]
struct BlockInner {
    seqno: SequenceNo,
    difficulty: Difficulty,
    prev_id: BlockId,
    transactions: Vec<Transaction>,
    reward: Cash,
    miner: CoinAddress,
    timestamp: Timestamp,
    nonce: u64,
}

impl BlockInner {
    fn verify_nonce(&self) -> bool {
        // panic risk: How can this serialization fail?
        let bytes = hashsig::encode(&self).unwrap();
        let hash = Sha256::digest(&bytes);
        self.difficulty.is_met(*hash.as_ref())
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
// 3. does previous block's seqno match? Do we need this field even?
// 4. is difficulty valid?
// 5. are transactions' UTOs valid?
// 6. is reward valid? subsidy + fees. this is local. well it depends on config thus global.
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
    pub(crate) fn create_genesis(
        difficulty: Difficulty,
        timestamp: Timestamp,
        subsidy: u64,
    ) -> Self {
        let secret_key_bytes =
            hex_to_bytes("01a4b29a7fc6127080b9eb962ec4f18a3a61d5e011cc3fa821d5d1d1f30d0ddb");

        let mut signing_key: SigningKey = SigningKey::from_bytes(&secret_key_bytes);
        let miner: CoinAddress = signing_key.verifying_key().into();
        let reward = Cash::genesis_reward(subsidy, miner.clone());
        let unconfirmed = UnconfirmedBlock {
            seqno: 0,
            difficulty,
            prev_id: Sha256Hash([0; 32]),
            transactions: Vec::new(),
            reward,
            miner,
            timestamp,
        };

        unconfirmed.try_confirm(&mut signing_key).unwrap()
    }

    pub fn miner(&self) -> &CoinAddress {
        &self.inner.miner
    }

    pub fn reward(&self) -> &Cash {
        &self.inner.reward
    }

    pub fn id(&self) -> BlockId {
        self.sha256()
    }

    pub fn prev_id(&self) -> BlockId {
        self.inner.prev_id
    }

    pub fn seqno(&self) -> SequenceNo {
        self.inner.seqno
    }

    pub fn difficulty(&self) -> Difficulty {
        self.inner.difficulty
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
            "Block: seqno: {}, difficulty: {}, id: {}, prev_id: {}, miner: {}, timestamp: {}, nouce: {}, reward: {}, ",
            self.inner.seqno,
            self.inner.difficulty,
            self.id(),
            self.inner.prev_id,
            self.inner.miner,
            self.inner.timestamp,
            self.inner.nonce,
            self.inner.reward,
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
    seqno: SequenceNo,
    difficulty: Difficulty,
    prev_id: BlockId,
    transactions: Vec<Transaction>,
    reward: Cash,
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
    // TODO: reconsider the config argument.
    pub fn new(
        prev: &Block,
        miner: CoinAddress,
        txns: Vec<Transaction>,
        prev_adjustment_time: Option<Timestamp>,
        config: &Config,
    ) -> Result<Self> {
        let timestamp = Timestamp::now();
        let seqno = prev.seqno() + 1;
        let difficulty = if seqno % config.difficulty_adjustment_period() == 0 {
            let Some(prev_adjustment_time) = prev_adjustment_time else {
                bail!("Previous adjustment timestamp is required for difficulty adjustment")
            };
            let time_span_actual = timestamp - prev_adjustment_time;
            prev.difficulty()
                .adjust_with_actual_span(time_span_actual, config)
        } else {
            prev.difficulty()
        };
        let subsidy =
            config.initial_block_subsidy() >> (seqno / config.block_subsidy_half_period());
        let fees = txns.iter().map(|txn| txn.fee()).sum::<u64>();
        let reward = Cash::new(subsidy + fees, miner.clone());
        Ok(Self {
            seqno,
            difficulty,
            prev_id: prev.id(),
            transactions: txns,
            reward,
            miner,
            timestamp,
        })
    }

    pub fn with_nonce(
        self,
        signing_key: &mut SigningKey,
        nonce: u64,
    ) -> Result<Block, BlockValidationError> {
        let inner = BlockInner {
            seqno: self.seqno,
            difficulty: self.difficulty,
            prev_id: self.prev_id,
            transactions: self.transactions,
            reward: self.reward,
            miner: self.miner,
            timestamp: self.timestamp,
            nonce,
        };
        // TODO: do we really need this check here?
        // if inner.transactions.len() > Block::BLOCK_TXN_LIMIT {
        // return Err(BlockValidationError::BlockSizeExceeded);
        // }
        if inner.miner != signing_key.verifying_key().into() {
            return Err(BlockValidationError::MinerNotMatchSigner);
        }
        // panic risk: How can this serialization fail?
        let bytes = hashsig::encode(&inner).unwrap();
        let hash = Sha256::digest(&bytes);
        if !inner.difficulty.is_met(*hash.as_ref()) {
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
            if self.difficulty.is_met(*hash.as_ref()) {
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
