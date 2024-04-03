
//! Autogenerated weights for pallet_cf_account_roles
//!
//! THIS FILE WAS AUTO-GENERATED USING THE SUBSTRATE BENCHMARK CLI VERSION 4.0.0-dev
//! DATE: 2024-03-28, STEPS: `2`, REPEAT: `2`, LOW RANGE: `[]`, HIGH RANGE: `[]`
//! WORST CASE MAP SIZE: `1000000`
//! HOSTNAME: `wagmi.local`, CPU: `<UNKNOWN>`
//! EXECUTION: , WASM-EXECUTION: Compiled, CHAIN: Some("dev-3"), DB CACHE: 1024

// Executed Command:
// ./target/debug/chainflip-node
// benchmark
// pallet
// --pallet
// pallet_cf_account_roles
// --extrinsic
// *
// --output
// state-chain/pallets/cf-account-roles/src/weights.rs
// --steps=2
// --repeat=2
// --template=state-chain/chainflip-weight-template.hbs
// --chain=dev-3

#![cfg_attr(rustfmt, rustfmt_skip)]
#![allow(unused_parens)]
#![allow(unused_imports)]
#![allow(missing_docs)]

use frame_support::{traits::Get, weights::{Weight, constants::RocksDbWeight}};
use core::marker::PhantomData;

/// Weight functions needed for pallet_cf_account_roles.
pub trait WeightInfo {
	fn noop() -> Weight;
}

/// Weights for pallet_cf_account_roles using the Substrate node and recommended hardware.
pub struct PalletWeight<T>(PhantomData<T>);
impl<T: frame_system::Config> WeightInfo for PalletWeight<T> {
	fn noop() -> Weight {
		// Proof Size summary in bytes:
		//  Measured:  `0`
		//  Estimated: `0`
		// Minimum execution time: 4_000_000 picoseconds.
		Weight::from_parts(4_000_000, 0)
	}
}

// For backwards compatibility and tests
impl WeightInfo for () {
	fn noop() -> Weight {
		// Proof Size summary in bytes:
		//  Measured:  `0`
		//  Estimated: `0`
		// Minimum execution time: 4_000_000 picoseconds.
		Weight::from_parts(4_000_000, 0)
	}
}
