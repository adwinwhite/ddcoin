use anyhow::Result;
use iroh::{endpoint::RecvStream, NodeId};
use ractor::ActorRef;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::{peerhub::PeerHubActorMessage, transaction::TransactionId, Transaction};

#[derive(Debug, Serialize, Deserialize)]
pub enum BroadcastMessage {
    AnnounceTransaction(TransactionId),
}

// FIXME: use Box or something.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Serialize, Deserialize)]
pub enum PeerMessage {
    Broadcast(BroadcastMessage),
    GetTransactionRequest(TransactionId),
    GetTransactionResponse(Transaction),
}

pub struct Peer {
    recv_stream: RecvStream,
    peer_hub: ActorRef<PeerHubActorMessage>,
    node_id: NodeId,
}

impl Peer {
    pub fn new(
        recv_stream: RecvStream,
        peer_hub: ActorRef<PeerHubActorMessage>,
        node_id: NodeId,
    ) -> Self {
        Self {
            recv_stream,
            peer_hub,
            node_id,
        }
    }

    pub async fn run(mut self) -> Result<!> {
        info!("Peer starts listening: {:?}", self.node_id);
        let mut len_buf = [0u8; 4];
        loop {
            self.recv_stream.read_exact(&mut len_buf).await?;
            let size = u32::from_be_bytes(len_buf) as usize;
            let mut buf = vec![0u8; size];
            self.recv_stream.read_exact(&mut buf).await?;
            let message: PeerMessage = crate::serdes::decode(&buf)?;
            match message {
                PeerMessage::Broadcast(msg) => {
                    match msg {
                        BroadcastMessage::AnnounceTransaction(txn_id) => {
                            info!("Peer received: transaction announcement - {}", txn_id);
                        }
                    }
                    self.peer_hub
                        .cast(PeerHubActorMessage::Broadcast(self.node_id, msg))?;
                }
                PeerMessage::GetTransactionRequest(txn_id) => {
                    self.peer_hub
                        .cast(PeerHubActorMessage::GetTransaction(self.node_id, txn_id))?;
                }
                PeerMessage::GetTransactionResponse(txn) => {
                    self.peer_hub
                        .cast(PeerHubActorMessage::NewTransaction(txn))?;
                }
            }
        }
    }
}
