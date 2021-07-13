use codec::{Encode, Decode};
use frame_support::RuntimeDebug;

type NewPublicKey = Vec<u8>;
type BadValidators<ValidatorId> = Vec<ValidatorId>;

pub trait RequestResponse<Request, Response> {
	fn request(&self, request: Request);
	fn response(&self, response: Response);
}
pub trait Construct<ValidatorId> {
	// Start the construction phase.  When complete `ConstructionHandler::on_completion()`
	// would be used to notify that this is complete
	fn start_construction_phase(keygen_response: KeygenResponse<ValidatorId>);
}

pub trait ConstructionHandler {
	// Construction phase complete
	// fn on_completion(completed: Result<CompletedConstruct, CompletedConstructError>);
}

pub trait KeyGenRequestResponse<T>
	: RequestResponse<KeygenRequest<T>, KeygenResponse<T>> {}

pub trait ValidatorRotationRequestResponse<T>
	: RequestResponse<ValidatorRotationRequest, ValidatorRotationResponse> {}

pub trait AuctionPenalty<ValidatorId> {
	fn penalise(bad_validators: BadValidators<ValidatorId>);
}

pub trait KeyRotation<ValidatorId> {
	type AuctionPenalty: AuctionPenalty<ValidatorId>;
	type KeyGenRequestResponse: KeyGenRequestResponse<ValidatorId>;
	type Construct: Construct<ValidatorId>;
	type ConstructionHandler: ConstructionHandler;
	type ValidatorRotationRequestResponse: ValidatorRotationRequestResponse<ValidatorId>;
}

#[derive(PartialEq, Eq, Clone, Encode, Decode, RuntimeDebug)]
pub enum ChainParams {
	// Ethereum blockchain
	//
	// The value is the call data encoded for the final transaction
	// to request the key rotation via `setAggKeyWithAggKey`
	Ethereum(Vec<u8>),
}

#[derive(PartialEq, Eq, Clone, Encode, Decode, RuntimeDebug)]
pub struct KeygenRequest<ValidatorId> {
	// Chain
	chain: ChainParams,
	// validator_candidates - the set from which we would like to generate the key
	validator_candidates: Vec<ValidatorId>,
}

#[derive(PartialEq, Eq, Clone, Encode, Decode, RuntimeDebug)]
pub enum KeygenResponse<ValidatorId> {
	// The KGC has completed successfully with a new public key
	Success(NewPublicKey),
	// Something went wrong and it has failed.
	// Re-run the auction minus the bad validators
	Failure(BadValidators<ValidatorId>),
}

#[derive(PartialEq, Eq, Clone, Encode, Decode, RuntimeDebug)]
pub struct ValidatorRotationRequest {
	chain: ChainParams,
}

#[derive(PartialEq, Eq, Clone, Encode, Decode, RuntimeDebug)]
pub struct ValidatorRotationResponse {
	old_key: Vec<u8>,
	new_key: Vec<u8>,
	tx: Vec<u8>
}

