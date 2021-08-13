//! # Ethereum Vault Module
//!
//! A module for the Ethereum vault
//!
//! - [`Config`]
//! - [`Call`]
//! - [`Module`]
//!
//! ## Overview
//! The module contains functionality to manage the rotation of the Ethereum vault.  It has a dependency
//! on the `Vaults` pallet and is treated as submodule of this pallet allowing specialisation.
//! A request to sign a payload is created on calling `ChainVault::try_start_vault_rotation()` and
//! emitted as an event to the network for signing.  A response is required with either a signature
//! or a failure reported with which the result is reported back via `ChainHandler::try_complete_vault_rotation()`
//! and the process is passed back to the `Vaults` pallet.
//! Final execution of the vault rotation is reported back via `vault_rotated()` and the response
//! `VaultRotationResponse` is put to storage.
//!
//! ## Terminology
//! - **Vaults** A ChainFlip pallet that delegates certain chain specific vault rotation duties to this
//!   pallet.
//! - **Vault:** A cryptocurrency wallet.
//! - **Validators:** A set of nodes that validate and support the ChainFlip network.
//! - **Bad Validators:** A set of nodes that have acted badly, the determination of what bad is is
//!   outside the scope of the `Vaults` pallet.
//! - **Key generation:** The process of creating a new key pair which would be used for operating a vault.
//! - **Auction:** A process by which a set of validators are proposed and on successful vault rotation
//!   become the next validating set for the network.
//! - **Vault Rotation:** The rotation of vaults where funds are 'moved' from one to another.

use crate::rotation::ChainParams::Ethereum;
use crate::rotation::*;
use cf_traits::NonceProvider;
use ethabi::{Bytes, Function, Param, ParamType, Token};
use frame_support::pallet_prelude::*;
pub use pallet::*;
use sp_core::{H160, U256};
use sp_std::prelude::*;

/// A signing request
#[derive(PartialEq, Eq, Clone, Encode, Decode, RuntimeDebug)]
pub struct EthSigningTxRequest<ValidatorId> {
	// Payload to be signed by the existing aggregate key
	pub(crate) payload: Vec<u8>,
	pub(crate) validators: Vec<ValidatorId>,
}

/// A response back with our signature else a list of bad validators
#[derive(PartialEq, Eq, Clone, Encode, Decode, RuntimeDebug)]
pub enum EthSigningTxResponse<ValidatorId> {
	// Signature
	Success(Vec<u8>),
	// Bad validators
	Error(Vec<ValidatorId>),
}

#[frame_support::pallet]
pub mod pallet {
	use super::*;
	use cf_traits::NonceProvider;
	use frame_system::pallet_prelude::*;
	use sp_runtime::traits::AtLeast32BitUnsigned;
	#[pallet::pallet]
	#[pallet::generate_store(pub (super) trait Store)]
	pub struct Pallet<T>(_);

	#[pallet::config]
	pub trait Config: frame_system::Config + ChainFlip {
		/// The event type
		type Event: From<Event<Self>> + IsType<<Self as frame_system::Config>::Event>;
		type Vaults: ChainHandler<
			ValidatorId = <Self as ChainFlip>::ValidatorId,
			Error = RotationError<Self::ValidatorId>,
		>;
		/// Provides an origin check for witness transactions.
		type EnsureWitnessed: EnsureOrigin<Self::Origin>;
		/// The request index
		type RequestIndex: Member + Parameter + Default + AtLeast32BitUnsigned + Copy;
		/// The new public key type
		type PublicKey: Into<Vec<u8>> + Member + Parameter + Default;
		/// A nonce
		type Nonce: Into<U256>;
		/// A nonce provider
		type NonceProvider: NonceProvider<Nonce = Self::Nonce>;
	}

	/// Pallet implements [`Hooks`] trait
	#[pallet::hooks]
	impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {}

	/// The Vault for this instance
	#[pallet::storage]
	#[pallet::getter(fn vault)]
	pub(super) type Vault<T: Config> =
		StorageValue<_, VaultRotationResponse<T::PublicKey>, ValueQuery>;

	#[pallet::event]
	#[pallet::generate_deposit(pub (super) fn deposit_event)]
	pub enum Event<T: Config> {
		/// Request this payload to be signed by the existing aggregate key
		EthSignTxRequestEvent(RequestIndex, EthSigningTxRequest<T::ValidatorId>),
	}

	#[pallet::error]
	pub enum Error<T> {
		EthSigningTxResponseFailed,
	}

	#[pallet::call]
	impl<T: Config> Pallet<T> {
		#[pallet::weight(10_000)]
		pub fn eth_signing_tx_response(
			origin: OriginFor<T>,
			request_id: RequestIndex,
			response: EthSigningTxResponse<T::ValidatorId>,
		) -> DispatchResultWithPostInfo {
			T::EnsureWitnessed::ensure_origin(origin)?;
			match Self::try_response(request_id, response) {
				Ok(_) => Ok(().into()),
				Err(_) => Err(Error::<T>::EthSigningTxResponseFailed.into()),
			}
		}
	}

