use std::collections::HashSet;

use anyhow::{Context, Result};
use iroh::{Endpoint, NodeId, discovery::DiscoveryItem};
use ractor::ActorRef;
use tokio::{io::AsyncWriteExt, select, task::JoinSet};
use tokio_stream::StreamExt;
use tracing::{info, warn};

use crate::{peer::Peer, peerhub::PeerHubActorMessage};

pub const ALPN: &[u8] = b"ddcoin/0.1";

type BoxStream<T> = futures_lite::stream::Boxed<T>;

pub struct NewPeerStreamSubscriber {
    stream: BoxStream<DiscoveryItem>,
    peer_hub: ActorRef<PeerHubActorMessage>,
    endpoint: Endpoint,
}

impl NewPeerStreamSubscriber {
    pub fn new(
        stream: BoxStream<DiscoveryItem>,
        peer_hub: ActorRef<PeerHubActorMessage>,
        endpoint: Endpoint,
    ) -> Self {
        Self {
            stream,
            peer_hub,
            endpoint,
        }
    }

    pub async fn run(mut self) -> Result<!> {
        // TODO: retry these when needed.
        let mut known_ids = HashSet::new();
        let mut tasks = JoinSet::new();
        loop {
            select! {
                item = self.stream.next() => {
                    let item = item.context("New peer stream ended")?;
                    let node_id = item.node_addr.node_id;
                    if known_ids.contains(&node_id) {
                        continue;
                    } else {
                        known_ids.insert(node_id);
                    }
                    info!("Found new peer: {:?}", item);

                    let should_connect =
                        ractor::call!(self.peer_hub, PeerHubActorMessage::ShouldConnect, node_id)?;
                    if !should_connect {
                        continue;
                    }

                    let conn = match self.endpoint.connect(item.node_addr, ALPN).await {
                        Ok(conn) => conn,
                        Err(e) => {
                            warn!("Failed to connect to remote: {:?}", e);
                            continue;
                        }
                    };
                    let (mut tx, rx) = conn.open_bi().await?;

                    tx.write_all(self.endpoint.node_id().as_bytes()).await?;
                    tx.flush().await?;

                    let not_exist =
                        ractor::call!(self.peer_hub, PeerHubActorMessage::NewPeer, node_id, tx)?;
                    if not_exist {
                        let peer = Peer::new(rx, self.peer_hub.clone(), node_id);
                        tasks.spawn(peer.run());
                    }
                },
                Some(res) = tasks.join_next() => {
                    // TODO: deal with this better. Remember my result is `Ok(Result<!>)`.
                    warn!("Peer task finished: {:?}", res);
                }
            }
        }
    }
}

pub struct IncomingConnectionListener {
    endpoint: Endpoint,
    peer_hub: ActorRef<PeerHubActorMessage>,
}

impl IncomingConnectionListener {
    pub fn new(endpoint: Endpoint, peer_hub: ActorRef<PeerHubActorMessage>) -> Self {
        Self { endpoint, peer_hub }
    }

    pub async fn run(self) -> Result<!> {
        let mut tasks = JoinSet::new();
        loop {
            select! {
                pending_conn = self.endpoint.accept() => {
                    let pending_conn = pending_conn.context("Endpoint cannot listen anymore")?;
                    let Ok(connecting) = pending_conn.accept() else {
                        warn!("Incoming connection has error");
                        continue;
                    };
                    let conn = match connecting.await {
                        Ok(conn) => conn,
                        Err(e) => {
                            warn!("Incoming connection has error: {:?}", e);
                            continue;
                        }
                    };
                    let (tx, mut rx) = conn.accept_bi().await?;
                    let mut node_id_buf = [0u8; 32];
                    rx.read_exact(&mut node_id_buf).await?;
                    let node_id = NodeId::from_bytes(&node_id_buf)?;

                    let should_connect =
                        ractor::call!(self.peer_hub, PeerHubActorMessage::ShouldConnect, node_id)?;
                    if !should_connect {
                        continue;
                    }

                    let not_exist =
                        ractor::call!(self.peer_hub, PeerHubActorMessage::NewPeer, node_id, tx)?;
                    if not_exist {
                        let peer = Peer::new(rx, self.peer_hub.clone(), node_id);
                        tasks.spawn(peer.run());
                    }
                },
                Some(res) = tasks.join_next() => {
                    // TODO: deal with this better. Remember my result is `Ok(Result<!>)`.
                    warn!("Peer task finished: {:?}", res);
                }
            }
        }
    }
}
