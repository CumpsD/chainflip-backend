use std::{collections::HashMap, convert::TryInto};

use crate::{
    eth::key_manager::KeyManager,
    eth::utils,
    logging::COMPONENT_KEY,
    mq::{IMQClient, Subject},
    p2p::ValidatorId,
    settings,
    signing::{
        KeyId, MessageHash, MessageInfo, MultisigEvent, MultisigInstruction, SchnorrSignature,
        SigningInfo,
    },
    types::chain::Chain,
};

use anyhow::Result;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use slog::o;
use sp_core::Hasher;
use sp_runtime::traits::Keccak256;
use web3::{ethabi::Token, types::Address};

/// Helper function, constructs and runs the [SetAggKeyWithAggKeyEncoder] asynchronously.
pub async fn start<MQC: IMQClient + Clone>(
    settings: &settings::Settings,
    mq_client: MQC,
    logger: &slog::Logger,
) {
    SetAggKeyWithAggKeyEncoder::new(
        &settings,
        mq_client,
        logger,
    )
    .expect("Should create eth tx encoder")
    .process_multi_sig_event_stream()
    .await;
}

/// Details of a transaction to be broadcast to ethereum.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct TxDetails {
    pub contract_address: Address,
    pub data: Vec<u8>,
}

/// Reads [AuctionConfirmedEvent]s off the message queue and encodes the function call to the stake manager.
#[derive(Clone)]
struct SetAggKeyWithAggKeyEncoder<MQC: IMQClient> {
    mq_client: MQC,
    key_manager: KeyManager,
    // maps the MessageHash which gets sent to the signer with the data that the MessageHash is a hash of
    messages: HashMap<MessageHash, ParamContainer>,
    // On genesis, where do these validators come from, to allow for the first key update
    validators: HashMap<KeyId, Vec<ValidatorId>>,
    curr_signing_key_id: Option<KeyId>,
    next_key_id: Option<KeyId>,
    logger: slog::Logger,
}

#[derive(Clone)]
struct ParamContainer {
    pub key_id: KeyId,
    pub key_nonce: [u8; 32],
    pub pubkey_x: [u8; 32],
    pub pubkey_y_parity: u8,
}

impl<MQC: IMQClient + Clone> SetAggKeyWithAggKeyEncoder<MQC> {
    fn new(
        settings: &settings::Settings,
        mq_client: MQC,
        logger: &slog::Logger,
    ) -> Result<Self> {
        let key_manager = KeyManager::new(settings)?;

        let mut genesis_validator_ids_hash_map = HashMap::new();
        genesis_validator_ids_hash_map.insert(KeyId(0), settings.signing.genesis_validator_ids.clone());
        Ok(Self {
            mq_client,
            key_manager,
            messages: HashMap::new(),
            validators: genesis_validator_ids_hash_map,
            curr_signing_key_id: Some(KeyId(0)),
            next_key_id: None,
            logger: logger.new(o!(COMPONENT_KEY => "SetAggKeyWithAggKeyEncoder")),
        })
    }

    /// Read events from the MultisigEvent subject and process them
    /// The messages we care about are:
    /// 1. `MultisigEvent::KeygenResult` which is emitted after a new key has been
    /// successfully generated by the signing module
    /// 2. `MultisigEvent::MessagedSigned` which is emitted after the Signing module
    /// has successfully signed a message with a particular (denoted by KeyId) key
    async fn process_multi_sig_event_stream(&mut self) {
        let mut multisig_event_stream = self
            .mq_client
            .subscribe::<MultisigEvent>(Subject::MultisigEvent)
            .await
            .unwrap();

        while let Some(event) = multisig_event_stream.next().await {
            match event {
                Ok(event) => match event {
                    MultisigEvent::KeygenResult(key_outcome) => match key_outcome.result {
                        Ok(key) => {
                            self.handle_keygen_success(key_outcome.ceremony_id, key)
                                .await;
                        }
                        Err((err, _)) => {
                            slog::error!(
                                self.logger,
                                "Signing module returned error generating key: {:?}",
                                err
                            )
                        }
                    },
                    MultisigEvent::MessageSigningResult(signing_outcome) => {
                        match signing_outcome.result {
                            Ok(sig) => {
                                self.handle_set_agg_key_message_signed(
                                    signing_outcome.ceremony_id,
                                    sig,
                                )
                                .await;
                            }
                            Err((err, _)) => {
                                // TODO: Use the reported bad nodes in the SigningOutcome / SigningFailure
                                // TODO: retry signing with a different subset of signers
                                slog::error!(
                                    self.logger,
                                    "Signing module returned error signing message: {:?}",
                                    err
                                )
                            }
                        }
                    }
                    _ => {
                        slog::trace!(
                            self.logger,
                            "Discarding non keygen result or message signed event"
                        )
                    }
                },
                Err(e) => {
                    slog::error!(
                        self.logger,
                        "Error reading event from multisig event stream: {:?}",
                        e
                    );
                }
            }
        }
    }

