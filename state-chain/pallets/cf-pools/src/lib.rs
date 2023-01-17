#![cfg_attr(not(feature = "std"), no_std)]
use cf_primitives::{chains::assets::any, AssetAmount, ExchangeRate, PoolAsset, TradingPosition};
use cf_traits::Chainflip;
use frame_support::{
	pallet_prelude::*,
	sp_runtime::{traits::Saturating, FixedPointNumber, Permill},
};
use frame_system::pallet_prelude::OriginFor;
pub use pallet::*;
use sp_arithmetic::traits::Zero;

const BASIS_POINTS_PER_MILLION: u32 = 100;

#[cfg(test)]
mod mock;

#[cfg(test)]
mod tests;

pub(crate) mod mini_pool {
	use super::*;

	#[derive(
		Copy, Clone, Debug, Default, PartialEq, Eq, Encode, Decode, MaxEncodedLen, TypeInfo,
	)]
	pub struct AmmPool {
		asset_0: AssetAmount,
		asset_1: AssetAmount,
	}

	impl AmmPool {
		pub fn get_liquidity(&self) -> (AssetAmount, AssetAmount) {
			(self.asset_0, self.asset_1)
		}

		pub fn add_liquidity(&mut self, volume_0: AssetAmount, volume_1: AssetAmount) {
			self.asset_0.saturating_accrue(volume_0);
			self.asset_1.saturating_accrue(volume_1);
		}

		pub fn remove_liquidity(
			&mut self,
			volume_0: AssetAmount,
			volume_1: AssetAmount,
		) -> (AssetAmount, AssetAmount) {
			let (asset_0_liquidity, asset_1_liquidity) = self.get_liquidity();
			self.asset_0.saturating_reduce(volume_0);
			self.asset_1.saturating_reduce(volume_1);
			(
				asset_0_liquidity.saturating_sub(self.asset_0),
				asset_1_liquidity.saturating_sub(self.asset_1),
			)
		}

		pub fn swap_rate(&self, input_amount: AssetAmount) -> ExchangeRate {
			ExchangeRate::from_rational(self.asset_1, self.asset_0 + input_amount)
		}

		pub fn swap(&mut self, input_amount: AssetAmount) -> AssetAmount {
			let output_amount = self.swap_rate(input_amount).saturating_mul_int(input_amount);
			self.asset_0.saturating_accrue(input_amount);
			self.asset_1.saturating_reduce(output_amount);
			output_amount
		}

		pub fn reverse_swap(&mut self, input_amount: AssetAmount) -> AssetAmount {
			self.in_reverse(|reversed| reversed.swap(input_amount))
		}

		pub fn reversed(self) -> Self {
			Self { asset_0: self.asset_1, asset_1: self.asset_0 }
		}

		fn in_reverse<R, F: FnOnce(&mut Self) -> R>(&mut self, f: F) -> R {
			let mut reversed = self.reversed();
			let r = f(&mut reversed);
			*self = reversed.reversed();
			r
		}
	}
}

#[frame_support::pallet]
pub mod pallet {
	use cf_primitives::Asset;
	use frame_system::pallet_prelude::BlockNumberFor;

	use super::*;

	#[pallet::config]
	#[pallet::disable_frame_system_supertrait_check]
	pub trait Config: Chainflip {
		/// The event type.
		type RuntimeEvent: From<Event<Self>> + IsType<<Self as frame_system::Config>::RuntimeEvent>;
		#[pallet::constant]
		type NetworkFee: Get<u16>;
		/// Implementation of EnsureOrigin trait for governance
		type EnsureGovernance: EnsureOrigin<Self::RuntimeOrigin>;
	}

	#[pallet::event]
	#[pallet::generate_deposit(pub(super) fn deposit_event)]
	pub enum Event<T: Config> {
		/// Updated the buy interval.
		UpdatedBuyInterval { buy_interval: T::BlockNumber },
	}

	#[pallet::error]
	pub enum Error<T> {
		/// Setting the buy interval to zero is not allowed.
		ZeroBuyIntervalNotAllowed,
	}

