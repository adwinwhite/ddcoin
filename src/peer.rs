use std::fmt::Display;

use anyhow::Result;
use iroh::{NodeId, endpoint::RecvStream};
use ractor::ActorRef;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::{
    Block, Transaction, block::BlockId, peerhub::PeerHubActorMessage, serdes::transport,
    transaction::TransactionId,
};

#[derive(Debug, Serialize, Deserialize)]
pub enum BroadcastMessage {
    AnnounceTransaction(TransactionId),
    AnnounceBlock(BlockId),
}

impl Display for BroadcastMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BroadcastMessage::AnnounceTransaction(txn_id) => {
                write!(f, "AnnounceTransaction({})", txn_id)
            }
            BroadcastMessage::AnnounceBlock(block_id) => {
                write!(f, "AnnounceBlock({})", block_id)
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
    GetBlockRequest(BlockId),
    // We may need fetch previous blocks as well so we need to know block's source.
    GetBlockResponse(NodeId, Block),
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
            PeerMessage::GetBlockRequest(block_id) => {
                write!(f, "GetBlockRequest({})", block_id)
            }
            PeerMessage::GetBlockResponse(node_id, block) => {
                write!(f, "GetBlockResponse({}, {})", node_id, block)
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
            let message: PeerMessage = transport::decode(&buf)?;
            info!("Peer received: {}", message);
            match message {
                PeerMessage::Broadcast(msg) => match msg {
                    BroadcastMessage::AnnounceTransaction(txn_id) => {
                        self.peer_hub
                            .cast(PeerHubActorMessage::AnnounceTransaction(
                                self.node_id,
                                txn_id,
                            ))?;
                    }
                    BroadcastMessage::AnnounceBlock(block_id) => {
                        self.peer_hub
                            .cast(PeerHubActorMessage::AnnounceBlock(self.node_id, block_id))?;
                    }
                },
                PeerMessage::GetTransactionRequest(txn_id) => {
                    self.peer_hub
                        .cast(PeerHubActorMessage::GetTransaction(self.node_id, txn_id))?;
                }
                PeerMessage::GetTransactionResponse(txn) => {
                    self.peer_hub
                        .cast(PeerHubActorMessage::NewTransaction(txn))?;
                }
                PeerMessage::GetBlockRequest(block_id) => {
                    self.peer_hub
                        .cast(PeerHubActorMessage::GetBlock(self.node_id, block_id))?;
                }
                PeerMessage::GetBlockResponse(node_id, block) => {
                    self.peer_hub
                        .cast(PeerHubActorMessage::NewBlock(node_id, block))?;
                }
            }
        }
    }
}
