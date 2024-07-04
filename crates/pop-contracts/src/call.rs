// SPDX-License-Identifier: GPL-3.0

use crate::{
	errors::Error,
	utils::{
		helpers::{get_manifest_path, parse_account, parse_balance},
		signer::create_signer,
	},
};
use anyhow::Context;
use contract_build::Verbosity;
use contract_extrinsics::{
	BalanceVariant, CallCommandBuilder, CallExec, DisplayEvents, ErrorVariant,
	ExtrinsicOptsBuilder, TokenMetadata,
};
use ink_env::{DefaultEnvironment, Environment};
use sp_weights::Weight;
use std::path::PathBuf;
use subxt::{Config, PolkadotConfig as DefaultConfig};
use subxt_signer::sr25519::Keypair;
use url::Url;

/// Attributes for the `call` command.
pub struct CallOpts {
	/// Path to the contract build folder.
	pub path: Option<PathBuf>,
	/// The address of the contract to call.
	pub contract: String,
	/// The name of the contract message to call.
	pub message: String,
	/// The constructor arguments, encoded as strings.
	pub args: Vec<String>,
	/// Transfers an initial balance to the instantiated contract.
	pub value: String,
	/// Maximum amount of gas to be used for this command.
	pub gas_limit: Option<u64>,
	/// Maximum proof size for the instantiation.
	pub proof_size: Option<u64>,
	/// Websocket endpoint of a node.
	pub url: Url,
	/// Secret key URI for the account deploying the contract.
	pub suri: String,
	/// Submit an extrinsic for on-chain execution.
	pub execute: bool,
}

/// Prepare the preprocessed data for a contract `call`.
///
/// # Arguments
///
/// * `call_opts` - options for the `call` command.
///
pub async fn set_up_call(
	call_opts: CallOpts,
) -> anyhow::Result<CallExec<DefaultConfig, DefaultEnvironment, Keypair>> {
	let token_metadata = TokenMetadata::query::<DefaultConfig>(&call_opts.url).await?;
	let manifest_path = get_manifest_path(call_opts.path.as_deref())?;
	let signer = create_signer(&call_opts.suri)?;

	let extrinsic_opts = ExtrinsicOptsBuilder::new(signer)
		.manifest_path(Some(manifest_path))
		.url(call_opts.url.clone())
		.done();

	let value: BalanceVariant<<DefaultEnvironment as Environment>::Balance> =
		parse_balance(&call_opts.value)?;

	let contract: <DefaultConfig as Config>::AccountId = parse_account(&call_opts.contract)?;

	let call_exec: CallExec<DefaultConfig, DefaultEnvironment, Keypair> =
		CallCommandBuilder::new(contract.clone(), &call_opts.message, extrinsic_opts)
			.args(call_opts.args.clone())
			.value(value.denominate_balance(&token_metadata)?)
			.gas_limit(call_opts.gas_limit)
			.proof_size(call_opts.proof_size)
			.done()
			.await?;
	return Ok(call_exec);
}

/// Simulate a smart contract call without modifying the state of the blockchain.
///
/// # Arguments
///
/// * `call_exec` - struct with the call to be executed.
///
pub async fn dry_run_call(
	call_exec: &CallExec<DefaultConfig, DefaultEnvironment, Keypair>,
) -> Result<String, Error> {
	let call_result = call_exec.call_dry_run().await?;
	match call_result.result {
		Ok(ref ret_val) => {
			let value = call_exec
				.transcoder()
				.decode_message_return(call_exec.message(), &mut &ret_val.data[..])
				.context(format!("Failed to decode return value {:?}", &ret_val))?;
			Ok(value.to_string())
		},
		Err(ref err) => {
			let error_variant =
				ErrorVariant::from_dispatch_error(err, &call_exec.client().metadata())?;
			Err(Error::DryRunCallContractError(format!("{error_variant}")))
		},
	}
}