	#[pallet::call]
	impl<T: Config> Pallet<T> {
		/// Updates the buy interval.
		///
		/// ## Events
		///
		/// - [UpdatedBuyInterval](Event::UpdatedBuyInterval)
		///
		/// ## Errors
		///
		/// - [BadOrigin](frame_system::BadOrigin)
		/// - [ZeroBuyIntervalNotAllowed](pallet_cf_pools::Error::ZeroBuyIntervalNotAllowed)
		#[pallet::weight(0)]
		pub fn update_buy_interval(
			origin: OriginFor<T>,
			new_buy_interval: T::BlockNumber,
		) -> DispatchResultWithPostInfo {
			T::EnsureGovernance::ensure_origin(origin)?;
			ensure!(new_buy_interval != Zero::zero(), Error::<T>::ZeroBuyIntervalNotAllowed);
			FlipBuyInterval::<T>::set(new_buy_interval);
			Self::deposit_event(Event::<T>::UpdatedBuyInterval { buy_interval: new_buy_interval });
			Ok(().into())
		}
	}

	#[pallet::pallet]
	#[pallet::without_storage_info]
	#[pallet::generate_store(pub(super) trait Store)]
	pub struct Pallet<T>(PhantomData<T>);

	#[pallet::genesis_config]
	pub struct GenesisConfig<T: Config> {
		pub flip_buy_interval: T::BlockNumber,
	}

	#[pallet::genesis_build]
	impl<T: Config> GenesisBuild<T> for GenesisConfig<T> {
		fn build(&self) {
			FlipBuyInterval::<T>::set(T::BlockNumber::from(1_u32));
		}
	}

	#[cfg(feature = "std")]
	impl<T: Config> Default for GenesisConfig<T> {
		fn default() -> Self {
			Self { flip_buy_interval: T::BlockNumber::from(1_u32) }
		}
	}

	/// Pools are indexed by single asset since USDC is implicit.
	#[pallet::storage]
	pub(super) type Pools<T: Config> =
		StorageMap<_, Twox64Concat, any::Asset, mini_pool::AmmPool, ValueQuery>;

	/// FLIP ready to be burned.
	#[pallet::storage]
	pub(super) type FlipToBurn<T: Config> = StorageValue<_, AssetAmount, ValueQuery>;

	/// Interval at which we buy FLIP in order to burn it.
	#[pallet::storage]
	pub(super) type FlipBuyInterval<T: Config> = StorageValue<_, T::BlockNumber, ValueQuery>;

	/// Network fee
	#[pallet::storage]
	pub type CollectedNetworkFee<T: Config> = StorageValue<_, AssetAmount, ValueQuery>;

	#[pallet::hooks]
	impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {
		fn on_initialize(current_block: BlockNumberFor<T>) -> Weight {
			// Note: FlipBuyInterval is never zero!
			if current_block % FlipBuyInterval::<T>::get() == Zero::zero() &&
				CollectedNetworkFee::<T>::get() != 0
			{
				let flip_to_burn = Pools::<T>::mutate(Asset::Flip, |pool| {
					pool.reverse_swap(CollectedNetworkFee::<T>::take())
				});
				FlipToBurn::<T>::mutate(|total| {
					*total = total.saturating_add(flip_to_burn);
				});
			}
			Weight::from_ref_time(0)
		}
	}
}

impl<T: Config> cf_traits::SwappingApi for Pallet<T> {
	fn swap(
		from: any::Asset,
		to: any::Asset,
		input_amount: AssetAmount,
		_fee: u16,
	) -> (AssetAmount, (cf_primitives::Asset, AssetAmount)) {
		(
			match (from, to) {
				(input_asset, any::Asset::Usdc) => {
					let swap_output =
						Pools::<T>::mutate(input_asset, |pool| pool.swap(input_amount));
					Self::take_network_fee(swap_output)
				},
				(any::Asset::Usdc, output_asset) => Pools::<T>::mutate(output_asset, |pool| {
					pool.reverse_swap(Self::take_network_fee(input_amount))
				}),
				(input_asset, output_asset) => Pools::<T>::mutate(output_asset, |pool| {
					pool.reverse_swap(Self::take_network_fee(Pools::<T>::mutate(
						input_asset,
						|pool| pool.swap(input_amount),
					)))
				}),
			},
			(any::Asset::Usdc, 0),
		)
	}
}

