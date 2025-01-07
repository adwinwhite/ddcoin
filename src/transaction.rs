use std::{fmt::Display, hash::Hash};

use ed25519_dalek::{ed25519::signature::SignerMut, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Eq, PartialEq, Hash, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct TransactionId {
    #[serde(with = "uuid::serde::compact")]
    id: Uuid,
}

impl Display for TransactionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.id)
    }
}

#[derive(Eq, PartialEq, Hash, Clone, Debug, Serialize, Deserialize)]
pub struct CoinAddress {
    pub_key: VerifyingKey,
}

impl CoinAddress {
    pub fn from_bytes(bytes: &[u8; 32]) -> Result<Self, anyhow::Error> {
        let pub_key = VerifyingKey::from_bytes(bytes)?;
        Ok(Self { pub_key })
    }
}

impl Display for CoinAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for b in self.pub_key.as_bytes() {
            write!(f, "{:02x}", b)?;
        }
        Ok(())
    }
}

#[derive(Eq, PartialEq, Clone, Debug, Serialize, Deserialize)]
pub struct Signature {
    sig: ed25519_dalek::Signature,
}

impl Hash for Signature {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.sig.to_bytes().hash(state);
    }
}

#[derive(Eq, PartialEq, Hash, Clone, Debug, Serialize, Deserialize)]
struct TransactionInner {
    id: TransactionId,
    sender: CoinAddress,
    receiver: CoinAddress,
    amount: u64,
    fee: u64,
}

#[derive(Eq, PartialEq, Hash, Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "TransactionValidator")]
pub struct Transaction {
    inner: TransactionInner,
    signature: Signature,
}

// [serde validation trick here](https://github.com/serde-rs/serde-rs.github.io/pull/148/files).
// Perhaps I should make a macro to duplicate the struct.
#[derive(Deserialize)]
struct TransactionValidator {
    inner: TransactionInner,
    signature: Signature,
}

impl std::convert::TryFrom<TransactionValidator> for Transaction {
    type Error = anyhow::Error;
    fn try_from(value: TransactionValidator) -> Result<Self, Self::Error> {
        let msg = crate::serdes::encode(&value.inner).unwrap();
        if let Ok(()) = value
            .inner
            .sender
            .pub_key
            .verify(&msg, &value.signature.sig)
        {
            Ok(Transaction {
                inner: value.inner,
                signature: value.signature,
            })
        } else {
            Err(anyhow::anyhow!("Invalid transaction"))
        }
    }
}

impl Transaction {
    pub fn new(
        signing_key: &mut ed25519_dalek::SigningKey,
        receiver: CoinAddress,
        amount: u64,
        fee: u64,
    ) -> Self {
        let id = Uuid::new_v4();
        let sender = CoinAddress {
            pub_key: signing_key.verifying_key(),
        };
        let inner = TransactionInner {
            id: TransactionId { id },
            sender,
            receiver,
            amount,
            fee,
        };
        let msg = crate::serdes::encode(&inner).unwrap();
        let signature = signing_key.sign(&msg);
        let signature = Signature { sig: signature };
        Self { inner, signature }
    }

    pub fn id(&self) -> TransactionId {
        self.inner.id
    }
}

impl Display for Transaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Transaction: id: {}, sender: {}, receiver: {}, amount: {}, fee: {}",
            self.inner.id,
            self.inner.sender,
            self.inner.receiver,
            self.inner.amount,
            self.inner.fee
        )
    }
}
