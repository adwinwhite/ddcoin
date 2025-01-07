use std::fmt::Display;

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

impl Display for BroadcastMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BroadcastMessage::AnnounceTransaction(txn_id) => {
                write!(f, "AnnounceTransaction({})", txn_id)
            }
        }
    }
}

// FIXME: use Box or something.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Serialize, Deserialize)]
pub enum PeerMessage {
    Broadcast(BroadcastMessage),
    GetTransactionRequest(TransactionId),
    GetTransactionResponse(Transaction),
}

impl Display for PeerMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PeerMessage::Broadcast(msg) => write!(f, "Broadcast({})", msg),
            PeerMessage::GetTransactionRequest(txn_id) => {
                write!(f, "GetTransactionRequest({})", txn_id)
            }
            PeerMessage::GetTransactionResponse(txn) => {
                write!(f, "GetTransactionResponse({})", txn)
            }
        }
    }
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
            info!("Peer received: {}", message);
            match message {
                PeerMessage::Broadcast(msg) => {
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