    // When the keygen message has been received we must:
    // 1. Build the ETH encoded setAggKeyWithAggKey transaction parameters
    // 2. Store the tx parameters in state for use later
    // 3. Create a Signing Instruction
    // 4. Push this instruction to the MQ for the signing module to pick up
    async fn handle_keygen_success(&mut self, key_id: KeyId, key: secp256k1::PublicKey) {
        // This has nothing to do with building an ETH transaction.
        // We encode the tx like this, in eth format, because this is how the contract will
        // serialise the data to verify the signature over the message hash
        let (pubkey_x, pubkey_y_parity) = destructure_pubkey(key);

        let param_container = ParamContainer {
            key_id,
            pubkey_x,
            pubkey_y_parity,
            key_nonce: [0u8; 32],
        };

        let encoded_fn_params = self
            .encode_set_agg_key_with_agg_key(
                [0u8; 32],
                [0u8; 32],
                [0u8; 32],
                [0u8; 20],
                pubkey_x,
                pubkey_y_parity,
            )
            .expect("should be a valid encoded params");

        let hash = Keccak256::hash(&encoded_fn_params[..]);
        let message_hash = MessageHash(hash.0);

        // store key: parameters, so we can fetch the parameters again, after the payload
        // has been signed by the signing module
        self.messages.insert(message_hash.clone(), param_container);

        // Use *all* the validators for now
        let key_id = self.curr_signing_key_id.expect("KeyId should be set here");
        let signing_info = SigningInfo::new(
            key_id,
            self.validators
                .get(&key_id)
                .expect("validators should exist for current KeyId")
                .clone(),
        );

        let signing_instruction = MultisigInstruction::Sign(message_hash, signing_info);

        self.mq_client
            .publish(Subject::MultisigInstruction, &signing_instruction)
            .await
            .expect("Should publish to MQ");
    }

    // When the signed message has been received we must:
    // 1. Get the parameters (`ParameterContainer`) that we stored in state (and submitted to the signing module in encoded form) earlier
    // 2. Build a valid ethereum encoded transaction using the message hash and signature returned by the Signing module
    // 3. Push this transaction to the Broadcast(Chain::ETH) subject, to be broadcast by the ETH Broadcaster
    // 4. Update the current key id, with the new key id returned by the signing module, so we know which key to sign with
    // from now onwards, until the next successful key rotation
    async fn handle_set_agg_key_message_signed(
        &mut self,
        message_info: MessageInfo,
        sig: SchnorrSignature,
    ) {
        // 1. Get the data from the message hash that was signed (using the `messages` field)
        let nonce_times_g_addr = utils::pubkey_to_eth_addr(sig.r);
        let key_id = message_info.key_id;
        let msg_hash = message_info.hash;
        let params = self
            .messages
            .get(&msg_hash)
            .expect("should have been stored when asked to sign");

        // 2. Call build_tx with the required info
        match self.build_tx(&msg_hash, sig.s, nonce_times_g_addr, params) {
            Ok(ref tx_details) => {
                // 3. Send it on its way to the eth broadcaster
                self.mq_client
                    .publish(Subject::Broadcast(Chain::ETH), tx_details)
                    .await
                    .unwrap_or_else(|err| {
                        slog::error!(self.logger, "Could not process: {:#?}", err);
                    });
                // here (for now) we assume the key was update successfully
                // update curr key id
                self.curr_signing_key_id = Some(key_id);
                // reset
                self.next_key_id = None;
            }
            Err(err) => {
                slog::error!(self.logger, "Failed to build: {:#?}", err);
            }
        }
    }

    fn build_tx(
        &self,
        msg: &MessageHash,
        s: [u8; 32],
        nonce_times_g_addr: [u8; 20],
        params: &ParamContainer,
    ) -> Result<TxDetails> {
        Ok(TxDetails {
            contract_address: self.key_manager.deployed_address,
            data: self.encode_set_agg_key_with_agg_key(
                msg.0,
                s,
                params.key_nonce,
                nonce_times_g_addr,
                params.pubkey_x,
                params.pubkey_y_parity,
            )?,
        })
    }

