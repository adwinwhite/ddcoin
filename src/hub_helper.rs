use anyhow::{Result, bail};
use ed25519_dalek::SigningKey;
use iroh::NodeId;
use ractor::ActorRef;

use crate::{
    Block, CoinAddress, PeerHubActorMessage, Transaction,
    block::BlockId,
    transaction::{Cash, TransactionId},
};

mod private {
    pub trait Sealed {}
}

pub trait HubHelper: private::Sealed {
    fn new_transfer(
        &self,
        signer: &mut SigningKey,
        receiver: &CoinAddress,
        amount: u64,
        fee: u64,
    ) -> impl Future<Output = Result<TransactionId>> + Send;

    fn leading_block(&self) -> impl Future<Output = Result<Block>> + Send;

    fn transaction_depth(
        &self,
        id: TransactionId,
    ) -> impl Future<Output = Result<Option<u64>>> + Send;

    fn get_transaction(
        &self,
        id: TransactionId,
    ) -> impl Future<Output = Result<Option<Transaction>>> + Send;

    fn get_block(&self, id: BlockId) -> impl Future<Output = Result<Option<Block>>> + Send;

    fn peers(&self) -> impl Future<Output = Result<Vec<NodeId>>> + Send;

    fn id(&self) -> impl Future<Output = Result<NodeId>> + Send;
}

impl private::Sealed for ActorRef<PeerHubActorMessage> {}

impl HubHelper for ActorRef<PeerHubActorMessage> {
    async fn new_transfer(
        &self,
        signer: &mut SigningKey,
        receiver: &CoinAddress,
        amount: u64,
        fee: u64,
    ) -> Result<TransactionId> {
        let cash_set = ractor::call!(self, PeerHubActorMessage::QueryLeadingCashSet)?;
        let sender = CoinAddress::from_signer(signer);

        // Try to find enough cash for this transfer.
        let mut inputs = Vec::new();
        for cash in cash_set.iter() {
            if *cash.owner() != sender {
                continue;
            }

            inputs.push(cash.clone());
            if inputs.iter().map(|c| c.amount()).sum::<u64>() >= amount + fee {
                break;
            }
        }
        if inputs.iter().map(|c| c.amount()).sum::<u64>() < amount + fee {
            bail!("insufficient cash from sender");
        }

        let transfer_cash = Cash::new(amount, receiver.clone());
        let remaining_amount = inputs.iter().map(|c| c.amount()).sum::<u64>() - amount - fee;
        let remaining_cash = Cash::new(remaining_amount, sender.clone());
        let outputs = [transfer_cash, remaining_cash];
        let transaction = Transaction::new(signer, &inputs, &outputs)?;
        let transaction_id = transaction.id();
        ractor::cast!(self, PeerHubActorMessage::NewTransaction(transaction))?;

        Ok(transaction_id)
    }

    async fn leading_block(&self) -> Result<Block> {
        let id = ractor::call!(self, PeerHubActorMessage::QueryLeadingBlock)?;
        let block = ractor::call!(self, PeerHubActorMessage::QueryBlock, id)?
            .expect("at least genesis block should exist");
        Ok(block)
    }

    async fn transaction_depth(&self, id: TransactionId) -> Result<Option<u64>> {
        ractor::call!(self, PeerHubActorMessage::QueryTransactionDepth, id).map_err(Into::into)
    }

    async fn get_transaction(&self, id: TransactionId) -> Result<Option<Transaction>> {
        ractor::call!(self, PeerHubActorMessage::QueryTransaction, id).map_err(Into::into)
    }

    async fn get_block(&self, id: BlockId) -> Result<Option<Block>> {
        ractor::call!(self, PeerHubActorMessage::QueryBlock, id).map_err(Into::into)
    }

    async fn peers(&self) -> Result<Vec<NodeId>> {
        ractor::call!(self, PeerHubActorMessage::QueryPeers).map_err(Into::into)
    }

    async fn id(&self) -> Result<NodeId> {
        ractor::call!(self, PeerHubActorMessage::QueryLocalNodeId).map_err(Into::into)
    }
}
