#![feature(never_type)]
#![feature(array_try_from_fn)]
#![feature(hash_extract_if)]

mod block;
mod new_peer_watcher;
mod peer;
mod peerhub;
mod serdes;
mod transaction;
mod util;
// TODO: gate behind a feature.
pub mod test_util;

use tracing::error;

pub use block::{Block, UnconfirmedBlock};
pub use peerhub::PeerHubActorMessage;
pub use transaction::{CoinAddress, Transaction};

use anyhow::Result;
use iroh::{
    Endpoint,
    discovery::{
        ConcurrentDiscovery, dns::DnsDiscovery, local_swarm_discovery::LocalSwarmDiscovery,
        pkarr::PkarrPublisher,
    },
};
use new_peer_watcher::{IncomingConnectionListener, NewPeerStreamSubscriber};
use peerhub::PeerHubActor;
use ractor::{Actor, ActorRef};
use tokio::task::{JoinHandle, JoinSet};

pub struct Config {
    secret_key: iroh::SecretKey,
    pub discovery: Vec<Box<dyn iroh::discovery::Discovery>>,
    pub alpn: Vec<u8>,
}

impl Default for Config {
    fn default() -> Self {
        let secret_key = iroh::SecretKey::generate(rand::rngs::OsRng);
        let discovery: Vec<Box<dyn iroh::discovery::Discovery>> = vec![
            Box::new(PkarrPublisher::n0_dns(secret_key.clone())),
            Box::new(DnsDiscovery::n0_dns()),
            Box::new(LocalSwarmDiscovery::new(secret_key.public()).unwrap()),
        ];
        Self {
            secret_key,
            discovery,
            alpn: b"ddcoin/1.0".to_vec(),
        }
    }
}

impl Config {
    pub fn with_local_discovery(alpn: &[u8]) -> Self {
        let secret_key = iroh::SecretKey::generate(rand::rngs::OsRng);
        let discovery: Vec<Box<dyn iroh::discovery::Discovery>> = vec![
            // FIXME: avoid this panic case.
            Box::new(LocalSwarmDiscovery::new(secret_key.public()).unwrap()),
        ];
        Self {
            secret_key,
            discovery,
            alpn: alpn.to_vec(),
        }
    }

    pub async fn run(self) -> Result<(ActorRef<PeerHubActorMessage>, JoinHandle<()>)> {
        // Create an endpoint, it allows creating and accepting
        // connections in the iroh p2p world
        let discovery = ConcurrentDiscovery::from_services(self.discovery);
        let endpoint = Endpoint::builder()
            .secret_key(self.secret_key)
            .discovery(Box::new(discovery))
            .alpns(vec![self.alpn.clone()])
            .bind()
            .await?;
        let discovery = endpoint.discovery().unwrap();
        let peer_stream = discovery.subscribe().unwrap();
        let (peer_hub_actor, _peer_hub_actor_handle) =
            Actor::spawn(None, PeerHubActor, endpoint.node_id()).await?;

        let mut tasks = JoinSet::new();
        let new_peer_stream_subscriber = NewPeerStreamSubscriber::new(
            peer_stream,
            peer_hub_actor.clone(),
            endpoint.clone(),
            &self.alpn,
        );
        tasks.spawn(new_peer_stream_subscriber.run());
        let incoming_connection_listener =
            IncomingConnectionListener::new(endpoint, peer_hub_actor.clone());
        tasks.spawn(incoming_connection_listener.run());

        let handle = tokio::spawn(async move {
            while let Some(res) = tasks.join_next().await {
                let Ok(Err(e)) = res else {
                    error!("Peer listener task has error in joining");
                    continue;
                };
                eprintln!("Peer listener task failed: {:?}", e);
            }
        });
        Ok((peer_hub_actor, handle))
    }
}

