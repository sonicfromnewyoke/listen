use std::error::Error;
use std::str::FromStr;
use anchor_lang::system_program;
use base64::Engine;
use futures_util::StreamExt;
use log::info;

use borsh::BorshSerialize;

use serde::{Deserialize, Serialize};
use solana_client::nonblocking::pubsub_client::PubsubClient;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::nonce_utils::nonblocking::get_account_with_commitment;
use solana_client::rpc_config::{RpcTransactionLogsConfig, RpcTransactionLogsFilter};
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use solana_sdk::signer::{EncodableKey, Signer};
use solana_transaction_status::{EncodedConfirmedTransactionWithStatusMeta, EncodedTransaction, UiMessage, UiParsedMessage};

use crate::get_tx_async;
use crate::raydium::make_compute_budget_ixs;
use crate::util::{env, pubkey_to_string, string_to_pubkey};

pub const BLOXROUTE_ADDRESS: &str = "HWEoBxYs7ssKuudEjzjmpfJVX7Dvi7wescFsVx2L5yoY";
pub const PUMP_GLOBAL_ADDRESS: &str = "4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf";
pub const PUMP_FEE_ADDRESS: &str = "CebN5WGQ4jvEPvsVU4EoHEpgzq1VV7AbicfhtW4xC9iM";
pub const PUMP_FUN_PROGRAM: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
pub const PUMP_FUN_MINT_AUTHORITY: &str = "TSLvdd1pWpHVjahSpsvCXUbgwsL3JAcvokwaKt1eokM";
pub const EVENT_AUTHORITY: &str = "Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp9F1";
pub const PUMP_BUY_METHOD: [u8; 8] = [0x66, 0x06, 0x3d, 0x12, 0x01, 0xda, 0xeb, 0xea];

#[derive(BorshSerialize)]
pub struct PumpFunBuyInstructionData {
    pub method_id: [u8; 8],
    pub token_amount: u64,
    pub lamports: u64,
}

pub async fn buy_pump_token(pump_accounts: PumpAccounts, token_amount: u64, lamports: u64) -> Result<(), Box<dyn Error>> {
    info!("Buying pump token {}", pump_accounts.mint.to_string());
    let wallet = Keypair::read_from_file("./fuck.json").expect("read wallet");
    let rpc_client = RpcClient::new("https://api.mainnet-beta.solana.com".to_string());
    let owner = wallet.pubkey();

    let mut ixs = vec![];
    ixs.append(&mut make_compute_budget_ixs(262500, 100000));
    // bloxroute might be required, it is used by pump but not sure if crucial they are probably an
    // enterprise user and that is why they are using it jito is probably fine, but jito rust sucks
    // coz of the stupid searcher_client lolz
    // 
    // 0.003 sol
    // let tip = 3000000;
    // ixs.push(solana_sdk::system_instruction::transfer(
    //     &owner,
    //     &Pubkey::from_str(BLOXROUTE_ADDRESS)?,
    //     tip,
    // ));
    let ata = spl_associated_token_account::get_associated_token_address(&owner, &pump_accounts.mint);
    let mut ata_ixs = raydium_library::common::create_ata_token_or_not(&owner, &pump_accounts.mint, &owner);

    ixs.append(&mut ata_ixs);
    ixs.push(make_pump_swap_ix(owner, pump_accounts, token_amount, lamports, ata)?);

    let recent_blockhash = rpc_client.get_latest_blockhash().await?;

    let transaction = solana_sdk::transaction::Transaction::new_signed_with_payer(
        &ixs,
        Some(&owner),
        &[&wallet],
        recent_blockhash
    );

    println!("signed: {}", transaction.is_signed());

    // send the tx
    let res = rpc_client.send_and_confirm_transaction_with_spinner(&transaction).await;
    match res {
        Ok(sig) => {
            info!("Transaction sent: {}", sig);
        }
        Err(e) => {
            return Err(e.into());
        }
    }

    Ok(())
}

