use cf_chains::Ethereum;
use cf_traits::{ChainflipAccountData, ChainflipAccountState};
use futures::{Stream, StreamExt};
use pallet_cf_broadcast::TransmissionFailure;
use pallet_cf_vaults::BlockHeightWindow;
use slog::o;
use sp_core::H256;
use state_chain_runtime::AccountId;
use std::{collections::BTreeSet, iter::FromIterator, sync::Arc};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::{
    eth::{EthBroadcaster, EthRpcApi},
    logging::{CEREMONY_ID_KEY, COMPONENT_KEY, LOG_ACCOUNT_STATE},
    multisig::{
        KeyId, KeygenOutcome, KeygenRequest, MessageHash, MultisigOutcome, MultisigRequest,
        SigningOutcome, SigningRequest,
    },
    multisig_p2p::AccountPeerMappingChange,
    state_chain::client::{StateChainClient, StateChainRpcApi},
};

async fn process_multisig_outcome<RpcClient>(
    state_chain_client: Arc<StateChainClient<RpcClient>>,
    multisig_outcome: MultisigOutcome,
    logger: &slog::Logger,
) where
    RpcClient: StateChainRpcApi,
{
    match multisig_outcome {
        MultisigOutcome::Signing(SigningOutcome { id, result }) => match result {
            Ok(sig) => {
                let _result = state_chain_client
                    .submit_unsigned_extrinsic(
                        pallet_cf_threshold_signature::Call::signature_success(id, sig.into()),
                        logger,
                    )
                    .await;
            }
            Err((err, bad_account_ids)) => {
                slog::error!(
                    logger,
                    "Signing ceremony failed with error: {:?}",
                    err;
                    CEREMONY_ID_KEY => id,
                );

                let _result = state_chain_client
                    .submit_signed_extrinsic(
                        pallet_cf_threshold_signature::Call::report_signature_failed_unbounded(
                            id,
                            bad_account_ids.into_iter().collect(),
                        ),
                        logger,
                    )
                    .await;
            }
        },
        MultisigOutcome::Keygen(KeygenOutcome { id, result }) => match result {
            Ok(keygen_result_info) => {
                let _result = state_chain_client
                    .submit_signed_extrinsic(
                        pallet_cf_vaults::Call::report_keygen_outcome(
                            id,
                            pallet_cf_vaults::KeygenOutcome::Success(
                                cf_chains::eth::AggKey::from_pubkey_compressed(
                                    keygen_result_info
                                        .key
                                        .get_public_key()
                                        .get_element()
                                        .serialize(),
                                ),
                            ),
                        ),
                        logger,
                    )
                    .await;
            }
            Err((err, bad_account_ids)) => {
                slog::error!(
                    logger,
                    "Keygen ceremony failed with error: {:?}",
                    err;
                    CEREMONY_ID_KEY => id,
                );
                let _result = state_chain_client
                    .submit_signed_extrinsic(
                        pallet_cf_vaults::Call::report_keygen_outcome(
                            id,
                            pallet_cf_vaults::KeygenOutcome::Failure(BTreeSet::from_iter(
                                bad_account_ids,
                            )),
                        ),
                        logger,
                    )
                    .await;
            }
        },
    };
}