pub async fn run() -> Result<(ActorRef<PeerHubActorMessage>, JoinHandle<()>)> {
    Config::default().run().await
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::test_util::{
        create_block, create_invalid_block, create_invalid_transaction, create_transaction,
        random_alpn,
    };
    use anyhow::Result;
    use tokio::task::JoinSet;

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
        let prev_block = Block::GENESIS;
        let bad_block = create_invalid_block(&prev_block);
        let bytes = transport::encode(&bad_block).unwrap();
        let received_block: Result<Block> = transport::decode(&bytes);
        assert!(received_block.is_err());
    }

    #[tokio::test]
    async fn connect_to_peers() {
        async fn query_peer_number(alpn: Vec<u8>) -> Result<usize> {
            let config = Config::with_local_discovery(&alpn);
            let (peer_hub, _task_handle) = config.run().await?;
            // Wait for connection.
            tokio::time::sleep(Duration::from_secs(5)).await;

            let peers = ractor::call!(peer_hub, PeerHubActorMessage::QueryPeers)?;
            Ok(peers.len())
        }
        let mut tasks = JoinSet::new();
        let alpn = random_alpn();
        tasks.spawn(query_peer_number(alpn.clone()));
        tasks.spawn(query_peer_number(alpn.clone()));
        tasks.spawn(query_peer_number(alpn.clone()));
        tasks.spawn(query_peer_number(alpn.clone()));
        while let Some(res) = tasks.join_next().await {
            let res = res.unwrap().unwrap();
            assert_eq!(res, 3);
        }
    }

    #[tokio::test]
    async fn propagate_transactions() {
        // Receive transactions before timeout.
        let mut tasks = JoinSet::new();
        let alpn = random_alpn();
        let txn1 = create_transaction();
        let txn2 = create_transaction();
        {
            let txn1 = txn1.clone();
            let txn2 = txn2.clone();
            let alpn = alpn.clone();
            tasks.spawn(async move {
                let config = Config::with_local_discovery(&alpn);
                let (peer_hub, _peer_hub_handle) = config.run().await?;
                // Wait for connection.
                tokio::time::sleep(Duration::from_secs(2)).await;

                // Send two transactions.
                peer_hub.cast(PeerHubActorMessage::NewTransaction(txn1))?;
                peer_hub.cast(PeerHubActorMessage::NewTransaction(txn2))?;

                // Wait for transaction propagtion.
                tokio::time::sleep(Duration::from_secs(3)).await;

                anyhow::Ok(())
            });
        }
        tasks.spawn(async move {
            let config = Config::with_local_discovery(&alpn);
            let (peer_hub, _peer_hub_handle) = config.run().await?;
            // Wait for connection and transactions.
            tokio::time::sleep(Duration::from_secs(5)).await;

            // Check if transactions are propagated.
            let recv_txn1 =
                ractor::call!(peer_hub, PeerHubActorMessage::QueryTransaction, txn1.id())?;
            assert_eq!(recv_txn1, Some(txn1));
            let recv_txn2 =
                ractor::call!(peer_hub, PeerHubActorMessage::QueryTransaction, txn2.id())?;
            assert_eq!(recv_txn2, Some(txn2));

            anyhow::Ok(())
        });
        while let Some(res) = tasks.join_next().await {
            let res = res.unwrap();
            assert!(res.is_ok());
        }
    }

    #[tokio::test]
    async fn propagate_blocks() {
        // Receive transactions before timeout.
        let mut tasks = JoinSet::new();
        let alpn = random_alpn();
        let mut blocks =
            Vec::with_capacity((Block::NUM_OF_BLOCKS_BEFORE_INCREMENT_ZEROS as usize) + 2);
        for i in 0..blocks.capacity() {
            let prev_block = if i == 0 {
                &Block::GENESIS
            } else {
                &blocks[i - 1]
            };
            let block = create_block(prev_block);
            blocks.push(block);
        }
        {
            let blocks = blocks.clone();
            let alpn = alpn.clone();
            tasks.spawn(async move {
                let config = Config::with_local_discovery(&alpn);
                let (peer_hub, _peer_hub_handle) = config.run().await?;
                // Wait for connection.
                tokio::time::sleep(Duration::from_secs(2)).await;

                // Send blocks.
                for block in blocks {
                    peer_hub.cast(PeerHubActorMessage::NewBlock(None, block))?;
                }

                // Wait for transaction propagtion.
                tokio::time::sleep(Duration::from_secs(3)).await;

                anyhow::Ok(())
            });
        }
        tasks.spawn(async move {
            let config = Config::with_local_discovery(&alpn);
            let (peer_hub, _peer_hub_handle) = config.run().await?;
            // Wait for connection and transactions.
            tokio::time::sleep(Duration::from_secs(5)).await;

            // Check if blocks are propagated.
            for block in blocks {
                let recv_block =
                    ractor::call!(peer_hub, PeerHubActorMessage::QueryBlock, block.id())?;
                assert_eq!(recv_block, Some(block));
            }

            anyhow::Ok(())
        });
        while let Some(res) = tasks.join_next().await {
            let res = res.unwrap();
            assert!(res.is_ok());
        }
    }

    #[tokio::test]
    async fn leading_block_with_forks() {
        // Receive transactions before timeout.
        let mut tasks = JoinSet::new();
        let alpn = random_alpn();
        let mut old_chain =
            Vec::with_capacity((Block::NUM_OF_BLOCKS_BEFORE_INCREMENT_ZEROS as usize) + 2);
        for i in 0..old_chain.capacity() {
            let prev_block = if i == 0 {
                &Block::GENESIS
            } else {
                &old_chain[i - 1]
            };
            let block = create_block(prev_block);
            old_chain.push(block);
        }
        let mut fork1 = old_chain[..old_chain.len() - 2].to_vec();
        for _ in 0..5 {
            let prev_block = fork1.last().unwrap();
            let block = create_block(prev_block);
            fork1.push(block);
        }
        let correct_leading_block = fork1.last().unwrap().id();
        let mut fork2 = old_chain[..old_chain.len() - 2].to_vec();
        for _ in 0..3 {
            let prev_block = fork2.last().unwrap();
            let block = create_block(prev_block);
            fork2.push(block);
        }
        {
            let alpn = alpn.clone();
            tasks.spawn(async move {
                let config = Config::with_local_discovery(&alpn);
                let (peer_hub, _peer_hub_handle) = config.run().await?;
                // Wait for connection.
                tokio::time::sleep(Duration::from_secs(2)).await;

                // Send old_chain.
                for block in old_chain {
                    peer_hub.cast(PeerHubActorMessage::NewBlock(None, block))?;
                }
                for block in fork1 {
                    peer_hub.cast(PeerHubActorMessage::NewBlock(None, block))?;
                }
                for block in fork2 {
                    peer_hub.cast(PeerHubActorMessage::NewBlock(None, block))?;
                }

                // Wait for transaction propagtion.
                tokio::time::sleep(Duration::from_secs(3)).await;

                anyhow::Ok(())
            });
        }
        tasks.spawn(async move {
            let config = Config::with_local_discovery(&alpn);
            let (peer_hub, _peer_hub_handle) = config.run().await?;
            // Wait for connection and transactions.
            tokio::time::sleep(Duration::from_secs(5)).await;

            // Check if the leading block is correct.
            let leading_id = ractor::call!(peer_hub, PeerHubActorMessage::QueryLeadingBlock)?;
            assert_eq!(leading_id, correct_leading_block);

            anyhow::Ok(())
        });
        while let Some(res) = tasks.join_next().await {
            let res = res.unwrap();
            assert!(res.is_ok());
        }
    }
}