/// Interact With Pump.Fun 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P
/// Input Accounts
/// #1 - Global: 4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf
/// #2 - Fee Recipient: Pump.fun Fee Account [Writable]
/// #3 - Mint 
/// #4 - Bonding Curve [Writable]
/// #5 - Associated Bonding Curve [Writable]
/// #6 - Associated User Account [Writable] (ATA)
/// #7 - User - owner, sender [Writable, Signer, Fee Payer]
/// #8 - System Program (11111111111111111111111111111111)
/// #9 - Token Program (TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA)
/// #10 - Rent (SysvarRent111111111111111111111111111111111)
/// #11 - Event Authority: Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp9F1
/// #12 - Program: Pump.fun Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P
pub fn make_pump_swap_ix(owner: Pubkey, pump_accounts: PumpAccounts, token_amount: u64, lamports: u64, ata: Pubkey) -> Result<Instruction, Box<dyn Error>> {
    let accounts: [AccountMeta; 12] = [
        AccountMeta::new_readonly(Pubkey::from_str(PUMP_GLOBAL_ADDRESS)?, false),
        AccountMeta::new(Pubkey::from_str(PUMP_FEE_ADDRESS)?, false), // writable
        AccountMeta::new_readonly(pump_accounts.mint, false),
        AccountMeta::new(pump_accounts.bonding_curve, false), // writable
        AccountMeta::new(pump_accounts.associated_bonding_curve, false), // writable
        AccountMeta::new(ata, false), // writable
        AccountMeta::new(owner, true), // writable, signer
        AccountMeta::new_readonly(system_program::ID, false),
        AccountMeta::new_readonly(spl_token::id(), false),
        AccountMeta::new_readonly(solana_sdk::sysvar::rent::id(), false),
        AccountMeta::new_readonly(Pubkey::from_str(EVENT_AUTHORITY)?, false),
        AccountMeta::new_readonly(Pubkey::from_str(PUMP_FUN_PROGRAM)?, false),
    ];

    let data = PumpFunBuyInstructionData {
        method_id: PUMP_BUY_METHOD,
        token_amount,
        lamports,
    };

    Ok(Instruction::new_with_borsh(
        Pubkey::from_str(PUMP_FUN_PROGRAM)?,
        &data,
        accounts.to_vec(),
    ))
}

pub async fn listen_pump() -> Result<(), Box<dyn Error>> {
    let client = PubsubClient::new(&env("WS_URL"))
        .await
        .expect("pubsub client async");
    let (mut notifications, unsub) = client
        .logs_subscribe(
            RpcTransactionLogsFilter::Mentions(vec![PUMP_FUN_MINT_AUTHORITY.to_string()]),
            RpcTransactionLogsConfig {
                commitment: Some(CommitmentConfig::confirmed()),
            },
        )
        .await
        .expect("subscribe to logs");
    info!("Listening for PumpFun events");
    while let Some(log) = notifications.next().await {
        let sig = log.value.signature;
        let tx = get_tx_async(&sig).await?;
        let accounts = parse_pump_accounts(tx)?;
        println!("{}: {}", sig, serde_json::to_string_pretty(&accounts).unwrap());
        println!("Fetching metadata");
    }
    unsub().await;
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PumpAccounts {
    #[serde(
        serialize_with = "pubkey_to_string",
        deserialize_with = "string_to_pubkey"
    )]
    pub mint: Pubkey,
    #[serde(
        serialize_with = "pubkey_to_string",
        deserialize_with = "string_to_pubkey"
    )]
    pub bonding_curve: Pubkey,
    #[serde(
        serialize_with = "pubkey_to_string",
        deserialize_with = "string_to_pubkey"
    )]
    pub associated_bonding_curve: Pubkey,
    #[serde(
        serialize_with = "pubkey_to_string",
        deserialize_with = "string_to_pubkey"
    )]
    pub dev: Pubkey,
    #[serde(
        serialize_with = "pubkey_to_string",
        deserialize_with = "string_to_pubkey"
    )]
    pub metadata: Pubkey,
}

