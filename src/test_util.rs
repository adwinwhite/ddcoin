use ed25519_dalek::SigningKey;

use crate::{
    Block, CoinAddress, Transaction, UnconfirmedBlock,
    block::{BlockId, SequenceNo},
    transaction::Signature,
    util::{Timestamp, hex_to_bytes},
};

pub fn random_alpn() -> Vec<u8> {
    let mut alpn = vec![0; 16];
    for byte in alpn.iter_mut() {
        *byte = rand::random::<u8>();
    }
    alpn
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

pub fn create_block(prev_block: &Block) -> Block {
    let mut csprng = rand::rngs::OsRng;
    let mut signing_key = SigningKey::generate(&mut csprng);
    let miner: CoinAddress = signing_key.verifying_key().into();
    let txn1 = create_transaction();
    let txn2 = create_transaction();
    let unconfirmed = UnconfirmedBlock::new(prev_block, miner.clone(), vec![txn1, txn2]);
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

pub fn create_invalid_block(prev_block: &Block) -> Block {
    let block = create_block(prev_block);
    let mut block_viewer: BlockViewer = unsafe { std::mem::transmute(block) };
    // Malicious action here.
    let mut csprng = rand::rngs::OsRng;
    let signing_key = SigningKey::generate(&mut csprng);
    let new_miner = signing_key.verifying_key().into();
    block_viewer.inner.miner = new_miner;
    unsafe { std::mem::transmute(block_viewer) }
}
