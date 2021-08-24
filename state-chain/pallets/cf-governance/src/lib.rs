#![cfg_attr(not(feature = "std"), no_std)]

//! # Chainflip governance
//!
//! ## Purpose
//!
//! This pallet implements the current Chainflip governance functionality. The purpose of this pallet is primarily
//! to provide the following capabilities:
//!
//! - Handle the set of governance members
//! - Handle submitting proposals
//! - Handle approving proposals
//! - Provide tools to implement governance secured extrinsic in other pallets
//!
//! ## Governance model
//!
//! The governance model is a simple approved system. Every member can propose an extrinsic, which is secured by
//! the EnsureGovernance implementation of the EnsureOrigin trait. Apart from that, every member is allowed to
//! approve a proposed governance extrinsic. If a proposal can raise 2/3 + 1 approvals, it's getting executed by
//! the system automatically. Moreover, every proposal has an expiry date. If a proposal is not able to raise
//! enough approvals in time, it gets dropped and won't be executed.
//!
//! note: For implementation details pls see the readme.

use codec::Decode;
use frame_support::traits::EnsureOrigin;
use frame_support::traits::UnfilteredDispatchable;
pub use pallet::*;
use sp_runtime::DispatchError;
use sp_std::ops::Add;
use sp_std::vec::Vec;

#[cfg(test)]
mod mock;
#[cfg(test)]
mod tests;
/// Implements the functionality of the Chainflip governance.
#[frame_support::pallet]
pub mod pallet {

	use frame_support::{
		dispatch::GetDispatchInfo,
		pallet_prelude::*,
		traits::{UnfilteredDispatchable, UnixTime},
	};

	use codec::{Encode, FullCodec};
	use frame_system::{pallet, pallet_prelude::*};
	use sp_std::boxed::Box;
	use sp_std::vec;
	use sp_std::vec::Vec;

	pub type ActiveProposal = (ProposalId, u64);
	/// Proposal struct
	#[derive(Encode, Decode, Clone, RuntimeDebug, Default, PartialEq, Eq)]
	pub struct Proposal<AccountId> {
		/// Encoded representation of a extrinsic
		pub call: OpaqueCall,
		/// Date of creation
		pub created: u64,
		/// Array of accounts which already approved the proposal
		pub approved: Vec<AccountId>,
	}

	type AccountId<T> = <T as frame_system::Config>::AccountId;
	type OpaqueCall = Vec<u8>;
	type ProposalId = u32;

	#[pallet::config]
	pub trait Config: frame_system::Config {
		/// Standard Event type.
		type Event: From<Event<Self>> + IsType<<Self as frame_system::Config>::Event>;
		/// The outer Origin needs to be compatible with this pallet's Origin
		type Origin: From<RawOrigin>
			+ From<frame_system::RawOrigin<<Self as frame_system::Config>::AccountId>>;
		/// Implementation of EnsureOrigin trait for governance
		type EnsureGovernance: EnsureOrigin<<Self as pallet::Config>::Origin>;
		/// The overarching call type.
		type Call: Member
			+ FullCodec
			+ UnfilteredDispatchable<Origin = <Self as Config>::Origin>
			+ GetDispatchInfo;
		/// UnixTime implementation for TimeSource
		type TimeSource: UnixTime;
	}
	#[pallet::pallet]
	#[pallet::generate_store(pub(super) trait Store)]
	pub struct Pallet<T>(_);

	/// Proposals
	#[pallet::storage]
	#[pallet::getter(fn proposals)]
	pub(super) type Proposals<T: Config> =
		StorageMap<_, Blake2_128Concat, u32, Proposal<T::AccountId>, ValueQuery>;

	/// Active proposals
	#[pallet::storage]
	#[pallet::getter(fn active_proposals)]
	pub(super) type ActiveProposals<T> = StorageValue<_, Vec<ActiveProposal>, ValueQuery>;

	/// Number of proposals
	#[pallet::storage]
	#[pallet::getter(fn number_of_proposals)]
	pub(super) type NumberOfProposals<T> = StorageValue<_, u32, ValueQuery>;

	/// Time span in which an proposal expires
	#[pallet::storage]
	#[pallet::getter(fn expiry_span)]
	pub(super) type ExpirySpan<T> = StorageValue<_, u64, ValueQuery>;

	/// Array of accounts which are included in the current governance
	#[pallet::storage]
	#[pallet::getter(fn members)]
	pub(super) type Members<T> = StorageValue<_, Vec<AccountId<T>>, ValueQuery>;