    // not sure if key nonce should be u64...
    // sig = s in the literature. The scalar of the signature
    fn encode_set_agg_key_with_agg_key(
        &self,
        msg_hash: [u8; 32],
        sig: [u8; 32],
        key_nonce: [u8; 32],
        nonce_times_g_addr: [u8; 20],
        pubkey_x: [u8; 32],
        pubkey_y_parity: u8,
    ) -> Result<Vec<u8>> {
        // Serialize the data using eth encoding so the KeyManager contract can serialize the data in the same way
        // in order to verify the signature
        Ok(self.key_manager.contract.function("setAggKeyWithAggKey").expect("Function 'setAggKeyWithAggKey' should be defined in the KeyManager abi.").encode_input(
            // These are two arguments, SigData and Key from:
            // https://github.com/chainflip-io/chainflip-eth-contracts/blob/master/contracts/interfaces/IShared.sol
            &[
                // SigData
                Token::Tuple(vec![
                    Token::Uint(msg_hash.into()),              // msgHash
                    Token::Uint(sig.into()), // sig - this 's' in the literature, the signature scalar
                    Token::Uint(key_nonce.into()), // key nonce
                    Token::Address(nonce_times_g_addr.into()), // nonceTimesGAddr - this is r in the literature
                ]),
                // Key - the signing module will sign over the params, containing this
                Token::Tuple(vec![
                    Token::Uint(pubkey_x.into()),        // pubkeyX
                    Token::Uint(pubkey_y_parity.into()), // pubkeyYparity
                ]),
            ],
        )?)
    }
}

// Take a secp256k1 pubkey and return the pubkey_x and pubkey_y_parity
fn destructure_pubkey(pubkey: secp256k1::PublicKey) -> ([u8; 32], u8) {
    let pubkey_bytes: [u8; 33] = pubkey.serialize();
    let pubkey_y_parity_byte = pubkey_bytes[0];
    let pubkey_y_parity = if pubkey_y_parity_byte == 2 { 0u8 } else { 1u8 };
    let pubkey_x: [u8; 32] = pubkey_bytes[1..].try_into().expect("Is valid pubkey");

    return (pubkey_x, pubkey_y_parity);
}

#[cfg(test)]
mod test_eth_tx_encoder {
    use super::*;
    use hex;
    use num::BigInt;
    use secp256k1::PublicKey;
    use std::str::FromStr;

    use crate::{logging, mq::mq_mock::MQMock};

    const AGG_PRIV_HEX_1: &str = "fbcb47bc85b881e0dfb31c872d4e06848f80530ccbd18fc016a27c4a744d0eba";
    const AGG_PRIV_HEX_2: &str = "bbade2da39cfc81b1b64b6a2d66531ed74dd01803dc5b376ce7ad548bbe23608";
    const NONCE_TIMES_G_ADDR: &str = "02eDd8421D87B7c0eE433D3AFAd3aa2Ef039f27a";
    const SIGNATURE_S: &str =
        "86256580123538456061655860770396085945007591306530617821168588559087896188216";
    const MESSAGE_HASH: &str =
        "19838331578708755702960229198816480402256567085479269042839672688267843389518";

