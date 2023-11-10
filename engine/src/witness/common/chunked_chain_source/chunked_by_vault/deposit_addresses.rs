use std::sync::Arc;

use cf_chains::ChainState;
use frame_support::CloneNoBound;
use futures::FutureExt;
use futures_util::{stream, StreamExt};
use pallet_cf_ingress_egress::DepositChannelDetails;
use state_chain_runtime::PalletInstanceAlias;
use tokio::sync::watch;
use utilities::{
	loop_select,
	task_scope::{Scope, OR_CANCEL},
};

use crate::{
	state_chain_observer::client::{storage_api::StorageApi, StateChainStreamApi},
	witness::common::{
		chain_source::{ChainClient, ChainStream, Header},
		RuntimeHasChain, STATE_CHAIN_BEHAVIOUR, STATE_CHAIN_CONNECTION,
	},
};

use super::{builder::ChunkedByVaultBuilder, ChunkedByVault};

pub type Addresses<Inner> = Vec<
	DepositChannelDetails<
		state_chain_runtime::Runtime,
		<<Inner as ChunkedByVault>::Chain as PalletInstanceAlias>::Instance,
	>,
>;

/// This helps ensure the set of deposit addresses witnessed at each block are consistent across
/// every validator

#[derive(Clone)]
#[allow(clippy::type_complexity)]
pub struct DepositAddresses<Inner: ChunkedByVault>
where
	state_chain_runtime::Runtime: RuntimeHasChain<Inner::Chain>,
{
	inner: Inner,
	receiver: tokio::sync::watch::Receiver<(ChainState<Inner::Chain>, Addresses<Inner>)>,
}
impl<Inner: ChunkedByVault> DepositAddresses<Inner>
where
	state_chain_runtime::Runtime: RuntimeHasChain<Inner::Chain>,
{
	// We wait for the chain_tracking to pass a blocks height before assessing the addresses that
	// should be witnessed at that block to ensure, the set of addresses each engine attempts to
	// witness at a given block is consistent.
	// We ensure the index is strictly less than the block height. This is because we need to ensure
	// that for a particular chain state block height, no more deposit channels can be created with
	// that opened_at block height.
	fn is_header_ready(index: Inner::Index, chain_state: &ChainState<Inner::Chain>) -> bool {
		index < chain_state.block_height
	}

	// FOr a given header we only witness addresses opened at or before the header, the set of
	// addresses each engine attempts to witness at a given block is consistent
	fn addresses_for_header(index: Inner::Index, addresses: &Addresses<Inner>) -> Addresses<Inner> {
		addresses
			.iter()
			.filter(|details| details.opened_at <= index && index <= details.expires_at)
			.cloned()
			.collect()
	}

	async fn get_chain_state_and_addresses<StateChainClient: StorageApi + Send + Sync + 'static>(
		state_chain_client: &StateChainClient,
		block_hash: state_chain_runtime::Hash,
	) -> (ChainState<Inner::Chain>, Addresses<Inner>)
	where
		state_chain_runtime::Runtime: RuntimeHasChain<Inner::Chain>,
	{
		(
			state_chain_client
				.storage_value::<pallet_cf_chain_tracking::CurrentChainState<
					state_chain_runtime::Runtime,
					<Inner::Chain as PalletInstanceAlias>::Instance,
				>>(block_hash)
				.await
				.expect(STATE_CHAIN_CONNECTION)
				.expect(STATE_CHAIN_BEHAVIOUR),
			state_chain_client
				.storage_map_values::<pallet_cf_ingress_egress::DepositChannelLookup<
					state_chain_runtime::Runtime,
					<Inner::Chain as PalletInstanceAlias>::Instance,
				>>(block_hash)
				.await
				.expect(STATE_CHAIN_CONNECTION),
		)
	}

	pub async fn new<
		'env,
		StateChainStream: StateChainStreamApi<FINALIZED>,
		StateChainClient: StorageApi + Send + Sync + 'static,
		const FINALIZED: bool,
	>(
		inner: Inner,
		scope: &Scope<'env, anyhow::Error>,
		mut state_chain_stream: StateChainStream,
		state_chain_client: Arc<StateChainClient>,
	) -> Self
	where
		state_chain_runtime::Runtime: RuntimeHasChain<Inner::Chain>,
	{
		let (sender, receiver) = watch::channel(
			Self::get_chain_state_and_addresses(
				&*state_chain_client,
				state_chain_stream.cache().hash,
			)
			.await,
		);

		scope.spawn(async move {
            utilities::loop_select! {
                let _ = sender.closed() => { break Ok(()) },
                if let Some(_block_header) = state_chain_stream.next() => {
					// Note it is still possible for engines to inconsistently select addresses to witness for a block due to how the SC expiries deposit addresses
                    let _result = sender.send(Self::get_chain_state_and_addresses(&*state_chain_client, state_chain_stream.cache().hash).await);
                } else break Ok(()),
            }
        });

		Self { inner, receiver }
	}
}
#[async_trait::async_trait]
impl<Inner: ChunkedByVault> ChunkedByVault for DepositAddresses<Inner>
where
	state_chain_runtime::Runtime: RuntimeHasChain<Inner::Chain>,
{
	type ExtraInfo = Inner::ExtraInfo;
	type ExtraHistoricInfo = Inner::ExtraHistoricInfo;

	type Index = Inner::Index;
	type Hash = Inner::Hash;
	type Data = (Inner::Data, Addresses<Inner>);

	type Client = DepositAddressesClient<Inner>;

	type Chain = Inner::Chain;

	type Parameters = Inner::Parameters;

	async fn stream(
		&self,
		parameters: Self::Parameters,
	) -> crate::witness::common::BoxActiveAndFuture<'_, super::Item<'_, Self>> {
		self.inner
			.stream(parameters)
			.await
			.then(move |(epoch, chain_stream, chain_client)| async move {
				struct State<Inner: ChunkedByVault> where
				state_chain_runtime::Runtime: RuntimeHasChain<Inner::Chain> {
					receiver:
						tokio::sync::watch::Receiver<(ChainState<Inner::Chain>, Addresses<Inner>)>,
					pending_headers: Vec<Header<Inner::Index, Inner::Hash, Inner::Data>>,
					ready_headers:
						Vec<Header<Inner::Index, Inner::Hash, (Inner::Data, Addresses<Inner>)>>,
				}
				impl<Inner: ChunkedByVault> State<Inner> where
				state_chain_runtime::Runtime: RuntimeHasChain<Inner::Chain> {
					fn add_headers<
						It: IntoIterator<Item = Header<Inner::Index, Inner::Hash, Inner::Data>>,
					>(
						&mut self,
						headers: It,
					) {
						let chain_state_and_addresses = self.receiver.borrow();
						let (chain_state, addresses) = &*chain_state_and_addresses;
						for header in headers {
							if DepositAddresses::<Inner>::is_header_ready(
								header.index,
								chain_state,
							) {
								self.ready_headers.push(header.map_data(|header| {
									(
										header.data,
										DepositAddresses::<Inner>::addresses_for_header(
											header.index,
											addresses,
										),
									)
								}));
							} else {
								self.pending_headers.push(header);
							}
						}
					}
				}

				(
					epoch,
					stream::unfold(
						(
							chain_stream.fuse(),
							State::<Inner> {
								receiver: self.receiver.clone(),
								pending_headers: vec![],
								ready_headers: vec![],
							}
						),
						|(mut chain_stream, mut state)| async move {
							loop_select!(
								if !state.ready_headers.is_empty() => break Some((state.ready_headers.pop().unwrap(), (chain_stream, state))),
								if let Some(header) = chain_stream.next() => {
									state.add_headers(std::iter::once(header));
								} else disable then if state.pending_headers.is_empty() => break None,
								let _ = state.receiver.changed().map(|result| result.expect(OR_CANCEL)) => {
									let pending_headers = std::mem::take(&mut state.pending_headers);
									state.add_headers(pending_headers);
								},
							)
						},
					)
					.into_box(),
					DepositAddressesClient::new(chain_client, self.receiver.clone()),
				)
			})
			.await
			.into_box()
	}
}

