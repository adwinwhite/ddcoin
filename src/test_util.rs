use anyhow::Result;
use ed25519_dalek::SigningKey;
use ractor::{ActorRef, concurrency::Duration};

use crate::{
    Block, CoinAddress, Config, PeerHubActorMessage, Transaction, UnconfirmedBlock,
    block::{BlockId, SequenceNo},
    config::HubHandle,
    hub_helper::HubHelper,
    transaction::{Cash, Signature},
    util::{Difficulty, Timestamp, hex_to_bytes},
};

pub fn random_address() -> CoinAddress {
    let mut csprng = rand::rngs::OsRng;
    let signing_key = SigningKey::generate(&mut csprng);
    signing_key.verifying_key().into()
}

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
    let receiver_pub_key = CoinAddress::from_bytes(&receiver_pub_key);

    let mut csprng = rand::rngs::OsRng;
    let mut signing_key = SigningKey::generate(&mut csprng);
    let sender_address = signing_key.verifying_key().into();
    let input = Cash::new(rand::random::<u64>(), sender_address);
    let output = Cash::new(input.amount() - 10, receiver_pub_key.clone());
    Transaction::new(&mut signing_key, &[input], &[output])
        .expect("create_transaction util function is not sound")
}

pub fn create_empty_chain(config: &Config, len: usize) -> Vec<Block> {
    let mut blocks = Vec::with_capacity(len);
    for i in 0..len {
        let block = if i == 0 {
            create_genesis_block(config)
        } else {
            create_empty_block(&blocks)
        };
        blocks.push(block);
    }
    blocks
}

pub fn create_genesis_block(config: &Config) -> Block {
    Block::create_genesis(
        *config.genesis_difficulty(),
        config.genesis_timestamp(),
        config.initial_block_subsidy(),
    )
}

// Precondition: chain is a valid blockchain.
pub fn create_empty_block(chain: &[Block]) -> Block {
    let mut csprng = rand::rngs::OsRng;
    let mut signing_key = SigningKey::generate(&mut csprng);
    let miner: CoinAddress = signing_key.verifying_key().into();
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
            Vec::new(),
            Some(prev_adjustment_time),
            &Config::INCOMPLETE_TESTING_CONFIG,
        )
        .unwrap()
    } else {
        UnconfirmedBlock::new(
            prev_block,
            miner.clone(),
            Vec::new(),
            None,
            &Config::INCOMPLETE_TESTING_CONFIG,
        )
        .unwrap()
    };

    unconfirmed.try_confirm(&mut signing_key).unwrap()
}

pub const EVIL_GUY_ADDRESS: CoinAddress = CoinAddress::from_bytes(&[0; 32]);

pub fn create_invalid_transaction() -> Transaction {
    #[allow(dead_code)]
    struct TransactionInnerViewer {
        inputs: Vec<Cash>,
        outputs: Vec<Cash>,
    }
    #[allow(dead_code)]
    struct TransactionViewer {
        inner: TransactionInnerViewer,
        signature: Signature,
    }

    let original_txn = create_transaction();
    let mut txn_viewer: TransactionViewer = unsafe { std::mem::transmute(original_txn) };
    // Malicious action here.
    let output = Cash::new(
        txn_viewer.inner.inputs.iter().map(|c| c.amount()).sum(),
        EVIL_GUY_ADDRESS,
    );
    txn_viewer.inner.outputs = vec![output];
    unsafe { std::mem::transmute(txn_viewer) }
}
pub struct BlockInnerViewer {
    pub sequence_no: SequenceNo,
    pub difficulty: Difficulty,
    pub prev_id: BlockId,
    pub transactions: Vec<Transaction>,
    pub reward: Cash,
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
    let block = create_empty_block(chain);
    let mut block_viewer: BlockViewer = unsafe { std::mem::transmute(block) };
    // Malicious action here.
    let mut csprng = rand::rngs::OsRng;
    let signing_key = SigningKey::generate(&mut csprng);
    let new_miner = signing_key.verifying_key().into();
    block_viewer.inner.miner = new_miner;
    unsafe { std::mem::transmute(block_viewer) }
}

pub async fn fully_connected(
    config: Config,
    n: usize,
) -> Result<Box<[(ActorRef<PeerHubActorMessage>, HubHandle)]>> {
    let nodes = {
        let futs = (0..n)
            .map(|_| config.clone().run_with_local_discovery())
            .collect::<Vec<_>>();
        let mut res = Vec::with_capacity(n);
        for fut in futs {
            let (node, handle) = fut.await?;
            res.push((node, handle.into()));
        }
        res
    };
    for (hub, _) in &nodes {
        loop {
            let peers = hub.peers().await?;
            if peers.len() == n - 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    Ok(nodes.into())
}
