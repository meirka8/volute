use anyhow::{Context, Result};
use keyring::Entry;
use serde::{Deserialize, Serialize};

const KEYRING_SERVICE: &str = "cvc-cli";
const KEYRING_USER: &str = "current_user_token";

#[derive(Serialize, Deserialize, Debug)]
struct TokenData {
    access_token: String,
    refresh_token: String,
    expiry: u64,
}

pub async fn login() -> Result<()> {
    println!("Initiating Device Authorization Flow...");
    println!("Please visit: https://cvc.helixthought.com/activate");
    println!("Enter code: ABCD-1234");

    println!("Waiting for authorization...");
    // Mocking the polling loop
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

    let mock_token = TokenData {
        access_token: "mock_access_token_123".to_string(),
        refresh_token: "mock_refresh_token_456".to_string(),
        expiry: 9999999999,
    };

    let json = serde_json::to_string(&mock_token)?;

    let entry = Entry::new(KEYRING_SERVICE, KEYRING_USER)
        .map_err(|e| anyhow::anyhow!("Keyring error: {}", e))?;

    entry
        .set_password(&json)
        .map_err(|e| anyhow::anyhow!("Failed to save token: {}", e))?;

    println!("Successfully logged in!");
    Ok(())
}

pub async fn status() -> Result<()> {
    let entry = Entry::new(KEYRING_SERVICE, KEYRING_USER)
        .map_err(|e| anyhow::anyhow!("Keyring error: {}", e))?;

    match entry.get_password() {
        Ok(_) => {
            println!("Authenticated.");
            Ok(())
        }
        Err(_) => {
            println!("Not authenticated. Run 'cvc auth login'.");
            Ok(())
        }
    }
}