    #[test]
    fn test_message_hashing() {
        // The data is generated from: https://github.com/chainflip-io/chainflip-eth-contracts/blob/master/tests/integration/keyManager/test_setKey_setKey.py

        // sig data from contract, we aren't testing signing, so we use these values, generated from the contract tests
        // messageHashHex as an int int("{messageHashHex}", 16)), s / sig scalar, key nonce, nonce_times_g_addr
        // [19838331578708755702960229198816480402256567085479269042839672688267843389518, 86256580123538456061655860770396085945007591306530617821168588559087896188216, 0, '02eDd8421D87B7c0eE433D3AFAd3aa2Ef039f27a']

        // params used:
        // AGG_PRIV_HEX_1 = "fbcb47bc85b881e0dfb31c872d4e06848f80530ccbd18fc016a27c4a744d0eba"
        // AGG_K_HEX_1 = "d51e13c68bf56155a83e50fd9bc840e2a1847fb9b49cd206a577ecd1cd15e285"
        // AGG_SIGNER_1 = Signer(AGG_PRIV_HEX_1, AGG_K_HEX_1, AGG, nonces)
        // JUNK_HEX_PAD = 0000000000000000000000000000000000000000000000000000000000003039

        // We move to these keys
        // AGG_PRIV_HEX_2 = "bbade2da39cfc81b1b64b6a2d66531ed74dd01803dc5b376ce7ad548bbe23608"
        // AGG_K_HEX_2 = "ecb77b2eb59614237e5646b38bdf03cbdbdce61c874fdee6e228edaa26f01f5d"
        // AGG_SIGNER_2 = Signer(AGG_PRIV_HEX_2, AGG_K_HEX_2, AGG, nonces)

        // Pub data
        // [22479114112312168431982914496826057754130808976066989807481484372215659188398, 1]

        let mq = MQMock::new();

        let mq_client = mq.get_client();

        let logger = logging::test_utils::create_test_logger();

        let settings = settings::test_utils::new_test_settings().unwrap();

        let encoder = SetAggKeyWithAggKeyEncoder::new(
            &settings,
            mq_client,
            &logger,
        )
        .unwrap();

        let s = secp256k1::Secp256k1::signing_only();
        let _sk_1 = secp256k1::SecretKey::from_str(AGG_PRIV_HEX_1).unwrap();

        let sk_2 = secp256k1::SecretKey::from_str(AGG_PRIV_HEX_2).unwrap();

        // we rotate to key 2, so this is the pubkey we want to sign over
        let pk_2 = PublicKey::from_secret_key(&s, &sk_2);

        let (pubkey_x, pubkey_y_parity) = destructure_pubkey(pk_2);

        let encoded = encoder
            .encode_set_agg_key_with_agg_key(
                [0u8; 32],
                [0u8; 32],
                [0u8; 32],
                [0u8; 20],
                pubkey_x,
                pubkey_y_parity,
            )
            .unwrap();
        let hex_params = hex::encode(&encoded);
        println!("hex params: {:#?}", hex_params);
        // hex - from smart contract tests
        let call_data_no_sig_from_contract = "24969d5d00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001742daacd4dbfbe66d4c8965550295873c683cb3b65019d3a53975ba553cc31d0000000000000000000000000000000000000000000000000000000000000001";
        assert_eq!(call_data_no_sig_from_contract, hex_params);

        let message_hash: [u8; 32] = BigInt::from_str(MESSAGE_HASH)
            .unwrap()
            .to_bytes_be()
            .1
            .try_into()
            .unwrap();

        let message_hash = MessageHash(message_hash);

        let sig: num::BigInt = BigInt::from_str(SIGNATURE_S).unwrap();

        let param_container = ParamContainer {
            key_id: KeyId(0),
            key_nonce: [0u8; 32],
            pubkey_x,
            pubkey_y_parity,
        };

        let nonce_times_g_addr = hex::decode(NONCE_TIMES_G_ADDR).unwrap().try_into().unwrap();

        let sigma: [u8; 32] = sig.to_bytes_be().1.try_into().unwrap();

        let tx_data = encoder
            .build_tx(&message_hash, sigma, nonce_times_g_addr, &param_container)
            .unwrap()
            .data;

        let eth_input_from_receipt = "24969d5d2bdc19071c7994f088103dbf8d5476d6deb6d55ee005a2f510dc7640055cc84ebeb37e87509e15cd88b19fa224441c56acc0e143cb25b9fd1e57fdafed215538000000000000000000000000000000000000000000000000000000000000000000000000000000000000000002edd8421d87b7c0ee433d3afad3aa2ef039f27a1742daacd4dbfbe66d4c8965550295873c683cb3b65019d3a53975ba553cc31d0000000000000000000000000000000000000000000000000000000000000001";

        assert_eq!(eth_input_from_receipt.to_string(), hex::encode(&tx_data));
    }

    #[test]
    fn secp256k1_sanity_check() {
        let s = secp256k1::Secp256k1::signing_only();

        let sk = secp256k1::SecretKey::from_str(AGG_PRIV_HEX_1).unwrap();

        let pubkey_from_sk = PublicKey::from_secret_key(&s, &sk);

        // these keys should be derivable from each other.
        let pubkey = secp256k1::PublicKey::from_str(
            "0331b2ba4b46201610901c5164f42edd1f64ce88076fde2e2c544f9dc3d7b350ae",
        )
        .unwrap();

        // for sanity
        assert_eq!(pubkey_from_sk, pubkey);
    }
}