	/// on_initialize hook - check and execute before every block all ongoing proposals
	#[pallet::hooks]
	impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {
		fn on_initialize(_n: BlockNumberFor<T>) -> Weight {
			// Check if their are any ongoing proposals
			match <ActiveProposals<T>>::decode_len() {
				Some(proposal_len) if proposal_len > 0 => {
					// Get all expired proposals
					let expired_proposals =
						Self::filter_proposals_by(&|x| x.1 <= T::TimeSource::now().as_secs());
					// Remove expired proposals
					for expired_proposal in expired_proposals {
						<Proposals<T>>::remove(expired_proposal.0);
						Self::deposit_event(Event::Expired(expired_proposal.0));
					}
					// Get all not expired proposals
					let new_active_proposals =
						Self::filter_proposals_by(&|x| x.1 > T::TimeSource::now().as_secs());
					// Set the new proposals
					<ActiveProposals<T>>::set(new_active_proposals);
					// Todo: figure out some value here
					0
				}
				_ => 0,
			}
		}
	}

	#[pallet::event]
	#[pallet::generate_deposit(pub(super) fn deposit_event)]
	pub enum Event<T: Config> {
		/// A new proposal was submitted [proposal_id]
		Proposed(ProposalId),
		/// A proposal was executed [proposal_id]
		Executed(ProposalId),
		/// A proposal is expired [proposal_id]
		Expired(ProposalId),
		/// The execution of a proposal failed [proposal_id]
		ExecutionFailed(ProposalId),
		/// The decode of the a proposal failed [proposal_id]
		DecodeFailed(ProposalId),
		/// A proposal was approved [proposal_id]
		Approved(ProposalId),
	}

	#[pallet::error]
	pub enum Error<T> {
		/// An account already approved a proposal
		AlreadyApproved,
		/// A proposal was already executed
		AlreadyExecuted,
		/// A proposal is already expired
		AlreadyExpired,
		/// The signer of an extrinsic is no member of the current governance
		NoMember,
		/// The proposal was not found in the the proposal map
		NotFound,
		/// Sudo call failed
		SudoCallFailed,
	}

	#[pallet::call]
	impl<T: Config> Pallet<T> {
		/// Propose a governance ensured extrinsic
		#[pallet::weight(10_000)]
		pub fn propose_governance_extrinsic(
			origin: OriginFor<T>,
			call: Box<<T as Config>::Call>,
		) -> DispatchResultWithPostInfo {
			let who = ensure_signed(origin)?;
			// Ensure origin is part of the governance
			ensure!(<Members<T>>::get().contains(&who), Error::<T>::NoMember);
			// Generate the next proposal id
			let id = Self::get_next_id();
			<Proposals<T>>::insert(
				id,
				Proposal {
					call: call.encode(),
					created: T::TimeSource::now().as_secs(),
					approved: vec![],
				},
			);
			Self::deposit_event(Event::Proposed(id));
			<NumberOfProposals<T>>::put(id);
			<ActiveProposals<T>>::mutate(|p| {
				p.push((id, T::TimeSource::now().as_secs() + <ExpirySpan<T>>::get()));
			});
			Ok(().into())
		}
		/// Sets a new set of governance members
		#[pallet::weight(10_000)]
		pub fn new_membership_set(
			origin: OriginFor<T>,
			accounts: Vec<T::AccountId>,
		) -> DispatchResultWithPostInfo {
			// Ensure the extrinsic was executed by the governance
			T::EnsureGovernance::ensure_origin(origin)?;
			<Members<T>>::put(accounts);
			Ok(().into())
		}
		/// Approve a proposal by a given proposal id
		#[pallet::weight(10_000)]
		pub fn approve(origin: OriginFor<T>, id: u32) -> DispatchResultWithPostInfo {
			let who = ensure_signed(origin)?;
			// Ensure origin is part of the governance
			ensure!(<Members<T>>::get().contains(&who), Error::<T>::NoMember);
			// Try to approve the proposal
			Self::try_approve(who, id)?;
			// Try to execute the proposal
			Self::execute_proposal(id);
			Ok(().into())
		}
		/// Execute an extrinsic as sudo
		#[pallet::weight(10_000)]
		pub fn call_as_sudo(
			origin: OriginFor<T>,
			call: Box<<T as Config>::Call>,
		) -> DispatchResultWithPostInfo {
			T::EnsureGovernance::ensure_origin(origin)?;
			let result = call.dispatch_bypass_filter(frame_system::RawOrigin::Root.into());
			ensure!(result.is_ok(), Error::<T>::SudoCallFailed);
			Ok(().into())
		}
	}