pub async fn start<BlockStream, RpcClient, EthRpc>(
    state_chain_client: Arc<StateChainClient<RpcClient>>,
    sc_block_stream: BlockStream,
    eth_broadcaster: EthBroadcaster<EthRpc>,
    multisig_request_sender: UnboundedSender<MultisigRequest>,
    account_peer_mapping_change_sender: UnboundedSender<(
        AccountId,
        sp_core::ed25519::Public,
        AccountPeerMappingChange,
    )>,
    mut multisig_outcome_receiver: UnboundedReceiver<MultisigOutcome>,

    // TODO: we should be able to factor this out into a single ETH window sender
    sm_window_sender: UnboundedSender<BlockHeightWindow>,
    km_window_sender: UnboundedSender<BlockHeightWindow>,
    initial_block_hash: H256,
    logger: &slog::Logger,
) where
    BlockStream: Stream<Item = anyhow::Result<state_chain_runtime::Header>>,
    RpcClient: StateChainRpcApi,
    EthRpc: EthRpcApi,
{
    let logger = logger.new(o!(COMPONENT_KEY => "SCObserver"));

    let blocks_per_heartbeat = std::cmp::max(1, state_chain_client.heartbeat_block_interval / 2);

    slog::info!(
        logger,
        "Sending heartbeat every {} blocks",
        blocks_per_heartbeat
    );

    state_chain_client
        .submit_signed_extrinsic(pallet_cf_online::Call::heartbeat(), &logger)
        .await
        .expect("Should be able to submit first heartbeat");

    async fn get_current_account_state<RpcClient: StateChainRpcApi>(
        state_chain_client: Arc<StateChainClient<RpcClient>>,
        block_hash: H256,
        logger: &slog::Logger,
    ) -> (ChainflipAccountData, bool) {
        let new_account_data = state_chain_client
            .get_account_data(block_hash)
            .await
            .expect("Could not get account data");

        let current_epoch = state_chain_client
            .epoch_at_block(block_hash)
            .await
            .expect("Could not get current epoch");

        let is_outgoing = if let Some(last_active_epoch) = new_account_data.last_active_epoch {
            last_active_epoch + 1 == current_epoch
        } else {
            false
        };

        slog::debug!(logger, #LOG_ACCOUNT_STATE, "Account state: {:?}",  new_account_data.state;
        "is_outgoing" => is_outgoing, "last_active_epoch" => new_account_data.last_active_epoch);

        (new_account_data, is_outgoing)
    }

    async fn send_windows_to_witness_processes<RpcClient: StateChainRpcApi>(
        state_chain_client: Arc<StateChainClient<RpcClient>>,
        block_hash: H256,
        account_data: ChainflipAccountData,
        sm_window_sender: &UnboundedSender<BlockHeightWindow>,
        km_window_sender: &UnboundedSender<BlockHeightWindow>,
    ) -> anyhow::Result<()> {
        let eth_vault = state_chain_client
            .get_vault::<Ethereum>(
                block_hash,
                account_data
                    .last_active_epoch
                    .expect("we are active or outgoing"),
            )
            .await?;
        sm_window_sender
            .send(eth_vault.active_window.clone())
            .unwrap();
        km_window_sender.send(eth_vault.active_window).unwrap();
        Ok(())
    }

    // Initialise the account state
    let (mut account_data, mut is_outgoing) =
        get_current_account_state(state_chain_client.clone(), initial_block_hash, &logger).await;

    if account_data.state == ChainflipAccountState::Validator || is_outgoing {
        send_windows_to_witness_processes(
            state_chain_client.clone(),
            initial_block_hash,
            account_data,
            &sm_window_sender,
            &km_window_sender,
        )
        .await
        .expect("Failed to send windows to the witness processes");
    }

    let mut sc_block_stream = Box::pin(sc_block_stream);
    loop {
        tokio::select! {
            option_result_block_header = sc_block_stream.next() => {
                match option_result_block_header {
                    Some(result_block_header) => {
                        match result_block_header {
                            Ok(current_block_header) => {
                                let current_block_hash = current_block_header.hash();
                                slog::debug!(
                                    logger,
                                    "Processing SC block {} with block hash: {:#x}",
                                    current_block_header.number,
                                    current_block_hash
                                );

                                let mut received_new_epoch = false;

                                // Process this block's events
                                match state_chain_client.get_events(current_block_hash).await {
                                    Ok(events) => {
                                        for (_phase, event, _topics) in events {

                                            slog::debug!(logger, "Received event at block {}: {:?}", current_block_header.number, &event);

                                            match event {
                                                state_chain_runtime::Event::Validator(
                                                    pallet_cf_validator::Event::NewEpoch(_),
                                                ) => {
                                                    received_new_epoch = true;
                                                }
                                                state_chain_runtime::Event::Validator(
                                                    pallet_cf_validator::Event::PeerIdRegistered(
                                                        account_id,
                                                        peer_id,
                                                        port,
                                                        ip_address,
                                                    ),
                                                ) => {
                                                    account_peer_mapping_change_sender
                                                        .send((
                                                            account_id,
                                                            peer_id,
                                                            AccountPeerMappingChange::Registered(
                                                                port,
                                                                ip_address.into(),
                                                            ),
                                                        ))
                                                        .unwrap();
                                                }
                                                state_chain_runtime::Event::Validator(
                                                    pallet_cf_validator::Event::PeerIdUnregistered(
                                                        account_id,
                                                        peer_id,
                                                    ),
                                                ) => {
                                                    account_peer_mapping_change_sender
                                                        .send((
                                                            account_id,
                                                            peer_id,
                                                            AccountPeerMappingChange::Unregistered,
                                                        ))
                                                        .unwrap();
                                                }
                                                state_chain_runtime::Event::EthereumVault(
                                                    pallet_cf_vaults::Event::KeygenRequest(
                                                        ceremony_id,
                                                        validator_candidates,
                                                    ),
                                                ) => {
                                                    let gen_new_key_event = MultisigRequest::Keygen(
                                                        KeygenRequest::new(ceremony_id, validator_candidates),
                                                    );

                                                    multisig_request_sender
                                                        .send(gen_new_key_event)
                                                        .map_err(|_| "Receiver should exist")
                                                        .unwrap();
                                                }
                                                state_chain_runtime::Event::EthereumThresholdSigner(
                                                    pallet_cf_threshold_signature::Event::ThresholdSignatureRequest(
                                                        ceremony_id,
                                                        key_id,
                                                        validators,
                                                        payload,
                                                    ),
                                                ) if validators.contains(&state_chain_client.our_account_id) => {
                                                    let sign_tx = MultisigRequest::Sign(SigningRequest::new(
                                                        ceremony_id,
                                                        KeyId(key_id),
                                                        MessageHash(payload.to_fixed_bytes()),
                                                        validators,
                                                    ));

                                                    // The below will be replaced with one shot channels
                                                    multisig_request_sender
                                                        .send(sign_tx)
                                                        .map_err(|_| "Receiver should exist")
                                                        .unwrap();
                                                }
                                                state_chain_runtime::Event::EthereumBroadcaster(
                                                    pallet_cf_broadcast::Event::TransactionSigningRequest(
                                                        attempt_id,
                                                        validator_id,
                                                        unsigned_tx,
                                                    ),
                                                ) if validator_id == state_chain_client.our_account_id => {
                                                    slog::debug!(
                                                        logger,
                                                        "Received signing request with attempt_id {} for transaction: {:?}",
                                                        attempt_id,
                                                        unsigned_tx,
                                                    );
                                                    match eth_broadcaster.encode_and_sign_tx(unsigned_tx).await {
                                                        Ok(raw_signed_tx) => {
                                                            let _result = state_chain_client.submit_signed_extrinsic(
                                                                state_chain_runtime::Call::EthereumBroadcaster(
                                                                    pallet_cf_broadcast::Call::transaction_ready_for_transmission(
                                                                        attempt_id,
                                                                        raw_signed_tx.0,
                                                                        eth_broadcaster.address,
                                                                    ),
                                                                ),
                                                                &logger,
                                                            ).await;
                                                        }
                                                        Err(e) => {
                                                            // Note: this error case should only occur if there is a problem with the
                                                            // local ethereum node, which would mean the web3 lib is unable to fill in
                                                            // the tranaction params, mainly the gas limit.
                                                            // In the long run all transaction parameters will be provided by the state
                                                            // chain and the above eth_broadcaster.sign_tx method can be made
                                                            // infallible.
                                                            slog::error!(
                                                                logger,
                                                                "TransactionSigningRequest attempt_id {} failed: {:?}",
                                                                attempt_id,
                                                                e
                                                            );
                                                        }
                                                    }
                                                }
                                                state_chain_runtime::Event::EthereumBroadcaster(
                                                    pallet_cf_broadcast::Event::TransmissionRequest(
                                                        attempt_id,
                                                        signed_tx,
                                                    ),
                                                ) => {
                                                    let response_extrinsic = match eth_broadcaster
                                                        .send(signed_tx)
                                                        .await
                                                    {
                                                        Ok(tx_hash) => {
                                                            slog::debug!(
                                                                logger,
                                                                "Successful TransmissionRequest attempt_id {}, tx_hash: {:#x}",
                                                                attempt_id,
                                                                tx_hash
                                                            );
                                                            pallet_cf_witnesser_api::Call::witness_eth_transmission_success(
                                                                attempt_id, tx_hash
                                                            )
                                                        }
                                                        Err(e) => {
                                                            slog::error!(
                                                                logger,
                                                                "TransmissionRequest attempt_id {} failed: {:?}",
                                                                attempt_id,
                                                                e
                                                            );
                                                            // TODO: Fill in the transaction hash with the real one
                                                            pallet_cf_witnesser_api::Call::witness_eth_transmission_failure(
                                                                attempt_id, TransmissionFailure::TransactionRejected, Default::default()
                                                            )
                                                        }
                                                    };
                                                    let _result = state_chain_client
                                                        .submit_signed_extrinsic(response_extrinsic, &logger)
                                                        .await;
                                                }
                                                ignored_event => {
                                                    // ignore events we don't care about
                                                    slog::trace!(
                                                        logger,
                                                        "Ignoring event at block {}: {:?}",
                                                        current_block_header.number,
                                                        ignored_event
                                                    );
                                                }
                                            }
                                        }
                                    }
                                    Err(error) => {
                                        slog::error!(
                                            logger,
                                            "Failed to decode events at block {}. {}",
                                            current_block_header.number,
                                            error,
                                        );
                                    }
                                }

                                // Backup and passive nodes need to update their state on every block (since it's possible to move
                                // between Backup and Passive on every block), while Active nodes only need to update every new epoch.
                                if received_new_epoch || matches!(
                                    account_data.state,
                                    ChainflipAccountState::Backup | ChainflipAccountState::Passive
                                ) {
                                    let (new_account_data, new_is_outgoing) =
                                        get_current_account_state(state_chain_client.clone(), current_block_hash, &logger).await;
                                    account_data = new_account_data;
                                    is_outgoing = new_is_outgoing;

                                }

                                // New windows should be sent to outgoing validators (so they know when to finish) or to
                                // new/existing validators (so they know when to start)
                                // Note: nodes that were outgoing in the last epoch (active 2 epochs ago) have already
                                // received their end window, so we don't need to send anything to them
                                if received_new_epoch && (matches!(account_data.state, ChainflipAccountState::Validator) || is_outgoing) {
                                    send_windows_to_witness_processes(
                                        state_chain_client.clone(),
                                        current_block_hash,
                                        account_data,
                                        &sm_window_sender,
                                        &km_window_sender,
                                    )
                                    .await
                                    .expect("Failed to send windows to the witness processes");
                                }



                                // All nodes must send a heartbeat regardless of their validator status (at least for now).
                                // We send it in the middle of the online interval (so any node sync issues don't
                                // cause issues (if we tried to send on one of the interval boundaries)
                                if (current_block_header.number + (state_chain_client.heartbeat_block_interval / 2))
                                    % blocks_per_heartbeat
                                    == 0
                                {
                                    slog::info!(
                                        logger,
                                        "Sending heartbeat at block: {}",
                                        current_block_header.number
                                    );
                                    let _result = state_chain_client
                                        .submit_signed_extrinsic(pallet_cf_online::Call::heartbeat(), &logger)
                                        .await;
                                }
                            }
                            Err(error) => {
                                slog::error!(logger, "Failed to decode block header: {}", error,);
                            }
                        }
                    },
                    None => {
                        slog::error!(logger, "Exiting as State Chain block stream ended");
                        break
                    }
                }
            },
            option_multisig_outcome = multisig_outcome_receiver.recv() => {
                match option_multisig_outcome {
                    Some(multisig_outcome) => {
                        process_multisig_outcome(state_chain_client.clone(), multisig_outcome, &logger).await;
                    },
                    None => {
                        slog::error!(logger, "Exiting as multisig_outcome channel ended");
                        break
                    }
                }
            }
        }
    }
}
