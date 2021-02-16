// Copyright (c) The Libra Core Contributors
// SPDX-License-Identifier: Apache-2.0

#![forbid(unsafe_code)]

use chrono::{
    prelude::{SecondsFormat, Utc},
    DateTime,
};
use cli::{
    client_proxy::ClientProxy,
    commands::{get_commands, parse_cmd, report_error, Command},
};
use libra_types::{chain_id::ChainId, waypoint::Waypoint};
use rustyline::{config::CompletionType, error::ReadlineError, Config, Editor};
use std::{env, str::FromStr, time::{Duration, UNIX_EPOCH}};
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
#[structopt(
    name = "Libra Client",
    author = "The Libra Association",
    about = "Libra client to connect to a specific validator"
)]
struct Args {
    /// Chain ID of the network this client is connecting to
    #[structopt(
        short = "c",
        long,
        help = "\
            Explicitly specify the chain ID of the network the CLI is connecting to: e.g.,
            for mainnet: \"MAINNET\" or 1, testnet: \"TESTNET\" or 2, devnet: \"DEVNET\" or 3, \
            local swarm: \"TESTING\" or 4
            Note: Chain ID of 0 is not allowed
        "
    )]
    pub chain_id: ChainId,
    /// Full URL address to connect to - should include port number, if applicable
    #[structopt(short = "u", long)]
    pub url: String,
    /// Path to the generated keypair for the faucet account. The faucet account can be used to
    /// mint coins. If not passed, a new keypair will be generated for
    /// you and placed in a temporary directory.
    /// To manually generate a keypair, use generate-key:
    /// `cargo run -p generate-keypair -- -o <output_file_path>`
    #[structopt(short = "m", long = "faucet-key-file-path")]
    pub faucet_account_file: Option<String>,
    /// Host that operates a faucet service
    /// If not passed, will be derived from host parameter
    #[structopt(short = "f", long)]
    pub faucet_url: Option<String>,
    /// File location from which to load mnemonic word for user account address/key generation.
    /// If not passed, a new mnemonic file will be generated by libra-wallet in the current
    /// directory.
    #[structopt(short = "n", long)]
    pub mnemonic_file: Option<String>,
    /// If set, client will sync with validator during wallet recovery.
    /// 0L Deprecated, always syncs on recovery.
    #[structopt(short = "r", long = "sync")]
    pub sync: bool,
    /// If set, a client uses the waypoint parameter for its initial LedgerInfo verification.
    #[structopt(
        name = "waypoint",
        long,
        help = "Explicitly specify the waypoint to use",
        required_unless = "waypoint_url"
    )]
    pub waypoint: Option<Waypoint>,
    #[structopt(
        name = "waypoint_url",
        long,
        help = "URL for a file with the waypoint to use",
        required_unless = "waypoint"
    )]
    pub waypoint_url: Option<String>,
    /// Verbose output.
    #[structopt(short = "v", long = "verbose")]
    pub verbose: bool,
}