	/// Genesis definition
	#[pallet::genesis_config]
	pub struct GenesisConfig<T: Config> {
		pub members: Vec<AccountId<T>>,
		pub expiry_span: u64,
	}

	#[cfg(feature = "std")]
	impl<T: Config> Default for GenesisConfig<T> {
		fn default() -> Self {
			Self {
				members: Default::default(),
				expiry_span: 7200,
			}
		}
	}

	/// Sets the genesis governance
	#[pallet::genesis_build]
	impl<T: Config> GenesisBuild<T> for GenesisConfig<T> {
		fn build(&self) {
			Members::<T>::set(self.members.clone());
			ExpirySpan::<T>::set(self.expiry_span);
		}
	}

	#[pallet::origin]
	pub type Origin = RawOrigin;

	/// The raw origin enum for this pallet.
	#[derive(PartialEq, Eq, Clone, RuntimeDebug, Encode, Decode)]
	pub enum RawOrigin {
		GovernanceThreshold,
	}
}

/// Custom governance origin
pub struct EnsureGovernance;

/// Implementation for EnsureOrigin trait for custom EnsureGovernance struct.
/// We use this to execute extrinsic by a governance origin.
impl<OuterOrigin> EnsureOrigin<OuterOrigin> for EnsureGovernance
where
	OuterOrigin: Into<Result<RawOrigin, OuterOrigin>> + From<RawOrigin>,
{
	type Success = ();

	fn try_origin(o: OuterOrigin) -> Result<Self::Success, OuterOrigin> {
		match o.into() {
			Ok(o) => match o {
				RawOrigin::GovernanceThreshold => Ok(()),
			},
			Err(o) => Err(o),
		}
	}

	#[cfg(feature = "runtime-benchmarks")]
	fn successful_origin() -> OuterOrigin {
		RawOrigin::GovernanceThreshold.into()
	}
}

impl<T: Config> Pallet<T> {
	/// Returns the next proposal id
	fn get_next_id() -> u32 {
		<NumberOfProposals<T>>::get().add(1)
	}
	/// Executes an proposal if the majority is reached
	fn execute_proposal(id: u32) {
		let proposal = <Proposals<T>>::get(id);
		if Self::majority_reached(proposal.approved.len()) {
			//Try to decode the stored extrinsic
			if let Some(call) = Self::decode_call(&proposal.call) {
				// Execute the extrinsic
				let result = call.dispatch_bypass_filter((RawOrigin::GovernanceThreshold).into());
				// Check the result and emit events
				if result.is_ok() {
					Self::deposit_event(Event::Executed(id));
				} else {
					Self::deposit_event(Event::ExecutionFailed(id));
				}
				// Remove the proposal from storage
				<Proposals<T>>::remove(id);
				// Remove the proposal from active proposals
				let new_active_proposals = Self::filter_proposals_by(&|x| x.0 != id);
				// Set the new active proposals
				<ActiveProposals<T>>::set(new_active_proposals);
			} else {
				// Emit an event if the decode of a call failed
				Self::deposit_event(Event::DecodeFailed(id));
			}
		}
	}
	/// Filters the active proposals array by a givin clojure
	fn filter_proposals_by(filter: &dyn Fn(&&(u32, u64)) -> bool) -> Vec<(u32, u64)> {
		let active_proposals = <ActiveProposals<T>>::get();
		let filtered_proposals = active_proposals
			.iter()
			.filter(filter)
			.cloned()
			.collect::<Vec<_>>();
		filtered_proposals
	}
	/// Checks if the majority for a proposal is reached
	fn majority_reached(approvals: usize) -> bool {
		let total_number_of_voters = <Members<T>>::get().len() as u32;
		let threshold = if total_number_of_voters % 2 == 0 {
			total_number_of_voters / 2
		} else {
			total_number_of_voters / 2 + 1
		};
		approvals as u32 >= threshold
	}
	/// Tries to approve a proposal
	fn try_approve(account: T::AccountId, id: u32) -> Result<(), DispatchError> {
		if <Proposals<T>>::contains_key(id) {
			<Proposals<T>>::mutate(id, |proposal| {
				// Check already approved
				if proposal.approved.contains(&account) {
					return Err(Error::<T>::AlreadyApproved.into());
				}
				proposal.approved.push(account);
				Self::deposit_event(Event::Approved(id));
				Ok(())
			})
		} else {
			Err(Error::<T>::NotFound.into())
		}
	}
	/// Decodes a encoded representation of a Call
	/// Returns None if the encode of the extrinsic has failed
	fn decode_call(call: &Vec<u8>) -> Option<<T as Config>::Call> {
		Decode::decode(&mut &call[..]).ok()
	}
}