	#[pallet::genesis_config]
	pub struct GenesisConfig {}

	#[cfg(feature = "std")]
	impl Default for GenesisConfig {
		fn default() -> Self {
			Self {}
		}
	}

	// The build of genesis for the pallet.
	#[pallet::genesis_build]
	impl<T: Config> GenesisBuild<T> for GenesisConfig {
		fn build(&self) {}
	}
}

impl<T: Config> ChainVault for Pallet<T> {
	type Bytes = T::PublicKey;
	type ValidatorId = T::ValidatorId;
	type Error = RotationError<T::ValidatorId>;

	/// Parameters required when creating key generation requests
	fn chain_params() -> ChainParams {
		ChainParams::Ethereum(vec![])
	}

	/// The initial phase has completed with success and we are notified of this from `Vaults`.
	/// Now the specifics for this chain/vault are processed.  In the case for Ethereum we request
	/// to have the function `setAggKeyWithAggKey` signed by the old set of validators.
	/// A payload is built and emitted as a `EthSigningTxRequest`, failing this an error is reported
	/// back to `Vaults`
	fn try_start_vault_rotation(
		index: RequestIndex,
		new_public_key: Self::Bytes,
		validators: Vec<Self::ValidatorId>,
	) -> Result<(), Self::Error> {
		// Create payload for signature here
		// function setAggKeyWithAggKey(SigData calldata sigData, Key calldata newKey)
		match Self::encode_set_agg_key_with_agg_key(new_public_key) {
			Ok(payload) => {
				// Emit the event
				Self::try_request(
					index,
					EthSigningTxRequest {
						validators,
						payload,
					},
				)
			}
			Err(_) => {
				// Failure in completing the vault rotation and we report back to `Vaults`
				T::Vaults::try_complete_vault_rotation(
					index,
					Err(RotationError::FailedToConstructPayload),
				)
			}
		}
	}

	/// The vault for this chain has been rotated and we store this response to storage
	fn vault_rotated(response: VaultRotationResponse<Self::Bytes>) {
		Vault::<T>::set(response);
	}
}

impl<T: Config>
	RequestResponse<
		RequestIndex,
		EthSigningTxRequest<T::ValidatorId>,
		EthSigningTxResponse<T::ValidatorId>,
		RotationError<T::ValidatorId>,
	> for Pallet<T>
{
	/// Make the request to sign by emitting an event
	fn try_request(
		index: RequestIndex,
		request: EthSigningTxRequest<T::ValidatorId>,
	) -> Result<(), RotationError<T::ValidatorId>> {
		Self::deposit_event(Event::EthSignTxRequestEvent(index, request));
		Ok(().into())
	}

	/// Try to handle the response and pass this onto `Vaults` to complete the vault rotation
	fn try_response(
		index: RequestIndex,
		response: EthSigningTxResponse<T::ValidatorId>,
	) -> Result<(), RotationError<T::ValidatorId>> {
		match response {
			EthSigningTxResponse::Success(signature) => {
				T::Vaults::try_complete_vault_rotation(index, Ok(Ethereum(signature).into()))
			}
			EthSigningTxResponse::Error(bad_validators) => T::Vaults::try_complete_vault_rotation(
				index,
				Err(RotationError::BadValidators(bad_validators)),
			),
		}
	}
}

impl From<Vec<u8>> for ChainParams {
	fn from(payload: Vec<u8>) -> Self {
		Ethereum(payload)
	}
}

impl<T: Config> Pallet<T> {
	/// Encode `setAggKeyWithAggKey` call using `ethabi`.  This is a long approach as we are working
	/// around `no_std` limitations here for the runtime.
	pub(crate) fn encode_set_agg_key_with_agg_key(
		new_public_key: T::PublicKey,
	) -> ethabi::Result<Bytes> {
		Function::new(
			"setAggKeyWithAggKey",
			vec![
				Param::new(
					"sigData",
					ParamType::Tuple(vec![
						ParamType::Uint(256),
						ParamType::Uint(256),
						ParamType::Uint(256),
						ParamType::Address,
					]),
				),
				Param::new("newKey", ParamType::FixedBytes(32)),
			],
			vec![],
			false,
		)
		.encode_input(&vec![
			// sigData: SigData(uint, uint, uint, address)
			Token::Tuple(vec![
				Token::Uint(ethabi::Uint::zero()),
				Token::Uint(ethabi::Uint::zero()),
				Token::Uint(T::NonceProvider::generate_nonce().into()),
				Token::Address(H160::zero()),
			]),
			// newKey: bytes32
			Token::FixedBytes(new_public_key.into()),
		])
	}
}
