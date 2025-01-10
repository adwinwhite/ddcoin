#![feature(never_type)]
#![feature(array_try_from_fn)]

mod new_peer_watcher;
mod peer;
mod peerhub;
mod serdes;
mod transaction;

pub use peerhub::PeerHubActorMessage;
use tracing::error;
pub use transaction::{CoinAddress, Transaction};

use anyhow::Result;
use iroh::{
    Endpoint,
    discovery::{
        ConcurrentDiscovery, dns::DnsDiscovery, local_swarm_discovery::LocalSwarmDiscovery,
        pkarr::PkarrPublisher,
    },
};
use new_peer_watcher::{ALPN, IncomingConnectionListener, NewPeerStreamSubscriber};
use peerhub::PeerHubActor;
use ractor::{Actor, ActorRef};
use tokio::task::{JoinHandle, JoinSet};

pub struct Config {
    secret_key: iroh::SecretKey,
    pub discovery: Vec<Box<dyn iroh::discovery::Discovery>>,
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
        }
    }
}

impl Config {
    pub fn with_local_discovery() -> Self {
        let secret_key = iroh::SecretKey::generate(rand::rngs::OsRng);
        let discovery: Vec<Box<dyn iroh::discovery::Discovery>> = vec![
            // FIXME: avoid this panic case.
            Box::new(LocalSwarmDiscovery::new(secret_key.public()).unwrap()),
        ];
        Self {
            secret_key,
            discovery,
        }
    }

    pub async fn run(self) -> Result<(ActorRef<PeerHubActorMessage>, JoinHandle<()>)> {
        // Create an endpoint, it allows creating and accepting
        // connections in the iroh p2p world
        let discovery = ConcurrentDiscovery::from_services(self.discovery);
        let endpoint = Endpoint::builder()
            .secret_key(self.secret_key)
            .discovery(Box::new(discovery))
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await?;
        let discovery = endpoint.discovery().unwrap();
        let peer_stream = discovery.subscribe().unwrap();
        let (peer_hub_actor, _peer_hub_actor_handle) =
            Actor::spawn(None, PeerHubActor, endpoint.node_id()).await?;

        let mut tasks = JoinSet::new();
        let new_peer_stream_subscriber =
            NewPeerStreamSubscriber::new(peer_stream, peer_hub_actor.clone(), endpoint.clone());
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

    use anyhow::Result;
    use ed25519_dalek::SigningKey;
    use tokio::task::JoinSet;

    use crate::{CoinAddress, Config, PeerHubActorMessage, Transaction};

    fn hex_to_bytes(s: &str) -> Result<[u8; 32]> {
        let s = s.trim();
        if s.len() == 64 {
            let arr = std::array::try_from_fn(|i| {
                let sub = s.get(i * 2..i * 2 + 2).unwrap();
                u8::from_str_radix(sub, 16)
            })?;
            Ok(arr)
        } else {
            Err(anyhow::anyhow!("Invalid hex string length"))
        }
    }

    fn create_transaction() -> Transaction {
        const RECEIVER_PUB_KEY: &str =
            "01a4b29a7fc6127080b9eb962ec4f18a3a61d5e011cc3fa821d5d1d1f30d0ddb";
        let receiver_pub_key = hex_to_bytes(RECEIVER_PUB_KEY).unwrap();
        let amount = rand::random::<u64>();
        let fee = rand::random::<u64>();
        let receiver_pub_key = CoinAddress::from_bytes(&receiver_pub_key).unwrap();

        let mut csprng = rand::rngs::OsRng;
        let mut signing_key = SigningKey::generate(&mut csprng);
        Transaction::new(&mut signing_key, receiver_pub_key, amount, fee)
    }

    #[tokio::test]
    async fn connect_to_peers() {
        async fn query_peer_number() -> Result<usize> {
            let config = Config::with_local_discovery();
            let (peer_hub, _task_handle) = config.run().await?;
            // Wait for connection.
            tokio::time::sleep(Duration::from_secs(5)).await;

            let peers = ractor::call!(peer_hub, PeerHubActorMessage::QueryPeers)?;
            Ok(peers.len())
        }
        let mut tasks = JoinSet::new();
        tasks.spawn(query_peer_number());
        tasks.spawn(query_peer_number());
        tasks.spawn(query_peer_number());
        tasks.spawn(query_peer_number());
        while let Some(res) = tasks.join_next().await {
            let res = res.unwrap().unwrap();
            assert_eq!(res, 3);
        }
    }

    #[tokio::test]
    async fn propagate_transactions() {
        // Receive transactions before timeout.
        let mut tasks = JoinSet::new();
        let txn1 = create_transaction();
        let txn2 = create_transaction();
        {
            let txn1 = txn1.clone();
            let txn2 = txn2.clone();
            tasks.spawn(async move {
                let config = Config::with_local_discovery();
                let (peer_hub, _peer_hub_handle) = config.run().await?;
                // Wait for connection.
                tokio::time::sleep(Duration::from_secs(2)).await;

                // Send two transactions.
                peer_hub.cast(PeerHubActorMessage::NewTransaction(txn1))?;
                peer_hub.cast(PeerHubActorMessage::NewTransaction(txn2))?;

                // Wait for transaction propagtion.
                tokio::time::sleep(Duration::from_secs(2)).await;

                anyhow::Ok(())
            });
        }
        tasks.spawn(async move {
            let config = Config::with_local_discovery();
            let (peer_hub, _peer_hub_handle) = config.run().await?;
            // Wait for connection and transactions.
            tokio::time::sleep(Duration::from_secs(4)).await;

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
}
