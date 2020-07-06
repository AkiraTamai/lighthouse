use super::{
    AggregateAndProof, Attestation, ChainSpec, Domain, EthSpec, Fork, Hash256, PublicKey,
    SecretKey, SelectionProof, Signature, SignedRoot,
};
use crate::test_utils::TestRandom;
use serde_derive::{Deserialize, Serialize};
use ssz_derive::{Decode, Encode};
use test_random_derive::TestRandom;
use tree_hash_derive::TreeHash;

/// A Validators signed aggregate proof to publish on the `beacon_aggregate_and_proof`
/// gossipsub topic.
///
/// Spec v0.12.1
#[cfg_attr(feature = "arbitrary-fuzz", derive(arbitrary::Arbitrary))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Encode, Decode, TestRandom, TreeHash)]
#[serde(bound = "T: EthSpec")]
pub struct SignedAggregateAndProof<T: EthSpec> {
    /// The `AggregateAndProof` that was signed.
    pub message: AggregateAndProof<T>,
    /// The aggregate attestation.
    pub signature: Signature,
}

impl<T: EthSpec> SignedAggregateAndProof<T> {
    /// Produces a new `SignedAggregateAndProof` with a `selection_proof` generated by signing
    /// `aggregate.data.slot` with `secret_key`.
    ///
    /// If `selection_proof.is_none()` it will be computed locally.
    pub fn from_aggregate(
        aggregator_index: u64,
        aggregate: Attestation<T>,
        selection_proof: Option<SelectionProof>,
        secret_key: &SecretKey,
        fork: &Fork,
        genesis_validators_root: Hash256,
        spec: &ChainSpec,
    ) -> Self {
        let message = AggregateAndProof::from_aggregate(
            aggregator_index,
            aggregate,
            selection_proof,
            secret_key,
            fork,
            genesis_validators_root,
            spec,
        );

        let target_epoch = message.aggregate.data.slot.epoch(T::slots_per_epoch());
        let domain = spec.get_domain(
            target_epoch,
            Domain::AggregateAndProof,
            fork,
            genesis_validators_root,
        );
        let signing_message = message.signing_root(domain);

        SignedAggregateAndProof {
            message,
            signature: secret_key.sign(signing_message),
        }
    }

    /// Verifies the signature of the `AggregateAndProof`
    pub fn is_valid_signature(
        &self,
        validator_pubkey: &PublicKey,
        fork: &Fork,
        genesis_validators_root: Hash256,
        spec: &ChainSpec,
    ) -> bool {
        let target_epoch = self.message.aggregate.data.slot.epoch(T::slots_per_epoch());
        let domain = spec.get_domain(
            target_epoch,
            Domain::AggregateAndProof,
            fork,
            genesis_validators_root,
        );
        let message = self.message.signing_root(domain);
        self.signature.verify(validator_pubkey, message)
    }

    /// Verifies the signature of the `AggregateAndProof` as well the underlying selection_proof in
    /// the contained `AggregateAndProof`.
    pub fn is_valid(
        &self,
        validator_pubkey: &PublicKey,
        fork: &Fork,
        genesis_validators_root: Hash256,
        spec: &ChainSpec,
    ) -> bool {
        self.is_valid_signature(validator_pubkey, fork, genesis_validators_root, spec)
            && self.message.is_valid_selection_proof(
                validator_pubkey,
                fork,
                genesis_validators_root,
                spec,
            )
    }
}