/// Estimate the gas required for a contract call without modifying the state of the blockchain.
///
/// # Arguments
///
/// * `call_exec` - the preprocessed data to call a contract.
///
pub async fn dry_run_gas_estimate_call(
	call_exec: &CallExec<DefaultConfig, DefaultEnvironment, Keypair>,
) -> Result<Weight, Error> {
	let call_result = call_exec.call_dry_run().await?;
	match call_result.result {
		Ok(_) => {
			// Use user specified values where provided, otherwise use the estimates.
			let ref_time =
				call_exec.gas_limit().unwrap_or_else(|| call_result.gas_required.ref_time());
			let proof_size =
				call_exec.proof_size().unwrap_or_else(|| call_result.gas_required.proof_size());
			Ok(Weight::from_parts(ref_time, proof_size))
		},
		Err(ref err) => {
			let error_variant =
				ErrorVariant::from_dispatch_error(err, &call_exec.client().metadata())?;
			Err(Error::DryRunCallContractError(format!("{error_variant}")))
		},
	}
}

/// Call a smart contract on the blockchain.
///
/// # Arguments
///
/// * `call_exec` - struct with the call to be executed.
/// * `gas_limit` - maximum amount of gas to be used for this call.
/// * `url` - endpoint of the node which to send the call to.
///
pub async fn call_smart_contract(
	call_exec: CallExec<DefaultConfig, DefaultEnvironment, Keypair>,
	gas_limit: Weight,
	url: &Url,
) -> anyhow::Result<String, ErrorVariant> {
	let token_metadata = TokenMetadata::query::<DefaultConfig>(url).await?;
	let metadata = call_exec.client().metadata();
	let events = call_exec.call(Some(gas_limit)).await?;
	let display_events =
		DisplayEvents::from_events::<DefaultConfig, DefaultEnvironment>(&events, None, &metadata)?;

	let output =
		display_events.display_events::<DefaultEnvironment>(Verbosity::Default, &token_metadata)?;
	Ok(output)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{create_smart_contract, errors::Error, templates::Contract};
	use anyhow::Result;
	use std::{env, fs};

	const CONTRACTS_NETWORK_URL: &str = "wss://rococo-contracts-rpc.polkadot.io";

	fn generate_smart_contract_test_environment() -> Result<tempfile::TempDir> {
		let temp_dir = tempfile::tempdir().expect("Could not create temp dir");
		let temp_contract_dir = temp_dir.path().join("testing");
		fs::create_dir(&temp_contract_dir)?;
		create_smart_contract("testing", temp_contract_dir.as_path(), &Contract::Standard)?;
		Ok(temp_dir)
	}
	// Function that mocks the build process generating the contract artifacts.
	fn mock_build_process(temp_contract_dir: PathBuf) -> Result<(), Error> {
		// Create a target directory
		let target_contract_dir = temp_contract_dir.join("target");
		fs::create_dir(&target_contract_dir)?;
		fs::create_dir(&target_contract_dir.join("ink"))?;
		// Copy a mocked testing.contract file inside the target directory
		let current_dir = env::current_dir().expect("Failed to get current directory");
		let contract_file = current_dir.join("tests/files/testing.contract");
		fs::copy(contract_file, &target_contract_dir.join("ink/testing.contract"))?;
		Ok(())
	}

	#[tokio::test]
	async fn test_set_up_call() -> Result<()> {
		let temp_dir = generate_smart_contract_test_environment()?;
		mock_build_process(temp_dir.path().join("testing"))?;

		let call_opts = CallOpts {
			path: Some(temp_dir.path().join("testing")),
			contract: "5CLPm1CeUvJhZ8GCDZCR7nWZ2m3XXe4X5MtAQK69zEjut36A".to_string(),
			message: "get".to_string(),
			args: [].to_vec(),
			value: "1000".to_string(),
			gas_limit: None,
			proof_size: None,
			url: Url::parse(CONTRACTS_NETWORK_URL)?,
			suri: "//Alice".to_string(),
			execute: false,
		};
		let call = set_up_call(call_opts).await?;
		assert_eq!(call.message(), "get");
		Ok(())
	}

	#[tokio::test]
	async fn test_set_up_call_error_contract_not_build() -> Result<()> {
		let temp_dir = generate_smart_contract_test_environment()?;
		let call_opts = CallOpts {
			path: Some(temp_dir.path().join("testing")),
			contract: "5CLPm1CeUvJhZ8GCDZCR7nWZ2m3XXe4X5MtAQK69zEjut36A".to_string(),
			message: "get".to_string(),
			args: [].to_vec(),
			value: "1000".to_string(),
			gas_limit: None,
			proof_size: None,
			url: Url::parse(CONTRACTS_NETWORK_URL)?,
			suri: "//Alice".to_string(),
			execute: false,
		};
		let call = set_up_call(call_opts).await;
		assert!(call.is_err());
		let error = call.err().unwrap();
		assert_eq!(error.root_cause().to_string(), "Failed to find any contract artifacts in target directory. \nRun `cargo contract build --release` to generate the artifacts.");

		Ok(())
	}
	#[tokio::test]
	async fn test_set_up_call_fails_no_smart_contract_folder() -> Result<()> {
		let call_opts = CallOpts {
			path: None,
			contract: "5CLPm1CeUvJhZ8GCDZCR7nWZ2m3XXe4X5MtAQK69zEjut36A".to_string(),
			message: "get".to_string(),
			args: [].to_vec(),
			value: "1000".to_string(),
			gas_limit: None,
			proof_size: None,
			url: Url::parse(CONTRACTS_NETWORK_URL)?,
			suri: "//Alice".to_string(),
			execute: false,
		};
		let call = set_up_call(call_opts).await;
		assert!(call.is_err());
		let error = call.err().unwrap();
		assert_eq!(error.root_cause().to_string(), "No 'ink' dependency found");

		Ok(())
	}

	#[tokio::test]
	async fn test_dry_run_call_error_contract_not_deployed() -> Result<()> {
		let temp_dir = generate_smart_contract_test_environment()?;
		mock_build_process(temp_dir.path().join("testing"))?;

		let call_opts = CallOpts {
			path: Some(temp_dir.path().join("testing")),
			contract: "5CLPm1CeUvJhZ8GCDZCR7nWZ2m3XXe4X5MtAQK69zEjut36A".to_string(),
			message: "get".to_string(),
			args: [].to_vec(),
			value: "1000".to_string(),
			gas_limit: None,
			proof_size: None,
			url: Url::parse(CONTRACTS_NETWORK_URL)?,
			suri: "//Alice".to_string(),
			execute: false,
		};
		let call = set_up_call(call_opts).await?;
		assert!(matches!(dry_run_call(&call).await, Err(Error::DryRunCallContractError(..))));
		Ok(())
	}

	#[tokio::test]
	async fn test_dry_run_estimate_call_error_contract_not_deployed() -> Result<()> {
		let temp_dir = generate_smart_contract_test_environment()?;
		mock_build_process(temp_dir.path().join("testing"))?;

		let call_opts = CallOpts {
			path: Some(temp_dir.path().join("testing")),
			contract: "5CLPm1CeUvJhZ8GCDZCR7nWZ2m3XXe4X5MtAQK69zEjut36A".to_string(),
			message: "get".to_string(),
			args: [].to_vec(),
			value: "1000".to_string(),
			gas_limit: None,
			proof_size: None,
			url: Url::parse(CONTRACTS_NETWORK_URL)?,
			suri: "//Alice".to_string(),
			execute: false,
		};
		let call = set_up_call(call_opts).await?;
		assert!(matches!(
			dry_run_gas_estimate_call(&call).await,
			Err(Error::DryRunCallContractError(..))
		));
		Ok(())
	}
}
