pub mod broadcast;
mod broadcast_verification;
mod ceremony_stage;

pub use ceremony_stage::{CeremonyCommon, CeremonyStage, ProcessMessageResult, StageResult};

pub use broadcast_verification::BroadcastVerificationMessage;

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::{
    multisig::crypto::{KeyShare, Point},
};

use super::{utils::PartyIdxMapping, ThresholdParameters};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeygenResult {
    pub key_share: KeyShare,
    pub party_public_keys: Vec<Point>,
}

impl KeygenResult {
    pub fn get_public_key(&self) -> Point {
        self.key_share.y
    }

    /// Gets the serialized compressed public key (33 bytes - 32 bytes + a y parity byte)
    pub fn get_public_key_bytes(&self) -> Vec<u8> {
        use crate::multisig::crypto::ECPoint;
        self.key_share.y.get_element().serialize().into()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeygenResultInfo {
    pub key: Arc<KeygenResult>,
    pub validator_map: Arc<PartyIdxMapping>,
    pub params: ThresholdParameters,
}
