#![feature(array_try_from_fn)]

use anyhow::Result;
use ddcoin::{CoinAddress, PeerHubActorMessage, Transaction};
use ed25519_dalek::SigningKey;
use ractor::ActorRef;
use rand::rngs::OsRng;
use tokio::task::spawn_blocking;

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

fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut s = String::new();
    for byte in bytes {
        s.push_str(&format!("{:02x}", byte));
    }
    s
}

fn login_or_create_account() -> Result<SigningKey> {
    println!("Input your signing key or press Enter create a new one.");
    let mut input = String::new();
    loop {
        std::io::stdin().read_line(&mut input)?;
        if input.trim().is_empty() {
            let mut csprng = OsRng;
            let signing_key = SigningKey::generate(&mut csprng);
            let bytes = signing_key.to_bytes();
            println!("Signing key created: {}", bytes_to_hex(&bytes));

            return Ok(signing_key);
        }
        match hex_to_bytes(&input) {
            Ok(bytes) => {
                println!("Signing key loaded.");
                let key = SigningKey::from_bytes(&bytes);
                return Ok(key);
            }
            Err(e) => {
                println!("Error: {}", e);
                println!("Try again or press Enter to create a new account.");
            }
        }
    }
}

fn create_transactions(
    peer_hub: ActorRef<PeerHubActorMessage>,
    mut signing_key: SigningKey,
) -> Result<()> {
    fn create_single_txn(signing_key: &mut SigningKey) -> Result<Transaction> {
        println!("Creating a new transaction, input as following format:");
        println!("receiver_pub_key amount txn_fee");
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        let params = input.split_whitespace().collect::<Vec<_>>();
        let (receiver_pub_key, amount, input) = match params.len() {
            3 => (params[0], params[1], params[2]),
            _ => {
            return Err(anyhow::anyhow!("Too many/few parameters"));
            }
        };
        let receiver_pub_key = hex_to_bytes(receiver_pub_key)?;
        let amount = amount.parse::<u64>()?;
        let fee = input.parse::<u64>()?;
        let receiver_pub_key = CoinAddress::from_bytes(&receiver_pub_key)?;
        let txn = ddcoin::Transaction::new(signing_key, receiver_pub_key, amount, fee);
        println!("Transaction created: {}", txn);
        Ok(txn)
    }
    loop {
        let txn = create_single_txn(&mut signing_key)?;
        peer_hub.cast(PeerHubActorMessage::NewTransaction(txn))?;
    }
    #[allow(unreachable_code)]
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let (peer_hub, tasks_handle) = ddcoin::run().await?;

    let signing_key = spawn_blocking(login_or_create_account).await??;
    spawn_blocking(move || create_transactions(peer_hub, signing_key)).await??;
    tasks_handle.await?;
    Ok(())
}
