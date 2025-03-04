#![feature(never_type)]
#![feature(array_try_from_fn)]
#![feature(hash_extract_if)]

mod block;
mod config;
pub mod hub_helper;
mod new_peer_watcher;
mod peer;
mod peerhub;
mod serdes;
mod transaction;
mod util;

#[cfg(feature = "test_util")]
pub mod test_util;

pub use block::{Block, UnconfirmedBlock};
pub use config::Config;
pub use peerhub::PeerHubActorMessage;
pub use transaction::{CoinAddress, Transaction};

use anyhow::Result;
use ractor::ActorRef;
use tokio::task::JoinHandle;

pub async fn run() -> Result<(ActorRef<PeerHubActorMessage>, JoinHandle<()>)> {
    Config::default().run().await
}

// TODO: add more tests against validation.
#[cfg(feature = "test_util")]
#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::hub_helper::HubHelper;
    use crate::test_util::{
        config_with_random_alpn, create_empty_block, create_empty_chain, create_invalid_block,
        create_invalid_transaction, create_transaction, fully_connected,
    };
    use anyhow::Result;

    use crate::{Block, Config, PeerHubActorMessage, Transaction, serdes::transport};

    #[test]
    fn invalid_transaction() {
        let bad_txn = create_invalid_transaction();
        let bytes = transport::encode(&bad_txn).unwrap();
        let received_txn: Result<Transaction> = transport::decode(&bytes);
        assert!(received_txn.is_err());
    }

    #[test]
    fn invalid_block() {
        let config = Config::INCOMPLETE_TESTING_CONFIG;
        let blocks = create_empty_chain(&config, 1);
        let bad_block = create_invalid_block(&blocks);
        let bytes = transport::encode(&bad_block).unwrap();
        let received_block: Result<Block> = transport::decode(&bytes);
        assert!(received_block.is_err());
    }

    #[tokio::test]
    async fn connect_to_peers() {
        let config = config_with_random_alpn();
        tokio::time::timeout(Duration::from_secs(5), async {
            fully_connected(config, 4)
                .await
                .expect("setup dense nodes successfully");
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn propagate_transactions() {
        let config = config_with_random_alpn();
        tokio::time::timeout(Duration::from_secs(6), async {
            let nodes = fully_connected(config, 2)
                .await
                .expect("setup dense nodes successfully");

            let txn1 = create_transaction();
            let txn2 = create_transaction();
            // Send two transactions.
            nodes[0]
                .0
                .cast(PeerHubActorMessage::NewTransaction(txn1.clone()))
                .unwrap();
            nodes[0]
                .0
                .cast(PeerHubActorMessage::NewTransaction(txn2.clone()))
                .unwrap();

            let recv_txn1 = loop {
                let Some(txn) = nodes[1].0.get_transaction(txn1.id()).await.unwrap() else {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                };
                break txn;
            };
            assert_eq!(recv_txn1, txn1);
            let recv_txn2 = loop {
                let Some(txn) = nodes[1].0.get_transaction(txn2.id()).await.unwrap() else {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                };
                break txn;
            };
            assert_eq!(recv_txn2, txn2);
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn propagate_blocks() {
        let config = config_with_random_alpn();
        let blocks = create_empty_chain(
            &config,
            (Config::INCOMPLETE_TESTING_CONFIG.difficulty_adjustment_period() as usize) + 2,
        );
        tokio::time::timeout(Duration::from_secs(6), async {
            let nodes = fully_connected(config, 2)
                .await
                .expect("setup dense nodes successfully");

            // Send blocks.
            for block in blocks.clone() {
                nodes[0]
                    .0
                    .cast(PeerHubActorMessage::NewBlock(None, block))
                    .unwrap();
            }

            // Check if blocks are propagated.
            for block in blocks {
                let recv_block = loop {
                    let Some(b) = nodes[1].0.get_block(block.id()).await.unwrap() else {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    };
                    break b;
                };
                assert_eq!(recv_block, block);
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn leading_block_with_forks() {
        let config = config_with_random_alpn();
        let old_chain = create_empty_chain(
            &config,
            (Config::INCOMPLETE_TESTING_CONFIG.difficulty_adjustment_period() as usize) + 2,
        );
        let mut fork1 = old_chain[..old_chain.len() - 4].to_vec();
        for _ in 0..6 {
            let block = create_empty_block(&fork1);
            fork1.push(block);
        }
        let correct_leading_block = fork1.last().unwrap().id();
        let mut fork2 = old_chain[..old_chain.len() - 4].to_vec();
        for _ in 0..5 {
            let block = create_empty_block(&fork2);
            fork2.push(block);
        }
        tokio::time::timeout(Duration::from_secs(6), async {
            let nodes = fully_connected(config, 2)
                .await
                .expect("setup dense nodes successfully");

            // Send old_chain.
            for block in old_chain {
                nodes[0]
                    .0
                    .cast(PeerHubActorMessage::NewBlock(None, block))
                    .unwrap();
            }
            for block in fork1 {
                nodes[0]
                    .0
                    .cast(PeerHubActorMessage::NewBlock(None, block))
                    .unwrap();
            }
            for block in fork2 {
                nodes[0]
                    .0
                    .cast(PeerHubActorMessage::NewBlock(None, block))
                    .unwrap();
            }

            // Wait for blocks.
            tokio::time::sleep(Duration::from_secs(3)).await;
            // Check if the leading block is correct.
            let leading_id = nodes[1].0.leading_block().await.unwrap().id();
            assert_eq!(leading_id, correct_leading_block);
        })
        .await
        .unwrap();
    }
}
