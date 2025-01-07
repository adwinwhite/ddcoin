#![feature(never_type)]

mod new_peer_watcher;
mod peer;
mod peerhub;
mod serdes;
mod transaction;

pub use peerhub::PeerHubActorMessage;
pub use transaction::{CoinAddress, Transaction};

use anyhow::Result;
use iroh::{
    discovery::{
        dns::DnsDiscovery, local_swarm_discovery::LocalSwarmDiscovery, pkarr::PkarrPublisher,
        ConcurrentDiscovery,
    },
    Endpoint,
};
use new_peer_watcher::{IncomingConnectionListener, NewPeerStreamSubscriber, ALPN};
use peerhub::PeerHubActor;
use ractor::{concurrency::JoinHandle, Actor, ActorRef};
use tokio::task::JoinSet;

pub async fn run() -> Result<(ActorRef<PeerHubActorMessage>, JoinHandle<()>)> {
    // Create an endpoint, it allows creating and accepting
    // connections in the iroh p2p world
    let secret_key = iroh::SecretKey::generate(rand::rngs::OsRng);
    let discovery = ConcurrentDiscovery::from_services(vec![
        Box::new(PkarrPublisher::n0_dns(secret_key.clone())),
        Box::new(DnsDiscovery::n0_dns()),
        Box::new(LocalSwarmDiscovery::new(secret_key.public())?),
    ]);
    let endpoint = Endpoint::builder()
        .secret_key(secret_key)
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
            if let Err(e) = res {
                println!("Task failed: {:?}", e);
            }
        }
    });
    // FIXME: not really okay.
    Ok((peer_hub_actor, handle))
}
