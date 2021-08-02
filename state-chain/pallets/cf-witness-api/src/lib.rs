#![cfg_attr(not(feature = "std"), no_std)]

//! Witness Api Pallet
//!
//! A collection of convenience extrinsics that delegate to other pallets via witness consensus.

pub use pallet::*;

#[cfg(test)]
mod mock;

#[cfg(test)]
mod tests;

#[cfg(feature = "runtime-benchmarks")]
mod benchmarking;

#[frame_support::pallet]
pub mod pallet {
	use cf_traits::Witnesser;
	use frame_support::{dispatch::DispatchResultWithPostInfo, pallet_prelude::*};
	use frame_system::pallet_prelude::*;
	use pallet_cf_staking::{
		Call as StakingCall, Config as StakingConfig, EthTransactionHash, EthereumAddress,
		FlipBalance,
	};
	use pallet_cf_vaults::chains::ethereum::{Call as EthereumCall, Config as EthereumConfig, EthSigningTxResponse};
	use pallet_cf_vaults::{Call as VaultsCall, Config as VaultsConfig};
	use pallet_cf_vaults::rotation::{KeygenResponse, VaultRotationResponse};

	type AccountId<T> = <T as frame_system::Config>::AccountId;
	type RequestIndexFor<T> = <T as pallet_cf_vaults::Config>::RequestIndex;
	type PublicKeyFor<T> = <T as pallet_cf_vaults::Config>::PublicKey;

	#[pallet::config]
	pub trait Config: frame_system::Config + StakingConfig + VaultsConfig + EthereumConfig {
		/// Standard Call type. We need this so we can use it as a constraint in `Witnesser`.
		type Call: IsType<<Self as frame_system::Config>::Call>
			+ From<StakingCall<Self>>
			+ From<VaultsCall<Self>>
			+ From<EthereumCall<Self>>;

		/// An implementation of the witnesser, allows us to define our witness_* helper extrinsics.
		type Witnesser: Witnesser<Call = <Self as Config>::Call, AccountId = AccountId<Self>>;
	}

	#[pallet::pallet]
	pub struct Pallet<T>(_);

	#[pallet::hooks]
	impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {}

	#[pallet::call]
	impl<T: Config> Pallet<T> {
		//*** Staking pallet witness calls ***//

		/// Witness that a `Staked` event was emitted by the `StakeManager` smart contract.
		///
		/// This is a convenience extrinsic that simply delegates to the configured witnesser.
		#[pallet::weight(10_000)]
		pub fn witness_staked(
			origin: OriginFor<T>,
			staker_account_id: AccountId<T>,
			amount: FlipBalance<T>,
			withdrawal_address: Option<EthereumAddress>,
			tx_hash: EthTransactionHash,
		) -> DispatchResultWithPostInfo {
			let who = ensure_signed(origin)?;
			let call = StakingCall::staked(staker_account_id, amount, withdrawal_address, tx_hash);
			T::Witnesser::witness(who, call.into())
		}

		/// Witness that a `Claimed` event was emitted by the `StakeManager` smart contract.
		///
		/// This is a convenience extrinsic that simply delegates to the configured witnesser.
		#[pallet::weight(10_000)]
		pub fn witness_claimed(
			origin: OriginFor<T>,
			account_id: AccountId<T>,
			claimed_amount: FlipBalance<T>,
			tx_hash: EthTransactionHash,
		) -> DispatchResultWithPostInfo {
			let who = ensure_signed(origin)?;
			let call = StakingCall::claimed(account_id, claimed_amount, tx_hash);
			T::Witnesser::witness(who, call.into())
		}

		/// Witness that a key generation response from 2/3 of our old validators
		///
		/// This is a convenience extrinsic that simply delegates to the configured witnesser.
		#[pallet::weight(10_000)]
		pub fn witness_keygen_response(
			origin: OriginFor<T>,
			request_id: RequestIndexFor<T>,
			response: KeygenResponse<T::ValidatorId, PublicKeyFor<T>>,
		) -> DispatchResultWithPostInfo {
			let who = ensure_signed(origin)?;
			let call = VaultsCall::keygen_response(request_id, response);
			T::Witnesser::witness(who, call.into())
		}

		/// Witness that a vault rotation response from 2/3 of our old validators
		///
		/// This is a convenience extrinsic that simply delegates to the configured witnesser.
		#[pallet::weight(10_000)]
		pub fn witness_vault_rotation_response(
			origin: OriginFor<T>,
			request_id: RequestIndexFor<T>,
			response: VaultRotationResponse,
		) -> DispatchResultWithPostInfo {
			let who = ensure_signed(origin)?;
			let call = VaultsCall::vault_rotation_response(request_id, response);
			T::Witnesser::witness(who, call.into())
		}

		#[pallet::weight(10_000)]
		pub fn witness_eth_signing_tx_response(
			origin: OriginFor<T>,
			request_id: <T as pallet_cf_vaults::chains::ethereum::Config>::RequestIndex,
			response: EthSigningTxResponse<T::ValidatorId>,
		) -> DispatchResultWithPostInfo {
			let who = ensure_signed(origin)?;
			let call = EthereumCall::eth_signing_tx_response(request_id, response);
			T::Witnesser::witness(who, call.into())
		}
	}
}