fn main() {
    let args = Args::from_args();

    // TODO: Duplicated with 0L miner.

    let mut entered_mnem = false;
    println!("Enter your 0L mnemonic:");
    let mnemonic_string = match env::var("NODE_ENV") {
        Ok(val) => {
           match val.as_str() {
            "prod" => rpassword::read_password_from_tty(Some("\u{1F511}")).unwrap(),
            // for test and stage environments, so mnemonics can be inputted.
             _ => {
               println!("(unsafe STDIN input for testing) \u{1F511}");
               rpassword::read_password().unwrap()
             }
           }          
        },
        // if not set assume prod
        _ => rpassword::read_password_from_tty(Some("\u{1F511}")).unwrap()
    };

    if mnemonic_string.len() > 0 { entered_mnem = true };


    let mut logger = ::libra_logger::Logger::new();
    if !args.verbose {
        logger.level(::libra_logger::Level::Warn);
    }
    logger.init();
    crash_handler::setup_panic_handler();

    let (commands, alias_to_cmd) = get_commands(true);

    let faucet_account_file = args
        .faucet_account_file
        .clone()
        .unwrap_or_else(|| "".to_string());
    // Faucet, TreasuryCompliance and DD use the same keypair for now
    let treasury_compliance_account_file = faucet_account_file.clone();
    let dd_account_file = faucet_account_file.clone();
    let mnemonic_file = args.mnemonic_file.clone();

    // If waypoint is given explicitly, use its value,
    // otherwise waypoint_url is required, try to retrieve the waypoint from the URL.
    let waypoint = args.waypoint.unwrap_or_else(|| {
        args.waypoint_url
            .as_ref()
            .map(|url_str| {
                retrieve_waypoint(url_str.as_str()).unwrap_or_else(|e| {
                    panic!("Failure to retrieve a waypoint from {}: {}", url_str, e)
                })
            })
            .unwrap()
    });

    let mut client_proxy = ClientProxy::new(
        args.chain_id,
        &args.url,
        &faucet_account_file,
        &treasury_compliance_account_file,
        &dd_account_file,
        true, // 0L change
        args.faucet_url.clone(),
        mnemonic_file,
        Some(mnemonic_string), // 0L change
        waypoint,
    )
    .expect("Failed to construct client.");
    
    // Test connection to validator
    let block_metadata = client_proxy
        .test_validator_connection()
        .unwrap_or_else(|e| {
            panic!(
                "Not able to connect to validator at {}. Error: {}",
                args.url, e,
            )
        });
    let ledger_info_str = format!(
        "latest version = {}, timestamp = {}",
        block_metadata.version,
        DateTime::<Utc>::from(UNIX_EPOCH + Duration::from_micros(block_metadata.timestamp))
    );
    let cli_info = format!(
        "Connected to validator at: {}, {}",
        args.url, ledger_info_str
    );
    // if args.mnemonic_file.is_some() {
    
    if entered_mnem || args.mnemonic_file.is_some() {
        match client_proxy.recover_accounts_in_wallet() {
            Ok(account_data) => {
                println!(
                    "Wallet recovered and the first {} child accounts were derived",
                    account_data.len()
                );
                for data in account_data {
                    println!("#{} address {}", data.index, hex::encode(data.address));
                }
            }
            Err(e) => report_error("Error recovering Libra wallet", e),
        }
    }
    print_help(&cli_info, &commands);
    println!("Please, input commands: \n");

    let config = Config::builder()
        .history_ignore_space(true)
        .completion_type(CompletionType::List)
        .auto_add_history(true)
        .build();
    let mut rl = Editor::<()>::with_config(config);
    loop {
        let readline = rl.readline("libra% ");
        match readline {
            Ok(line) => {
                let params = parse_cmd(&line);
                if params.is_empty() {
                    continue;
                }
                match alias_to_cmd.get(&params[0]) {
                    Some(cmd) => {
                        if args.verbose {
                            println!("{}", Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true));
                        }
                        cmd.execute(&mut client_proxy, &params);
                    }
                    None => match params[0] {
                        "quit" | "q!" => break,
                        "help" | "h" => print_help(&cli_info, &commands),
                        "" => continue,
                        x => println!("Unknown command: {:?}", x),
                    },
                }
            }
            Err(ReadlineError::Interrupted) => {
                println!("CTRL-C");
                break;
            }
            Err(ReadlineError::Eof) => {
                println!("CTRL-D");
                break;
            }
            Err(err) => {
                println!("Error: {:?}", err);
                break;
            }
        }
    }
}

/// Print the help message for the client and underlying command.
fn print_help(client_info: &str, commands: &[std::sync::Arc<dyn Command>]) {
    println!("{}", client_info);
    println!("usage: <command> <args>\n\nUse the following commands:\n");
    for cmd in commands {
        println!(
            "{} {}\n\t{}",
            cmd.get_aliases().join(" | "),
            cmd.get_params_help(),
            cmd.get_description()
        );
    }

    println!("help | h \n\tPrints this help");
    println!("quit | q! \n\tExit this client");
    println!("\n");
}

/// Retrieve a waypoint given the URL.
fn retrieve_waypoint(url_str: &str) -> anyhow::Result<Waypoint> {
    let client = reqwest::blocking::ClientBuilder::new().build()?;
    let response = client.get(url_str).send()?;

    Ok(response
        .error_for_status()
        .map_err(|_| anyhow::format_err!("Failed to retrieve waypoint from URL {}", url_str))?
        .text()
        .map(|r| Waypoint::from_str(r.trim()))??)
}