#[derive(CloneNoBound)]
pub struct DepositAddressesClient<Inner: ChunkedByVault>
where
	state_chain_runtime::Runtime: RuntimeHasChain<Inner::Chain>,
{
	inner_client: Inner::Client,
	receiver: tokio::sync::watch::Receiver<(ChainState<Inner::Chain>, Addresses<Inner>)>,
}

impl<Inner: ChunkedByVault> DepositAddressesClient<Inner>
where
	state_chain_runtime::Runtime: RuntimeHasChain<Inner::Chain>,
{
	pub fn new(
		inner_client: Inner::Client,
		receiver: tokio::sync::watch::Receiver<(ChainState<Inner::Chain>, Addresses<Inner>)>,
	) -> Self {
		Self { inner_client, receiver }
	}
}
#[async_trait::async_trait]
impl<Inner: ChunkedByVault> ChainClient for DepositAddressesClient<Inner>
where
	state_chain_runtime::Runtime: RuntimeHasChain<Inner::Chain>,
{
	type Index = Inner::Index;
	type Hash = Inner::Hash;
	type Data = (Inner::Data, Addresses<Inner>);

	async fn header_at_index(
		&self,
		index: Self::Index,
	) -> Header<Self::Index, Self::Hash, Self::Data> {
		let mut receiver = self.receiver.clone();

		let addresses = {
			let chain_state_and_addresses = receiver
				.wait_for(|(chain_state, _addresses)| {
					DepositAddresses::<Inner>::is_header_ready(index, chain_state)
				})
				.await
				.expect(OR_CANCEL);
			let (_option_chain_state, addresses) = &*chain_state_and_addresses;
			DepositAddresses::<Inner>::addresses_for_header(index, addresses)
		};

		self.inner_client
			.header_at_index(index)
			.await
			.map_data(|header| (header.data, addresses))
	}
}

impl<Inner: ChunkedByVault> ChunkedByVaultBuilder<Inner> {
	pub async fn deposit_addresses<
		'env,
		StateChainStream,
		StateChainClient,
		const FINALIZED: bool,
	>(
		self,
		scope: &Scope<'env, anyhow::Error>,
		state_chain_stream: StateChainStream,
		state_chain_client: Arc<StateChainClient>,
	) -> ChunkedByVaultBuilder<DepositAddresses<Inner>>
	where
		state_chain_runtime::Runtime: RuntimeHasChain<Inner::Chain>,
		StateChainStream: StateChainStreamApi<FINALIZED>,
		StateChainClient: StorageApi + Send + Sync + 'static,
	{
		ChunkedByVaultBuilder::new(
			DepositAddresses::new(self.source, scope, state_chain_stream, state_chain_client).await,
			self.parameters,
		)
	}
}
