use crate::{address::ForeignChainAddress, Chain, ChannelIdConstructor};
use cf_primitives::{
	chains::{assets, AnyChain},
	AssetAmount,
};

impl Chain for AnyChain {
	const NAME: &'static str = "AnyChain";
	type ChainBlockNumber = u64;
	type ChainAmount = AssetAmount;
	type TransactionFee = Self::ChainAmount;
	type TrackedData = ();
	type ChainAsset = assets::any::Asset;
	type ChainAccount = ForeignChainAddress;
	type EpochStartData = ();
	type DepositFetchId = ();
}

impl ChannelIdConstructor for () {
	type Address = ForeignChainAddress;

	fn deployed(_channel_id: u64, _address: Self::Address) -> Self {
		unreachable!()
	}

	fn undeployed(_channel_id: u64, _address: Self::Address) -> Self {
		unreachable!()
	}
}
