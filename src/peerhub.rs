use std::collections::{hash_map::Entry, HashMap};

use anyhow::Result;
use iroh::{endpoint::SendStream, NodeId};
use ractor::{Actor, RpcReplyPort};
use tokio::io::AsyncWriteExt;
use tracing::info;

use crate::{
    peer::{BroadcastMessage, PeerMessage},
    transaction::{Transaction, TransactionId},
};

// TODO: make this configurable.
const PEER_SIZE_LIMIT: usize = 3;

pub struct PeerHubActor;

impl PeerHubActor {
    async fn broadcast(
        state: &mut PeerHubActorState,
        node_id: NodeId,
        msg: BroadcastMessage,
    ) -> Result<()> {
        // Only broadcast if we don't know it before.
        match msg {
            BroadcastMessage::AnnounceTransaction(txn_id) => {
                if state.mempool.contains_key(&txn_id) {
                    info!("Transaction {} already in mempool", txn_id);
                    return Ok(());
                }
                let peer_msg = PeerMessage::Broadcast(msg);
                let encoded = crate::serdes::encode(&peer_msg)?;
                let len = encoded.len() as u32;
                for (id, send_stream) in &mut state.peers {
                    if *id == node_id {
                        continue;
                    }
                    send_stream.write_all(&len.to_be_bytes()).await?;
                    send_stream.write_all(&encoded).await?;
                    send_stream.flush().await?;
                }
            }
        }
        Ok(())
    }
}

pub struct PeerHubActorState {
    peers: HashMap<NodeId, SendStream>,
    mempool: HashMap<TransactionId, Transaction>,
    local_node_id: NodeId,
}

impl PeerHubActorState {
    pub fn new(local_node_id: NodeId) -> Self {
        Self {
            peers: HashMap::new(),
            mempool: HashMap::new(),
            local_node_id,
        }
    }
}

// FIXME: optimize enum size here.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum PeerHubActorMessage {
    ShouldConnect(NodeId, RpcReplyPort<bool>),
    NewPeer(NodeId, SendStream, RpcReplyPort<bool>),
    Broadcast(NodeId, BroadcastMessage),
    NewTransaction(Transaction),
}

impl Actor for PeerHubActor {
    type Msg = PeerHubActorMessage;
    type State = PeerHubActorState;
    type Arguments = NodeId;

    async fn pre_start(
        &self,
        _myself: ractor::ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> std::result::Result<Self::State, ractor::ActorProcessingErr> {
        Ok(PeerHubActorState::new(args))
    }

    async fn handle(
        &self,
        _myself: ractor::ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> std::result::Result<(), ractor::ActorProcessingErr> {
        match message {
            PeerHubActorMessage::ShouldConnect(node_id, reply_port) => {
                info!("PeerHubActor received: ShouldConnect {:?}", node_id);
                if state.peers.len() >= PEER_SIZE_LIMIT {
                    reply_port.send(false)?;
                    info!("Rejecting connection from {:?}", node_id);
                } else {
                    let contains = state.peers.contains_key(&node_id);
                    let should_connect = !contains;
                    reply_port.send(should_connect)?;
                    info!("{} accepting connection from {:?}", should_connect, node_id);
                }
            }
            PeerHubActorMessage::Broadcast(node_id, broadcast_msg) => {
                info!(
                    "PeerHubActor received: Broadcast {:?} - {:?}",
                    node_id, broadcast_msg
                );
                PeerHubActor::broadcast(state, node_id, broadcast_msg).await?;
            }
            PeerHubActorMessage::NewPeer(node_id, send_stream, reply) => {
                info!("PeerHubActor received: NewPeer {:?}", node_id);
                if let Entry::Vacant(e) = state.peers.entry(node_id) {
                    e.insert(send_stream);
                    reply.send(true)?;
                } else {
                    reply.send(false)?;
                }
            }
            PeerHubActorMessage::NewTransaction(txn) => {
                info!("PeerHubActor received: NewTransaction {:?}", txn.id());

                PeerHubActor::broadcast(
                    state,
                    state.local_node_id,
                    BroadcastMessage::AnnounceTransaction(txn.id()),
                )
                .await?;
                // NOTE: cannot insert before we broadcast.
                state.mempool.insert(txn.id(), txn);
            }
        }
        Ok(())
    }
}
