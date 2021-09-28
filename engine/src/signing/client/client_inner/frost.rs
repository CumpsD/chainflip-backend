//! The types and operations as discussed in https://eprint.iacr.org/2020/852.pdf.
//! Comments in this file reference sections from this document.
//! Note that unlike the protocol described in the document, we don't have a
//! centralised signature aggregator and don't have a preprocessing stage.

use std::{
    collections::HashMap,
    convert::{TryFrom, TryInto},
    fmt::Display,
};

use pallet_cf_vaults::CeremonyId;
use serde::{Deserialize, Serialize};

use super::{client_inner::MultisigMessage, SchnorrSignature};

use crate::signing::crypto::{
    build_challenge, BigInt, BigIntConverter, ECPoint, ECScalar, KeyShare, FE as Scalar,
    GE as Point,
};

use sha2::{Digest, Sha256};

/// A pair of secret single-use nonces (and their
/// corresponding public commitments). Correspond to (d,e)
/// generated during the preprocessing stage in Secion 5.3 (page 13)
// TODO: Not sure if it is a good idea to to make
// the secret values clonable
#[derive(Clone)]
pub struct SecretNoncePair {
    pub d: Scalar,
    pub d_pub: Point,
    pub e: Scalar,
    pub e_pub: Point,
}

impl SecretNoncePair {
    /// Generate a random pair of nonces
    pub fn sample_random() -> Self {
        let d = Scalar::new_random();
        let e = Scalar::new_random();

        let d_pub = Point::generator() * d;
        let e_pub = Point::generator() * e;

        SecretNoncePair { d, d_pub, e, e_pub }
    }
}

/// Public components of the single-use nonces generated by
/// a single party at signer index `index`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]

pub struct SigningCommitment {
    pub index: usize,
    pub d: Point,
    pub e: Point,
}

pub type Comm1 = SigningCommitment;

/// Data received by a single party for a given
/// stage from all parties (includes our own for
/// simplicity). Used for broadcast verification.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BroadcastVerificationMessage<T: Clone> {
    /// Data is expected to be ordered by signer_idx
    pub data: Vec<T>,
}

pub type VerifyComm2 = BroadcastVerificationMessage<Comm1>;
pub type VerifyLocalSig4 = BroadcastVerificationMessage<LocalSig3>;

/// Signature (the "response" part) shard generated by a single party
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LocalSig3 {
    pub response: Scalar,
}

macro_rules! derive_from_enum {
    ($variant: ty, $variant_path: path, $enum: ty) => {
        impl From<$variant> for $enum {
            fn from(x: $variant) -> Self {
                $variant_path(x)
            }
        }
    };
}

macro_rules! derive_try_from_variant {
    ($variant: ty, $variant_path: path, $enum: ty) => {
        impl TryFrom<$enum> for $variant {
            type Error = &'static str;

            fn try_from(data: $enum) -> Result<Self, Self::Error> {
                if let $variant_path(x) = data {
                    Ok(x)
                } else {
                    Err(stringify!($enum))
                }
            }
        }
    };
}

macro_rules! derive_impls_for_enum_variants {
    ($variant: ty, $variant_path: path, $enum: ty) => {
        derive_from_enum!($variant, $variant_path, $enum);
        derive_try_from_variant!($variant, $variant_path, $enum);
        derive_display_as_type_name!($variant);
    };
}

macro_rules! derive_impls_for_signing_data {
    ($variant: ty, $variant_path: path) => {
        derive_impls_for_enum_variants!($variant, $variant_path, SigningData);
    };
}

/// Data exchanged between parties during various stages
/// of the FROST signing protocol
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum SigningData {
    CommStage1(Comm1),
    BroadcastVerificationStage2(VerifyComm2),
    LocalSigStage3(LocalSig3),
    VerifyLocalSigsStage4(VerifyLocalSig4),
}

derive_impls_for_signing_data!(Comm1, SigningData::CommStage1);
derive_impls_for_signing_data!(VerifyComm2, SigningData::BroadcastVerificationStage2);
derive_impls_for_signing_data!(LocalSig3, SigningData::LocalSigStage3);
derive_impls_for_signing_data!(VerifyLocalSig4, SigningData::VerifyLocalSigsStage4);

impl Display for SigningData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = match self {
            SigningData::CommStage1(x) => x.to_string(),
            SigningData::BroadcastVerificationStage2(x) => x.to_string(),
            SigningData::LocalSigStage3(x) => x.to_string(),
            SigningData::VerifyLocalSigsStage4(x) => x.to_string(),
        };
        write!(f, "SigningData({})", inner)
    }
}

/// Helps identify data as used for
/// a specific signing ceremony
#[derive(Serialize, Deserialize, Debug)]
pub struct SigningDataWrapped {
    pub data: SigningData,
    pub ceremony_id: CeremonyId,
}

impl SigningDataWrapped {
    pub fn new<S>(data: S, ceremony_id: CeremonyId) -> Self
    where
        S: Into<SigningData>,
    {
        SigningDataWrapped {
            data: data.into(),
            ceremony_id,
        }
    }
}

impl From<SigningDataWrapped> for MultisigMessage {
    fn from(wrapped: SigningDataWrapped) -> Self {
        MultisigMessage::SigningMessage(wrapped)
    }
}

