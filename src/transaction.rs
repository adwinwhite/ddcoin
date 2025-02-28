use std::{fmt::Display, hash::Hash};

use anyhow::bail;
use ed25519_dalek::{Verifier, VerifyingKey, ed25519::signature::SignerMut};
use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{serdes::hashsig, util::Sha256Hash};

#[derive(Eq, PartialEq, Hash, Clone, Debug, Serialize, Deserialize)]
pub struct Cash {
    id: Uuid,
    amount: u64,
    owner: CoinAddress,
}

impl Cash {
    pub fn new(amount: u64, owner: CoinAddress) -> Self {
        Self {
            id: Uuid::new_v4(),
            amount,
            owner,
        }
    }

    pub fn genesis_reward(amount: u64, owner: CoinAddress) -> Self {
        Self {
            id: Uuid::nil(),
            amount,
            owner,
        }
    }

    pub fn amount(&self) -> u64 {
        self.amount
    }

    pub fn owner(&self) -> &CoinAddress {
        &self.owner
    }
}

impl Display for Cash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Cash {{ id: {}, amount: {}, owner: {} }}",
            self.id, self.amount, self.owner
        )
    }
}

pub type TransactionId = Sha256Hash;

#[derive(Eq, PartialEq, Hash, Clone, Debug, Serialize, Deserialize)]
pub struct CoinAddress {
    pub(crate) pub_key: [u8; 32],
}

impl CoinAddress {
    pub const fn from_bytes(bytes: &[u8; 32]) -> Self {
        Self { pub_key: *bytes }
    }

    pub fn from_signer(signing_key: &ed25519_dalek::SigningKey) -> Self {
        Self {
            pub_key: signing_key.verifying_key().to_bytes(),
        }
    }
}

impl From<VerifyingKey> for CoinAddress {
    fn from(pub_key: VerifyingKey) -> Self {
        Self {
            pub_key: pub_key.to_bytes(),
        }
    }
}

impl Display for CoinAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for b in self.pub_key {
            write!(f, "{:02x}", b)?;
        }
        Ok(())
    }
}

#[derive(Eq, PartialEq, Clone, Debug, Serialize, Deserialize)]
pub struct Signature {
    #[serde(with = "BigArray")]
    pub(crate) sig: [u8; 64],
}

impl Hash for Signature {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.sig.hash(state);
    }
}

impl From<ed25519_dalek::Signature> for Signature {
    fn from(sig: ed25519_dalek::Signature) -> Self {
        Self {
            sig: sig.to_bytes(),
        }
    }
}

impl Display for Signature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for b in self.sig {
            write!(f, "{:02x}", b)?;
        }
        Ok(())
    }
}

#[derive(Eq, PartialEq, Hash, Clone, Debug, Serialize, Deserialize)]
struct TransactionInner {
    inputs: Vec<Cash>,
    outputs: Vec<Cash>,
}

// We only ensure that `Transaction` is locally valid.
// - verified signature thus no middle-man modification.
// - inputs' owner is the signer.
// - inputs >= outputs.
// Validation like double spending or valid inputs depends on global state.
// It's postponed to block validation.
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
        // Apply construction checks.
        let inputs = &value.inner.inputs;
        let outputs = &value.inner.outputs;
        Transaction::check_inputs_and_outputs(inputs, outputs)?;

        let msg = crate::serdes::hashsig::encode(&value.inner).unwrap();
        let verifying_key = VerifyingKey::from_bytes(&inputs[0].owner.pub_key)?;
        if let Ok(()) = verifying_key.verify(
            &msg,
            &ed25519_dalek::Signature::from_bytes(&value.signature.sig),
        ) {
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
    // Validations.
    // 0. inputs cannot be empty.
    // 1. inputs' owner is consistent with signer.
    // 2. inputs' total amount is larger or equal to outputs'.
    fn check_inputs_and_outputs(inputs: &[Cash], outputs: &[Cash]) -> Result<(), anyhow::Error> {
        if inputs.is_empty() {
            bail!("inputs cannot be empty");
        }
        let input_owner = &inputs[0].owner;
        for input in &inputs[1..] {
            if input.owner != *input_owner {
                bail!("inputs owners aren't consistent");
            }
        }
        let input_amount: u64 = inputs.iter().map(|c| c.amount).sum();
        let output_amount: u64 = outputs.iter().map(|c| c.amount).sum();
        if input_amount < output_amount {
            bail!("insufficient inputs");
        }
        Ok(())
    }

    pub fn new(
        signing_key: &mut ed25519_dalek::SigningKey,
        inputs: &[Cash],
        outputs: &[Cash],
    ) -> Result<Self, anyhow::Error> {
        Self::check_inputs_and_outputs(inputs, outputs)?;

        let sender = CoinAddress {
            pub_key: signing_key.verifying_key().to_bytes(),
        };
        let input_owner = &inputs[0].owner;
        if sender != *input_owner {
            bail!("inputs owner isn't the provided signer");
        }
        let inner = TransactionInner {
            inputs: inputs.to_vec(),
            outputs: outputs.to_vec(),
        };
        let msg = crate::serdes::hashsig::encode(&inner).unwrap();
        let signature = signing_key.sign(&msg);
        let signature = Signature {
            sig: signature.into(),
        };
        Ok(Self { inner, signature })
    }

    pub fn id(&self) -> TransactionId {
        let bytes = hashsig::encode(&self).unwrap();
        let array: [u8; 32] = Sha256::digest(&bytes).into();
        array.into()
    }

    pub fn fee(&self) -> u64 {
        self.inner.inputs.iter().map(|c| c.amount).sum::<u64>()
            - self.inner.outputs.iter().map(|c| c.amount).sum::<u64>()
    }

    pub fn inputs(&self) -> &[Cash] {
        &self.inner.inputs
    }

    pub fn outputs(&self) -> &[Cash] {
        &self.inner.outputs
    }
}

impl Display for Transaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Transaction: id: {}, inputs: {:?}, outputs: {:?}",
            self.id(),
            self.inner.inputs,
            self.inner.outputs,
        )
    }
}