impl<T: Config> cf_traits::LiquidityPoolApi for Pallet<T> {
	const STABLE_ASSET: any::Asset = any::Asset::Usdc;

	fn deploy(asset: &any::Asset, position: cf_primitives::TradingPosition<AssetAmount>) {
		match position {
			TradingPosition::ClassicV3 { volume_0, volume_1, .. } => {
				Pools::<T>::mutate(asset, |pool| pool.add_liquidity(volume_0, volume_1));
			},
			TradingPosition::VolatileV3 { side, volume, .. } => {
				Pools::<T>::mutate(asset, |pool| match side {
					PoolAsset::Asset0 => pool.add_liquidity(volume, 0),
					PoolAsset::Asset1 => pool.add_liquidity(0, volume),
				});
			},
		}
	}

	fn retract(
		asset: &any::Asset,
		position: cf_primitives::TradingPosition<AssetAmount>,
	) -> (AssetAmount, AssetAmount) {
		match position {
			TradingPosition::ClassicV3 { volume_0, volume_1, .. } =>
				Pools::<T>::mutate(asset, |pool| pool.remove_liquidity(volume_0, volume_1)),
			TradingPosition::VolatileV3 { side, volume, .. } =>
				Pools::<T>::mutate(asset, |pool| match side {
					PoolAsset::Asset0 => pool.remove_liquidity(volume, 0),
					PoolAsset::Asset1 => pool.remove_liquidity(0, volume),
				}),
		}
	}

	fn get_liquidity(asset: &any::Asset) -> (AssetAmount, AssetAmount) {
		Pools::<T>::get(asset).get_liquidity()
	}

	fn swap_rate(
		input_asset: any::Asset,
		output_asset: any::Asset,
		input_amount: AssetAmount,
	) -> ExchangeRate {
		if input_amount == 0 {
			match (input_asset, output_asset) {
				(input_asset, any::Asset::Usdc) => Pools::<T>::get(input_asset).swap_rate(0),
				(any::Asset::Usdc, output_asset) =>
					Pools::<T>::get(output_asset).reversed().swap_rate(0),
				(input_asset, output_asset) => {
					let rate_1 = Pools::<T>::get(input_asset).swap_rate(0);
					let rate_2 = Pools::<T>::get(output_asset).reversed().swap_rate(0);
					rate_1 * rate_2
				},
			}
		} else {
			let output_amount = match (input_asset, output_asset) {
				(input_asset, any::Asset::Usdc) => Pools::<T>::get(input_asset).swap(input_amount),
				(any::Asset::Usdc, output_asset) =>
					Pools::<T>::get(output_asset).reverse_swap(input_amount),
				(input_asset, output_asset) => Pools::<T>::get(output_asset)
					.reverse_swap(Pools::<T>::get(input_asset).swap(input_amount)),
			};
			ExchangeRate::from_rational(output_amount, input_amount)
		}
	}

	fn get_liquidity_amount_by_position(
		_asset: &any::Asset,
		position: &TradingPosition<AssetAmount>,
	) -> Option<(AssetAmount, AssetAmount)> {
		// Naive placeholder implementation. Does not take account into existing liquidity in the
		// pool.
		Some(match position {
			TradingPosition::ClassicV3 { volume_0, volume_1, .. } => (*volume_0, *volume_1),
			TradingPosition::VolatileV3 { side, volume, .. } => match side {
				PoolAsset::Asset0 => (*volume, 0u128),
				PoolAsset::Asset1 => (0u128, *volume),
			},
		})
	}
}

impl<T: Config> cf_traits::FlipBurnInfo for Pallet<T> {
	fn take_flip_to_burn() -> AssetAmount {
		FlipToBurn::<T>::take()
	}
}

impl<T: Config> Pallet<T> {
	fn calc_fee(fee: u16, input: AssetAmount) -> AssetAmount {
		Permill::from_parts(fee as u32 * BASIS_POINTS_PER_MILLION) * input
	}

	fn take_network_fee(input: AssetAmount) -> AssetAmount {
		let fee = Self::calc_fee(T::NetworkFee::get(), input);
		CollectedNetworkFee::<T>::mutate(|total| {
			*total = total.saturating_add(fee);
		});
		input.saturating_sub(fee)
	}
}
