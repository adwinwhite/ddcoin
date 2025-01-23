use ed25519_dalek::SigningKey;

use crate::{
    Block, CoinAddress, Config, Transaction, UnconfirmedBlock,
    block::{BlockId, SequenceNo},
    transaction::Signature,
    util::{Difficulty, Timestamp, hex_to_bytes},
};

pub fn config_with_random_alpn() -> Config {
    let mut alpn = vec![0; 16];
    for byte in alpn.iter_mut() {
        *byte = rand::random::<u8>();
    }
    Config::with_testing_alpn(&alpn)
}

pub fn create_transaction() -> Transaction {
    const RECEIVER_PUB_KEY: &str =
        "01a4b29a7fc6127080b9eb962ec4f18a3a61d5e011cc3fa821d5d1d1f30d0ddb";
    let receiver_pub_key = hex_to_bytes(RECEIVER_PUB_KEY);
    let amount = rand::random::<u64>();
    let fee = rand::random::<u64>();
    let receiver_pub_key = CoinAddress::from_bytes(&receiver_pub_key).unwrap();

    let mut csprng = rand::rngs::OsRng;
    let mut signing_key = SigningKey::generate(&mut csprng);
    Transaction::new(&mut signing_key, receiver_pub_key, amount, fee)
}

pub fn create_genesis_block(config: &Config) -> Block {
    Block::create_genesis(*config.genesis_difficulty(), config.genesis_timestamp())
}

// Precondition: chain is a valid blockchain.
pub fn create_block(chain: &[Block]) -> Block {
    let mut csprng = rand::rngs::OsRng;
    let mut signing_key = SigningKey::generate(&mut csprng);
    let miner: CoinAddress = signing_key.verifying_key().into();
    let txn1 = create_transaction();
    let txn2 = create_transaction();
    let prev_block = chain.last().unwrap();
    let unconfirmed = if (prev_block.seqno() + 1)
        % Config::INCOMPLETE_TESTING_CONFIG.difficulty_adjustment_period()
        == 0
    {
        // FIXME: consider chain that's too long.
        let prev_adjustment_time = chain[chain.len()
            - Config::INCOMPLETE_TESTING_CONFIG.difficulty_adjustment_period() as usize]
            .timestamp();

        UnconfirmedBlock::new(
            prev_block,
            miner.clone(),
            vec![txn1, txn2],
            Some(prev_adjustment_time),
            &Config::INCOMPLETE_TESTING_CONFIG,
        )
        .unwrap()
    } else {
        UnconfirmedBlock::new(
            prev_block,
            miner.clone(),
            vec![txn1, txn2],
            None,
            &Config::INCOMPLETE_TESTING_CONFIG,
        )
        .unwrap()
    };

    unconfirmed.try_confirm(&mut signing_key).unwrap()
}

pub fn create_invalid_transaction() -> Transaction {
    #[allow(dead_code)]
    struct TransactionInnerViewer {
        sender: CoinAddress,
        receiver: CoinAddress,
        amount: u64,
        fee: u64,
        timestamp: Timestamp,
    }
    #[allow(dead_code)]
    struct TransactionViewer {
        inner: TransactionInnerViewer,
        signature: Signature,
    }

    let original_txn = create_transaction();
    let mut txn_viewer: TransactionViewer = unsafe { std::mem::transmute(original_txn) };
    // Malicious action here.
    txn_viewer.inner.amount = txn_viewer.inner.amount.wrapping_add(1);
    unsafe { std::mem::transmute(txn_viewer) }
}
pub struct BlockInnerViewer {
    pub sequence_no: SequenceNo,
    pub difficulty: Difficulty,
    pub prev_id: BlockId,
    pub transactions: Vec<Transaction>,
    pub miner: CoinAddress,
    pub timestamp: Timestamp,
    pub nonce: u64,
}
pub struct BlockViewer {
    pub inner: BlockInnerViewer,
    pub signature: Signature,
}

// Precondition: chain is a valid blockchain.
pub fn create_invalid_block(chain: &[Block]) -> Block {
    let block = create_block(chain);
    let mut block_viewer: BlockViewer = unsafe { std::mem::transmute(block) };
    // Malicious action here.
    let mut csprng = rand::rngs::OsRng;
    let signing_key = SigningKey::generate(&mut csprng);
    let new_miner = signing_key.verifying_key().into();
    block_viewer.inner.miner = new_miner;
    unsafe { std::mem::transmute(block_viewer) }
}