/// Combine individual commitments into group (schnorr) commitment.
/// See "Signing Protocol" in Section 5.2 (page 14).
fn gen_group_commitment(
    signing_commitments: &[SigningCommitment],
    bindings: &HashMap<usize, Scalar>,
) -> Point {
    signing_commitments
        .iter()
        .map(|comm| {
            let rho_i = bindings[&comm.index];
            comm.d + comm.e * rho_i
        })
        .reduce(|a, b| a + b)
        .expect("non empty list")
}

/// Generate a lagrange coefficient for party `signer_index`
/// according to Section 4 (page 9)
fn get_lagrange_coeff(
    signer_index: usize,
    all_signer_indices: &[usize],
) -> Result<Scalar, &'static str> {
    let mut num: Scalar = ECScalar::from(&BigInt::from(1));
    let mut den: Scalar = ECScalar::from(&BigInt::from(1));

    for j in all_signer_indices {
        if *j == signer_index {
            continue;
        }
        let j: Scalar = ECScalar::from(&BigInt::from(*j as u32));
        let signer_index: Scalar = ECScalar::from(&BigInt::from(signer_index as u32));
        num = num * j;
        den = den * (j.sub(&signer_index.get_element()));
    }

    if den == Scalar::zero() {
        return Err("Duplicate shares provided");
    }

    let lagrange_coeff = num * den.invert();

    Ok(lagrange_coeff)
}

/// Generate a "binding value" for party `index`. See "Signing Protocol" in Section 5.2 (page 14)
fn gen_rho_i(index: usize, msg: &[u8], signing_commitments: &[SigningCommitment]) -> Scalar {
    let mut hasher = Sha256::new();
    hasher.update(b"I");
    hasher.update(index.to_be_bytes());
    hasher.update(msg);

    for com in signing_commitments {
        hasher.update(com.index.to_be_bytes());
        hasher.update(com.d.get_element().serialize());
        hasher.update(com.e.get_element().serialize());
    }

    let result = hasher.finalize();

    let x: [u8; 32] = result.as_slice().try_into().expect("Invalid hash size");

    let x_bi = BigInt::from_bytes(&x);

    ECScalar::from(&x_bi)
}

type SigningResponse = LocalSig3;

/// Generate binding values for each party given their previously broadcast commitments
fn generate_bindings(msg: &[u8], commitments: &[SigningCommitment]) -> HashMap<usize, Scalar> {
    commitments
        .iter()
        .map(|c| (c.index, gen_rho_i(c.index, msg, commitments)))
        .collect()
}

/// Generate local signature/response (shard). See step 5 in Figure 3 (page 15).
pub fn generate_local_sig(
    msg: &[u8],
    key: &KeyShare,
    nonces: &SecretNoncePair,
    commitments: &[SigningCommitment],
    own_idx: usize,
    all_idxs: &[usize],
) -> SigningResponse {
    let bindings = generate_bindings(&msg, commitments);

    // This is `R` in a Schnorr signature
    let group_commitment = gen_group_commitment(&commitments, &bindings);

    let challenge = build_challenge(group_commitment.get_element(), key.y.get_element(), msg);

    let SecretNoncePair { d, e, .. } = nonces;

    let lambda_i = get_lagrange_coeff(own_idx, all_idxs).expect("lagrange coeff");

    let rho_i = bindings[&own_idx];

    let lhs = *d + (*e * rho_i);

    let response = lhs.sub(&(lambda_i * key.x_i * challenge).get_element());

    SigningResponse { response }
}

/// Check the validity of a signature response share.
/// (See step 7.b in Figure 3, page 15.)
fn is_party_resonse_valid(
    y_i: &Point,
    lambda_i: &Scalar,
    commitment: &Point,
    challenge: &Scalar,
    sig: &SigningResponse,
) -> bool {
    (Point::generator() * sig.response)
        == (commitment.sub_point(&(y_i * challenge * lambda_i).get_element()))
}

/// Combine local signatures received from all parties into the final
/// (aggregate) signature given that no party misbehavied. Otherwise
/// return the misbehaving parties.
pub fn aggregate_signature(
    msg: &[u8],
    signer_idxs: &[usize],
    agg_pubkey: Point,
    pubkeys: &[Point],
    commitments: &[SigningCommitment],
    responses: &[SigningResponse],
) -> Result<SchnorrSignature, Vec<usize>> {
    let bindings = generate_bindings(&msg, commitments);

    let group_commitment = gen_group_commitment(commitments, &bindings);

    let challenge = build_challenge(
        group_commitment.get_element(),
        agg_pubkey.get_element(),
        msg,
    );

    let mut invalid_idxs = vec![];

    for signer_idx in signer_idxs {
        let array_index = signer_idx - 1;

        let rho_i = bindings[&signer_idx];
        let lambda_i = get_lagrange_coeff(*signer_idx, signer_idxs).unwrap();

        let commitment = &commitments[array_index];
        let commitment_i = commitment.d + (commitment.e * rho_i);

        let y_i = pubkeys[array_index];

        let response = &responses[array_index];

        if !is_party_resonse_valid(&y_i, &lambda_i, &commitment_i, &challenge, &response) {
            invalid_idxs.push(*signer_idx);
            println!("A local signature is NOT valid!!!");
        }
    }

    if invalid_idxs.is_empty() {
        // Response shares/shards are additive, so we simply need to
        // add them together (see step 7.c in Figure 3, page 15).
        let z = responses
            .iter()
            .fold(Scalar::zero(), |acc, x| acc + x.response);

        Ok(SchnorrSignature {
            s: *z.get_element().as_ref(),
            r: group_commitment.get_element(),
        })
    } else {
        Err(invalid_idxs)
    }
}
