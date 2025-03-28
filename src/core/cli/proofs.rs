use hashbrown::HashMap;
use p3_baby_bear::BabyBear;
use p3_field::{AbstractField, PrimeField32};
use serde::{Deserialize, Serialize};
use sp1_stark::baby_bear_poseidon2::BabyBearPoseidon2;
use sp1_stark::{
    Challenge, Com, MachineProof, OpeningProof, ShardCommitment, ShardOpenedValues, ShardProof, Val,
};

use crate::{
    core::{
        tag::Tag,
        zstore::{ZPtr, ZStore, DIGEST_SIZE, ZPTR_SIZE},
    },
    lair::{chipset::Chipset, provenance::DEPTH_W},
};

use super::{lurk_data::LurkData, microchain::CallableData, zdag::ZDag};

// TODO: replace this with SP1's ShardProof type directly?
#[derive(Serialize, Deserialize)]
struct CryptoShardProof {
    commitment: ShardCommitment<Com<BabyBearPoseidon2>>,
    opened_values: ShardOpenedValues<Val<BabyBearPoseidon2>, Challenge<BabyBearPoseidon2>>,
    opening_proof: OpeningProof<BabyBearPoseidon2>,
    chip_ordering: HashMap<String, usize>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct CryptoProof {
    shard_proofs: Vec<CryptoShardProof>,
    verifier_version: String,
    depth: u32,
}

type F = BabyBear;

#[inline]
pub(crate) fn get_verifier_version() -> &'static str {
    env!("VERGEN_GIT_SHA")
}

impl CryptoProof {
    #[inline]
    pub(crate) fn into_machine_proof(
        self,
        expr: &ZPtr<F>,
        env: &ZPtr<F>,
        result: &ZPtr<F>,
    ) -> MachineProof<BabyBearPoseidon2> {
        let mut public_values = Vec::with_capacity(40);
        public_values.extend(expr.flatten());
        public_values.extend(env.digest);
        public_values.extend(result.flatten());
        public_values.extend(self.depth.to_le_bytes().map(F::from_canonical_u8));
        let shard_proofs = self
            .shard_proofs
            .into_iter()
            .map(|csp| {
                let CryptoShardProof {
                    commitment,
                    opened_values,
                    opening_proof,
                    chip_ordering,
                } = csp;
                ShardProof {
                    commitment,
                    opened_values,
                    opening_proof,
                    chip_ordering,
                    public_values: public_values.clone(),
                }
            })
            .collect();
        MachineProof { shard_proofs }
    }

    #[inline]
    pub(crate) fn has_same_verifier_version(&self) -> bool {
        self.verifier_version == get_verifier_version()
    }
}

// The asserts/expects/unwraps in this impl are all internal and should always succeed.
#[allow(clippy::fallible_impl_from)]
impl From<MachineProof<BabyBearPoseidon2>> for CryptoProof {
    #[inline]
    fn from(value: MachineProof<BabyBearPoseidon2>) -> Self {
        let (shard_proofs, all_public_values) = value
            .shard_proofs
            .into_iter()
            .map(|sp| {
                let ShardProof {
                    commitment,
                    opened_values,
                    opening_proof,
                    chip_ordering,
                    public_values,
                    ..
                } = sp;
                (
                    CryptoShardProof {
                        commitment,
                        opened_values,
                        opening_proof,
                        chip_ordering,
                    },
                    public_values,
                )
            })
            .collect::<(Vec<_>, Vec<_>)>();
        let public_values = all_public_values.first().expect("must have public values");
        // sanity check: all shards have the same public values
        assert!(all_public_values.iter().all(|pv| pv == public_values));
        let depth_bytes = public_values[public_values.len() - DEPTH_W..]
            .iter()
            .cloned()
            .map(|x| {
                assert!(x <= F::from_canonical_u8(u8::MAX));
                x.as_canonical_u32() as u8
            })
            .collect::<Vec<_>>();
        let depth = u32::from_le_bytes(depth_bytes.try_into().unwrap());
        Self {
            shard_proofs,
            verifier_version: env!("VERGEN_GIT_SHA").to_string(),
            depth,
        }
    }
}

/// Carries a cryptographic proof and the Lurk data for its public values. This
/// proof format is meant for local caching through filesystem persistence. The
/// Lurk data for its public values is fully specified to support inspection.
#[derive(Serialize, Deserialize)]
pub(crate) struct CachedProof {
    pub(crate) crypto_proof: CryptoProof,
    pub(crate) expr: ZPtr<F>,
    pub(crate) env: ZPtr<F>,
    pub(crate) result: ZPtr<F>,
    pub(crate) zdag: ZDag<F>,
}

impl CachedProof {
    pub(crate) fn new<C: Chipset<F>>(
        crypto_proof: CryptoProof,
        public_values: &[F],
        zstore: &ZStore<F, C>,
    ) -> Self {
        let mut zdag = ZDag::default();
        let (expr_data, rest) = public_values.split_at(ZPTR_SIZE);
        let (env_digest, rest) = rest.split_at(DIGEST_SIZE);
        let (result_data, _rest) = rest.split_at(ZPTR_SIZE);
        let expr = ZPtr::from_flat_data(expr_data);
        let env = ZPtr::from_flat_digest(Tag::Env, env_digest);
        let result = ZPtr::from_flat_data(result_data);
        zdag.populate_with_many([&expr, &env, &result], zstore);
        Self {
            crypto_proof,
            expr,
            env,
            result,
            zdag,
        }
    }

    #[inline]
    pub(crate) fn into_machine_proof(self) -> MachineProof<BabyBearPoseidon2> {
        let Self {
            crypto_proof,
            expr,
            env,
            result,
            ..
        } = self;
        crypto_proof.into_machine_proof(&expr, &env, &result)
    }
}

#[derive(Serialize, Deserialize)]
pub(crate) struct ProtocolProof {
    pub(crate) crypto_proof: CryptoProof,
    pub(crate) args: LurkData<F>,
}

impl ProtocolProof {
    #[inline]
    pub(crate) fn new<C: Chipset<F>>(
        crypto_proof: CryptoProof,
        args: ZPtr<F>,
        zstore: &ZStore<F, C>,
    ) -> Self {
        Self {
            crypto_proof,
            args: LurkData::new(args, zstore),
        }
    }
}

/// A proof of state transition, with the Lurk data for the new state fully
/// specified and ready to be shared with parties wanting to continue the chain.
#[derive(Serialize, Deserialize)]
pub(crate) struct ChainProof {
    pub(crate) crypto_proof: CryptoProof,
    pub(crate) call_args: ZPtr<F>,
    pub(crate) next_chain_result: LurkData<F>,
    pub(crate) next_callable: CallableData,
}

/// A slightly smaller version of `ChainProof` meant to be kept as transition
/// record and shared for verification purposes.
#[derive(Serialize, Deserialize)]
pub(crate) struct OpaqueChainProof {
    pub(crate) crypto_proof: CryptoProof,
    pub(crate) call_args: ZPtr<F>,
    pub(crate) next_chain_result: ZPtr<F>,
    pub(crate) next_callable: ZPtr<F>,
}