/// this kinda works but not using since metadata takes some time to populate
pub async fn fetch_metadata(rpc_client: &RpcClient ,metadata: Pubkey) -> Result<(), Box<dyn Error>> {
    let acc = get_account_with_commitment(rpc_client, &metadata, CommitmentConfig::confirmed()).await?;
    let account_data = base64::prelude::BASE64_STANDARD.decode(acc.data).expect("decode spl b64");
    println!("{:?}", account_data);

    Ok(())
}

pub fn parse_pump_accounts(tx: EncodedConfirmedTransactionWithStatusMeta) -> Result<PumpAccounts, Box<dyn Error>> {
    if let EncodedTransaction::Json(tx) = &tx.transaction.transaction {
        if let UiMessage::Parsed(UiParsedMessage {
            account_keys,
            instructions: _,
            recent_blockhash: _,
            address_table_lookups: _,
        }) = &tx.message {
            if account_keys.len() >= 5 {
                let dev = account_keys[0].pubkey.parse()?;
                let mint = account_keys[1].pubkey.parse()?;
                let bonding_curve = account_keys[3].pubkey.parse()?;
                let associated_bonding_curve = account_keys[4].pubkey.parse()?;
                let metadata = account_keys[5].pubkey.parse()?;

                Ok(PumpAccounts {
                    mint,
                    bonding_curve,
                    associated_bonding_curve,
                    dev,
                    metadata,
                })
            } else {
                Err("Not enough account keys".into())
            }
        } else {
            Err("Not a parsed transaction".into())
        }
    } else {
        Err("Not a JSON transaction".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_pump_accounts() {
        let sample_tx = std::fs::read_to_string("pump_fun_tx.json").expect("read tx");
        let tx: EncodedConfirmedTransactionWithStatusMeta = serde_json::from_str(&sample_tx).expect("parse tx");
        let accounts = parse_pump_accounts(tx).expect("parse accounts");
        println!("{:?}", accounts);
        assert!(accounts.mint.to_string() == "6kPvKNrLqg23mApAvHzMKWohhVdSrA54HvrpYud8pump");
        assert!(accounts.bonding_curve.to_string() == "6TGz5VAFF6UpSmTSk9327utugSWJCyVeVVFXDtZnMtNp");
        assert!(accounts.associated_bonding_curve.to_string() == "4VwNGUif2ubbPjx4YNHmxEH7L4Yt2QFeo8uVTrVC3F68");
        assert!(accounts.dev.to_string() == "2wgo94ZaiUNUkFBSKNaKsUgEANgSdex7gRpFKR39DPzw");
    }

    #[tokio::test]
    #[ignore]
    async fn test_buy_pump_token() {
        // 0.001 sol, 100% slippage
        let lamports = 1000000;
        let token_amount = 0; 
        let pump_accounts = PumpAccounts {
            mint: Pubkey::from_str("5KEDcNGebCcLptWzknqVmPRNLHfiHA9Mm2djVE26pump").expect("parse mint"),
            bonding_curve: Pubkey::from_str("Drhj4djqLsPyiA9qK2YmBngteFba8XhhvuQoBToW6pMS").expect("parse bonding curve"),
            associated_bonding_curve: Pubkey::from_str("7uXq8diH862Dh8NgMHt5Tzsai8SvURhH58rArgxvs7o1").expect("parse associated bonding curve"),
            dev: Pubkey::from_str("Gizxxed4uXCzL7Q8DyALDVoEEDfMkSV7XyUNrPDnPJ9J").expect("parse associated user"),
            metadata: Pubkey::default(), // not required
        };
        buy_pump_token(pump_accounts, token_amount, lamports).await.expect("buy pump token");
    }
}