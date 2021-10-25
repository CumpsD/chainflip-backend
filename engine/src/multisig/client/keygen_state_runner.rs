use std::sync::Arc;

use pallet_cf_vaults::CeremonyId;
use tokio::sync::mpsc::UnboundedSender;

use crate::{multisig::client::ThresholdParameters, p2p::AccountId};

use super::{
    common::{broadcast::BroadcastStage, CeremonyCommon, CeremonyStage, KeygenResult},
    keygen::{AwaitCommitments1, KeygenData, KeygenP2PSender},
    state_runner::{StateAuthorised, StateRunner},
    utils::PartyIdxMapping,
    InnerEvent, KeygenResultInfo,
};

#[derive(Clone)]
pub struct KeygenStateRunner {
    //
    inner: StateRunner<KeygenData, KeygenResult>,
    idx_mapping: Option<Arc<PartyIdxMapping>>,
    logger: slog::Logger,
}

impl KeygenStateRunner {
    pub fn new_unauthorised(logger: slog::Logger) -> Self {
        KeygenStateRunner {
            logger: logger.clone(),
            inner: StateRunner::new_unauthorised(logger),
            idx_mapping: None,
        }
    }

    pub fn on_keygen_request(
        &mut self,
        ceremony_id: CeremonyId,
        event_sender: UnboundedSender<InnerEvent>,
        idx_mapping: Arc<PartyIdxMapping>,
        our_idx: usize,
        all_idxs: Vec<usize>,
    ) {
        self.idx_mapping = Some(idx_mapping.clone());

        let common = CeremonyCommon {
            ceremony_id,
            // TODO: do not clone validator map
            p2p_sender: KeygenP2PSender::new(
                idx_mapping.clone(),
                event_sender.clone(),
                ceremony_id,
            ),
            own_idx: our_idx,
            all_idxs,
            logger: self.logger.clone(),
        };

        let processor = AwaitCommitments1::new(ceremony_id, common.clone());

        let mut stage = BroadcastStage::new(processor, common);

        stage.init();

        let state = StateAuthorised {
            ceremony_id,
            stage: Some(Box::new(stage)),
            idx_mapping,
            result_sender: event_sender,
        };

        self.inner.init(state);
    }

    pub fn try_expiring(&mut self) -> Option<Vec<AccountId>> {
        self.inner.try_expiring()
    }

    pub fn process_message(
        &mut self,
        sender_id: AccountId,
        data: KeygenData,
    ) -> Option<Result<KeygenResultInfo, Vec<AccountId>>> {
        self.inner.process_message(sender_id, data).map(|res| {
            res.map(|keygen_result| {
                let params =
                    ThresholdParameters::from_share_count(keygen_result.party_public_keys.len());

                let idx_mapping = self
                    .idx_mapping
                    .as_ref()
                    .expect("idx mapping should be present")
                    .clone();

                KeygenResultInfo {
                    key: Arc::new(keygen_result),
                    validator_map: idx_mapping,
                    params,
                }
            })
        })
    }
}

#[cfg(test)]
impl KeygenStateRunner {
    pub fn get_stage(&self) -> Option<String> {
        self.inner.get_stage()
    }

    #[cfg(test)]
    pub fn set_expiry_time(&mut self, expiry_time: std::time::Instant) {
        self.inner.set_expiry_time(expiry_time)
    }
}
