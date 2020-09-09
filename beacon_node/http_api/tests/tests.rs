use beacon_chain::{
    test_utils::{
        AttestationStrategy, BeaconChainHarness, BlockStrategy,
        BlockingMigratorEphemeralHarnessType,
    },
    BeaconChain, StateSkipConfig,
};
use environment::null_logger;
use eth2::{types::*, BeaconNodeClient, Url};
use http_api::{Config, Context};
use network::NetworkMessage;
use std::convert::TryInto;
use std::net::Ipv4Addr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use types::{
    test_utils::generate_deterministic_keypairs, BeaconState, Domain, EthSpec, Hash256, Keypair,
    MainnetEthSpec, RelativeEpoch, SignedRoot, Slot,
};

type E = MainnetEthSpec;

const SLOTS_PER_EPOCH: u64 = 32;
const VALIDATOR_COUNT: usize = SLOTS_PER_EPOCH as usize;
const CHAIN_LENGTH: u64 = SLOTS_PER_EPOCH * 5;
const JUSTIFIED_EPOCH: u64 = 4;
const FINALIZED_EPOCH: u64 = 3;

/// Skipping the slots around the epoch boundary allows us to check that we're obtaining states
/// from skipped slots for the finalized and justified checkpoints (instead of the state from the
/// block that those roots point to).
const SKIPPED_SLOTS: &[u64] = &[
    JUSTIFIED_EPOCH * SLOTS_PER_EPOCH - 1,
    JUSTIFIED_EPOCH * SLOTS_PER_EPOCH,
    FINALIZED_EPOCH * SLOTS_PER_EPOCH - 1,
    FINALIZED_EPOCH * SLOTS_PER_EPOCH,
];

struct ApiTester {
    chain: Arc<BeaconChain<BlockingMigratorEphemeralHarnessType<E>>>,
    client: BeaconNodeClient,
    next_block: SignedBeaconBlock<E>,
    attestations: Vec<Attestation<E>>,
    attester_slashing: AttesterSlashing<E>,
    proposer_slashing: ProposerSlashing,
    voluntary_exit: SignedVoluntaryExit,
    _server_shutdown: oneshot::Sender<()>,
    validator_keypairs: Vec<Keypair>,
    network_rx: mpsc::UnboundedReceiver<NetworkMessage<E>>,
}

impl ApiTester {
    pub fn new() -> Self {
        let mut harness = BeaconChainHarness::new(
            MainnetEthSpec,
            generate_deterministic_keypairs(VALIDATOR_COUNT),
        );

        harness.advance_slot();

        for _ in 0..CHAIN_LENGTH {
            let slot = harness.chain.slot().unwrap().as_u64();

            if !SKIPPED_SLOTS.contains(&slot) {
                harness.extend_chain(
                    1,
                    BlockStrategy::OnCanonicalHead,
                    AttestationStrategy::AllValidators,
                );
            }

            harness.advance_slot();
        }

        let head = harness.chain.head().unwrap();

        assert_eq!(
            harness.chain.slot().unwrap(),
            head.beacon_block.slot() + 1,
            "precondition: current slot is one after head"
        );

        let (next_block, _next_state) =
            harness.make_block(head.beacon_state.clone(), harness.chain.slot().unwrap());

        let attestations = harness
            .get_unaggregated_attestations(
                &AttestationStrategy::AllValidators,
                &head.beacon_state,
                head.beacon_block_root,
                harness.chain.slot().unwrap(),
            )
            .into_iter()
            .map(|vec| vec.into_iter().map(|(attestation, _subnet_id)| attestation))
            .flatten()
            .collect::<Vec<_>>();

        assert!(
            !attestations.is_empty(),
            "precondition: attestations for testing"
        );

        let attester_slashing = harness.make_attester_slashing(vec![0, 1]);
        let proposer_slashing = harness.make_proposer_slashing(2);
        let voluntary_exit = harness.make_voluntary_exit(
            3,
            harness.chain.epoch().unwrap() + harness.chain.spec.shard_committee_period,
        );

        let chain = Arc::new(harness.chain);

        assert_eq!(
            chain.head_info().unwrap().finalized_checkpoint.epoch,
            3,
            "precondition: finality"
        );
        assert_eq!(
            chain
                .head_info()
                .unwrap()
                .current_justified_checkpoint
                .epoch,
            4,
            "precondition: justification"
        );

        let (network_tx, network_rx) = mpsc::unbounded_channel();

        let context = Arc::new(Context {
            config: Config {
                enabled: true,
                listen_addr: Ipv4Addr::new(127, 0, 0, 1),
                listen_port: 0,
            },
            chain: Some(chain.clone()),
            network_tx: Some(network_tx),
            log: null_logger().unwrap(),
        });
        let ctx = context.clone();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let server_shutdown = async {
            // It's not really interesting why this triggered, just that it happened.
            let _ = shutdown_rx.await;
        };
        let (listening_socket, server) = http_api::serve(ctx, server_shutdown).unwrap();

        tokio::spawn(async { server.await });

        let client = BeaconNodeClient::new(
            Url::parse(&format!(
                "http://{}:{}",
                listening_socket.ip(),
                listening_socket.port()
            ))
            .unwrap(),
        )
        .unwrap();

        Self {
            chain,
            client,
            next_block,
            attestations,
            attester_slashing,
            proposer_slashing,
            voluntary_exit,
            _server_shutdown: shutdown_tx,
            validator_keypairs: harness.validators_keypairs,
            network_rx,
        }
    }

    fn interesting_state_ids(&self) -> Vec<StateId> {
        let mut ids = vec![
            StateId::Head,
            StateId::Genesis,
            StateId::Finalized,
            StateId::Justified,
            StateId::Slot(Slot::new(0)),
            StateId::Slot(Slot::new(32)),
            StateId::Slot(Slot::from(SKIPPED_SLOTS[0])),
            StateId::Slot(Slot::from(SKIPPED_SLOTS[1])),
            StateId::Slot(Slot::from(SKIPPED_SLOTS[2])),
            StateId::Slot(Slot::from(SKIPPED_SLOTS[3])),
            StateId::Root(Hash256::zero()),
        ];
        ids.push(StateId::Root(self.chain.head_info().unwrap().state_root));
        ids
    }

    fn interesting_block_ids(&self) -> Vec<BlockId> {
        let mut ids = vec![
            BlockId::Head,
            BlockId::Genesis,
            BlockId::Finalized,
            BlockId::Justified,
            BlockId::Slot(Slot::new(0)),
            BlockId::Slot(Slot::new(32)),
            BlockId::Slot(Slot::from(SKIPPED_SLOTS[0])),
            BlockId::Slot(Slot::from(SKIPPED_SLOTS[1])),
            BlockId::Slot(Slot::from(SKIPPED_SLOTS[2])),
            BlockId::Slot(Slot::from(SKIPPED_SLOTS[3])),
            BlockId::Root(Hash256::zero()),
        ];
        ids.push(BlockId::Root(self.chain.head_info().unwrap().block_root));
        ids
    }

    fn get_state(&self, state_id: StateId) -> Option<BeaconState<E>> {
        match state_id {
            StateId::Head => Some(self.chain.head().unwrap().beacon_state),
            StateId::Genesis => self
                .chain
                .get_state(&self.chain.genesis_state_root, None)
                .unwrap(),
            StateId::Finalized => {
                let finalized_slot = self
                    .chain
                    .head_info()
                    .unwrap()
                    .finalized_checkpoint
                    .epoch
                    .start_slot(E::slots_per_epoch());

                let root = self
                    .chain
                    .state_root_at_slot(finalized_slot)
                    .unwrap()
                    .unwrap();

                self.chain.get_state(&root, Some(finalized_slot)).unwrap()
            }
            StateId::Justified => {
                let justified_slot = self
                    .chain
                    .head_info()
                    .unwrap()
                    .current_justified_checkpoint
                    .epoch
                    .start_slot(E::slots_per_epoch());

                let root = self
                    .chain
                    .state_root_at_slot(justified_slot)
                    .unwrap()
                    .unwrap();

                self.chain.get_state(&root, Some(justified_slot)).unwrap()
            }
            StateId::Slot(slot) => {
                let root = self.chain.state_root_at_slot(slot).unwrap().unwrap();

                self.chain.get_state(&root, Some(slot)).unwrap()
            }
            StateId::Root(root) => self.chain.get_state(&root, None).unwrap(),
        }
    }

    pub fn increase_slot(self, count: u64) -> Self {
        self.chain
            .slot_clock
            .set_slot(self.chain.slot().unwrap().as_u64() + count);
        self
    }

    pub async fn test_beacon_genesis(self) -> Self {
        let result = self.client.get_beacon_genesis().await.unwrap().data;

        let state = self.chain.head().unwrap().beacon_state;
        let expected = GenesisData {
            genesis_time: state.genesis_time,
            genesis_validators_root: state.genesis_validators_root,
            genesis_fork_version: self.chain.spec.genesis_fork_version,
        };

        assert_eq!(result, expected);

        self
    }

    pub async fn test_beacon_states_root(self) -> Self {
        for state_id in self.interesting_state_ids() {
            let result = self
                .client
                .get_beacon_states_root(state_id)
                .await
                .unwrap()
                .map(|res| res.data.root);

            let expected = match state_id {
                StateId::Head => Some(self.chain.head_info().unwrap().state_root),
                StateId::Genesis => Some(self.chain.genesis_state_root),
                StateId::Finalized => {
                    let finalized_slot = self
                        .chain
                        .head_info()
                        .unwrap()
                        .finalized_checkpoint
                        .epoch
                        .start_slot(E::slots_per_epoch());

                    self.chain.state_root_at_slot(finalized_slot).unwrap()
                }
                StateId::Justified => {
                    let justified_slot = self
                        .chain
                        .head_info()
                        .unwrap()
                        .current_justified_checkpoint
                        .epoch
                        .start_slot(E::slots_per_epoch());

                    self.chain.state_root_at_slot(justified_slot).unwrap()
                }
                StateId::Slot(slot) => self.chain.state_root_at_slot(slot).unwrap(),
                StateId::Root(root) => Some(root),
            };

            assert_eq!(result, expected, "{:?}", state_id);
        }

        self
    }

    pub async fn test_beacon_states_fork(self) -> Self {
        for state_id in self.interesting_state_ids() {
            let result = self
                .client
                .get_beacon_states_fork(state_id)
                .await
                .unwrap()
                .map(|res| res.data);

            let expected = self.get_state(state_id).map(|state| state.fork);

            assert_eq!(result, expected, "{:?}", state_id);
        }

        self
    }

    pub async fn test_beacon_states_finality_checkpoints(self) -> Self {
        for state_id in self.interesting_state_ids() {
            let result = self
                .client
                .get_beacon_states_finality_checkpoints(state_id)
                .await
                .unwrap()
                .map(|res| res.data);

            let expected = self
                .get_state(state_id)
                .map(|state| FinalityCheckpointsData {
                    previous_justified: state.previous_justified_checkpoint,
                    current_justified: state.current_justified_checkpoint,
                    finalized: state.finalized_checkpoint,
                });

            assert_eq!(result, expected, "{:?}", state_id);
        }

        self
    }

    pub async fn test_beacon_states_validators(self) -> Self {
        for state_id in self.interesting_state_ids() {
            let result = self
                .client
                .get_beacon_states_validators(state_id)
                .await
                .unwrap()
                .map(|res| res.data);

            let expected = self.get_state(state_id).map(|state| {
                let epoch = state.current_epoch();
                let finalized_epoch = state.finalized_checkpoint.epoch;
                let far_future_epoch = self.chain.spec.far_future_epoch;

                let mut validators = Vec::with_capacity(state.validators.len());

                for i in 0..state.validators.len() {
                    let validator = state.validators[i].clone();

                    validators.push(ValidatorData {
                        index: i as u64,
                        balance: state.balances[i],
                        status: ValidatorStatus::from_validator(
                            Some(&validator),
                            epoch,
                            finalized_epoch,
                            far_future_epoch,
                        ),
                        validator,
                    })
                }

                validators
            });

            assert_eq!(result, expected, "{:?}", state_id);
        }

        self
    }

    pub async fn test_beacon_states_validator_id(self) -> Self {
        for state_id in self.interesting_state_ids() {
            let state_opt = self.get_state(state_id);
            let validators = match state_opt.as_ref() {
                Some(state) => state.validators.clone().into(),
                None => vec![],
            };

            for (i, validator) in validators.into_iter().enumerate() {
                let validator_ids = &[
                    ValidatorId::PublicKey(validator.pubkey.clone()),
                    ValidatorId::Index(i as u64),
                ];

                for validator_id in validator_ids {
                    let result = self
                        .client
                        .get_beacon_states_validator_id(state_id, validator_id)
                        .await
                        .unwrap()
                        .map(|res| res.data);

                    if result.is_none() && state_opt.is_none() {
                        continue;
                    }

                    let state = state_opt.as_ref().expect("result should be none");

                    let expected = {
                        let epoch = state.current_epoch();
                        let finalized_epoch = state.finalized_checkpoint.epoch;
                        let far_future_epoch = self.chain.spec.far_future_epoch;

                        ValidatorData {
                            index: i as u64,
                            balance: state.balances[i],
                            status: ValidatorStatus::from_validator(
                                Some(&validator),
                                epoch,
                                finalized_epoch,
                                far_future_epoch,
                            ),
                            validator: validator.clone(),
                        }
                    };

                    assert_eq!(result, Some(expected), "{:?}, {:?}", state_id, validator_id);
                }
            }
        }

        self
    }

    pub async fn test_beacon_states_committees(self) -> Self {
        for state_id in self.interesting_state_ids() {
            let mut state_opt = self.get_state(state_id);

            let epoch = state_opt
                .as_ref()
                .map(|state| state.current_epoch())
                .unwrap_or_else(|| Epoch::new(0));

            let results = self
                .client
                .get_beacon_states_committees(state_id, epoch, None, None)
                .await
                .unwrap()
                .map(|res| res.data);

            if results.is_none() && state_opt.is_none() {
                continue;
            }

            let state = state_opt.as_mut().expect("result should be none");
            state.build_all_committee_caches(&self.chain.spec).unwrap();
            let committees = state
                .get_beacon_committees_at_epoch(
                    RelativeEpoch::from_epoch(state.current_epoch(), epoch).unwrap(),
                )
                .unwrap();

            for (i, result) in results.unwrap().into_iter().enumerate() {
                let expected = &committees[i];

                assert_eq!(result.index, expected.index, "{}", state_id);
                assert_eq!(result.slot, expected.slot, "{}", state_id);
                assert_eq!(
                    result
                        .validators
                        .into_iter()
                        .map(|i| i as usize)
                        .collect::<Vec<_>>(),
                    expected.committee.to_vec(),
                    "{}",
                    state_id
                );
            }
        }

        self
    }

    fn get_block_root(&self, block_id: BlockId) -> Option<Hash256> {
        match block_id {
            BlockId::Head => Some(self.chain.head_info().unwrap().block_root),
            BlockId::Genesis => Some(self.chain.genesis_block_root),
            BlockId::Finalized => Some(self.chain.head_info().unwrap().finalized_checkpoint.root),
            BlockId::Justified => Some(
                self.chain
                    .head_info()
                    .unwrap()
                    .current_justified_checkpoint
                    .root,
            ),
            BlockId::Slot(slot) => self.chain.block_root_at_slot(slot).unwrap(),
            BlockId::Root(root) => Some(root),
        }
    }

    fn get_block(&self, block_id: BlockId) -> Option<SignedBeaconBlock<E>> {
        let root = self.get_block_root(block_id);
        root.and_then(|root| self.chain.get_block(&root).unwrap())
    }

    pub async fn test_beacon_headers_all_slots(self) -> Self {
        for slot in 0..CHAIN_LENGTH {
            let slot = Slot::from(slot);

            let result = self
                .client
                .get_beacon_headers(Some(slot), None)
                .await
                .unwrap()
                .map(|res| res.data);

            let root = self.chain.block_root_at_slot(slot).unwrap();

            if root.is_none() && result.is_none() {
                continue;
            }

            let root = root.unwrap();
            let block = self.chain.block_at_slot(slot).unwrap().unwrap();
            let header = BlockHeaderData {
                root,
                canonical: true,
                header: BlockHeaderAndSignature {
                    message: block.message.block_header(),
                    signature: block.signature.into(),
                },
            };
            let expected = vec![header];

            assert_eq!(result.unwrap(), expected, "slot {:?}", slot);
        }

        self
    }

    pub async fn test_beacon_headers_all_parents(self) -> Self {
        let mut roots = self
            .chain
            .rev_iter_block_roots()
            .unwrap()
            .map(Result::unwrap)
            .map(|(root, _slot)| root)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>();

        // The iterator natively returns duplicate roots for skipped slots.
        roots.dedup();

        for i in 1..roots.len() {
            let parent_root = roots[i - 1];
            let child_root = roots[i];

            let result = self
                .client
                .get_beacon_headers(None, Some(parent_root))
                .await
                .unwrap()
                .unwrap()
                .data;

            assert_eq!(result.len(), 1, "i {}", i);
            assert_eq!(result[0].root, child_root, "i {}", i);
        }

        self
    }

    pub async fn test_beacon_headers_block_id(self) -> Self {
        for block_id in self.interesting_block_ids() {
            let result = self
                .client
                .get_beacon_headers_block_id(block_id)
                .await
                .unwrap()
                .map(|res| res.data);

            let block_root_opt = self.get_block_root(block_id);

            let block_opt = block_root_opt.and_then(|root| self.chain.get_block(&root).unwrap());

            if block_opt.is_none() && result.is_none() {
                continue;
            }

            let result = result.unwrap();
            let block = block_opt.unwrap();
            let block_root = block_root_opt.unwrap();
            let canonical = self
                .chain
                .block_root_at_slot(block.slot())
                .unwrap()
                .map_or(false, |canonical| block_root == canonical);

            assert_eq!(result.canonical, canonical, "{:?}", block_id);
            assert_eq!(result.root, block_root, "{:?}", block_id);
            assert_eq!(
                result.header.message,
                block.message.block_header(),
                "{:?}",
                block_id
            );
            assert_eq!(
                result.header.signature,
                block.signature.into(),
                "{:?}",
                block_id
            );
        }

        self
    }

    pub async fn test_beacon_blocks_root(self) -> Self {
        for block_id in self.interesting_block_ids() {
            let result = self
                .client
                .get_beacon_blocks_root(block_id)
                .await
                .unwrap()
                .map(|res| res.data.root);

            let expected = self.get_block_root(block_id);

            assert_eq!(result, expected, "{:?}", block_id);
        }

        self
    }

    pub async fn test_post_beacon_blocks_valid(mut self) -> Self {
        let next_block = &self.next_block;

        self.client.post_beacon_blocks(next_block).await.unwrap();

        assert!(
            self.network_rx.try_recv().is_ok(),
            "valid blocks should be sent to network"
        );

        self
    }

    pub async fn test_post_beacon_blocks_invalid(mut self) -> Self {
        let mut next_block = self.next_block.clone();
        next_block.message.proposer_index += 1;

        assert!(self.client.post_beacon_blocks(&next_block).await.is_err());

        assert!(
            self.network_rx.try_recv().is_ok(),
            "invalid blocks should be sent to network"
        );

        self
    }

    pub async fn test_beacon_blocks(self) -> Self {
        for block_id in self.interesting_block_ids() {
            let result = self
                .client
                .get_beacon_blocks(block_id)
                .await
                .unwrap()
                .map(|res| res.data);

            let expected = self.get_block(block_id);

            assert_eq!(result, expected, "{:?}", block_id);
        }

        self
    }

    pub async fn test_beacon_blocks_attestations(self) -> Self {
        for block_id in self.interesting_block_ids() {
            let result = self
                .client
                .get_beacon_blocks_attestations(block_id)
                .await
                .unwrap()
                .map(|res| res.data);

            let expected = self
                .get_block(block_id)
                .map(|block| block.message.body.attestations.into());

            assert_eq!(result, expected, "{:?}", block_id);
        }

        self
    }

    pub async fn test_post_beacon_pool_attestations_valid(mut self) -> Self {
        for attestation in &self.attestations {
            self.client
                .post_beacon_pool_attestations(attestation)
                .await
                .unwrap();

            assert!(
                self.network_rx.try_recv().is_ok(),
                "valid attestation should be sent to network"
            );
        }

        self
    }

    pub async fn test_post_beacon_pool_attestations_invalid(mut self) -> Self {
        for attestation in &self.attestations {
            let mut attestation = attestation.clone();
            attestation.data.slot += 1;

            assert!(self
                .client
                .post_beacon_pool_attestations(&attestation)
                .await
                .is_err());

            assert!(
                self.network_rx.try_recv().is_err(),
                "invalid attestation should not be sent to network"
            );
        }

        self
    }

    pub async fn test_get_beacon_pool_attestations(self) -> Self {
        let result = self
            .client
            .get_beacon_pool_attestations()
            .await
            .unwrap()
            .data;

        let mut expected = self.chain.op_pool.get_all_attestations();
        expected.extend(self.chain.naive_aggregation_pool.read().iter().cloned());

        assert_eq!(result, expected);

        self
    }

    pub async fn test_post_beacon_pool_attester_slashings_valid(mut self) -> Self {
        self.client
            .post_beacon_pool_attester_slashings(&self.attester_slashing)
            .await
            .unwrap();

        assert!(
            self.network_rx.try_recv().is_ok(),
            "valid attester slashing should be sent to network"
        );

        self
    }

    pub async fn test_post_beacon_pool_attester_slashings_invalid(mut self) -> Self {
        let mut slashing = self.attester_slashing.clone();
        slashing.attestation_1.data.slot += 1;

        self.client
            .post_beacon_pool_attester_slashings(&slashing)
            .await
            .unwrap_err();

        assert!(
            self.network_rx.try_recv().is_err(),
            "invalid attester slashing should not be sent to network"
        );

        self
    }

    pub async fn test_get_beacon_pool_attester_slashings(self) -> Self {
        let result = self
            .client
            .get_beacon_pool_attester_slashings()
            .await
            .unwrap()
            .data;

        let expected = self.chain.op_pool.get_all_attester_slashings();

        assert_eq!(result, expected);

        self
    }

    pub async fn test_post_beacon_pool_proposer_slashings_valid(mut self) -> Self {
        self.client
            .post_beacon_pool_proposer_slashings(&self.proposer_slashing)
            .await
            .unwrap();

        assert!(
            self.network_rx.try_recv().is_ok(),
            "valid proposer slashing should be sent to network"
        );

        self
    }

    pub async fn test_post_beacon_pool_proposer_slashings_invalid(mut self) -> Self {
        let mut slashing = self.proposer_slashing.clone();
        slashing.signed_header_1.message.slot += 1;

        self.client
            .post_beacon_pool_proposer_slashings(&slashing)
            .await
            .unwrap_err();

        assert!(
            self.network_rx.try_recv().is_err(),
            "invalid proposer slashing should not be sent to network"
        );

        self
    }

    pub async fn test_get_beacon_pool_proposer_slashings(self) -> Self {
        let result = self
            .client
            .get_beacon_pool_proposer_slashings()
            .await
            .unwrap()
            .data;

        let expected = self.chain.op_pool.get_all_proposer_slashings();

        assert_eq!(result, expected);

        self
    }

    pub async fn test_post_beacon_pool_voluntary_exits_valid(mut self) -> Self {
        self.client
            .post_beacon_pool_voluntary_exits(&self.voluntary_exit)
            .await
            .unwrap();

        assert!(
            self.network_rx.try_recv().is_ok(),
            "valid exit should be sent to network"
        );

        self
    }

    pub async fn test_post_beacon_pool_voluntary_exits_invalid(mut self) -> Self {
        let mut exit = self.voluntary_exit.clone();
        exit.message.epoch += 1;

        self.client
            .post_beacon_pool_voluntary_exits(&exit)
            .await
            .unwrap_err();

        assert!(
            self.network_rx.try_recv().is_err(),
            "invalid exit should not be sent to network"
        );

        self
    }

    pub async fn test_get_beacon_pool_voluntary_exits(self) -> Self {
        let result = self
            .client
            .get_beacon_pool_voluntary_exits()
            .await
            .unwrap()
            .data;

        let expected = self.chain.op_pool.get_all_voluntary_exits();

        assert_eq!(result, expected);

        self
    }

    pub async fn test_get_config_fork_schedule(self) -> Self {
        let result = self.client.get_config_fork_schedule().await.unwrap().data;

        let expected = vec![self.chain.head_info().unwrap().fork];

        assert_eq!(result, expected);

        self
    }

    pub async fn test_get_config_spec(self) -> Self {
        let result = self.client.get_config_spec().await.unwrap().data;

        let expected = YamlConfig::from_spec::<E>(&self.chain.spec);

        assert_eq!(result, expected);

        self
    }

    pub async fn test_get_config_deposit_contract(self) -> Self {
        let result = self
            .client
            .get_config_deposit_contract()
            .await
            .unwrap()
            .data;

        let expected = DepositContractData {
            address: self.chain.spec.deposit_contract_address,
            chain_id: eth1::DEFAULT_NETWORK_ID.into(),
        };

        assert_eq!(result, expected);

        self
    }

    pub async fn test_get_debug_beacon_states(self) -> Self {
        for state_id in self.interesting_state_ids() {
            let result = self
                .client
                .get_debug_beacon_states(state_id)
                .await
                .unwrap()
                .map(|res| res.data);

            let mut expected = self.get_state(state_id);
            expected.as_mut().map(|state| state.drop_all_caches());

            assert_eq!(result, expected, "{:?}", state_id);
        }

        self
    }

    pub async fn test_get_debug_beacon_heads(self) -> Self {
        let result = self
            .client
            .get_debug_beacon_heads()
            .await
            .unwrap()
            .data
            .into_iter()
            .map(|head| (head.root, head.slot))
            .collect::<Vec<_>>();

        let expected = self.chain.heads();

        assert_eq!(result, expected);

        self
    }

    fn validator_count(&self) -> usize {
        self.chain.head().unwrap().beacon_state.validators.len()
    }

    fn interesting_validator_indices(&self) -> Vec<Vec<u64>> {
        let validator_count = self.validator_count() as u64;

        let mut interesting = vec![
            vec![],
            vec![0],
            vec![0, 1],
            vec![0, 1, 3],
            vec![validator_count],
            vec![validator_count, 1],
            vec![validator_count, 1, 3],
            vec![u64::max_value()],
            vec![u64::max_value(), 1],
            vec![u64::max_value(), 1, 3],
        ];

        interesting.push((0..validator_count).collect());

        interesting
    }

    pub async fn test_get_validator_duties_attester(self) -> Self {
        let current_epoch = self.chain.epoch().unwrap().as_u64();

        let half = current_epoch / 2;
        let first = current_epoch - half;
        let last = current_epoch + half;

        for epoch in first..=last {
            for indices in self.interesting_validator_indices() {
                let epoch = Epoch::from(epoch);

                // The endpoint does not allow getting duties older than the previous epoch.
                if epoch + 1 < current_epoch {
                    assert_eq!(
                        self.client
                            .get_validator_duties_attester(epoch, Some(&indices))
                            .await
                            .unwrap_err()
                            .status()
                            .map(Into::into),
                        Some(400)
                    );
                    continue;
                }

                let results = self
                    .client
                    .get_validator_duties_attester(epoch, Some(&indices))
                    .await
                    .unwrap()
                    .data;

                let mut state = self
                    .chain
                    .state_at_slot(
                        epoch.start_slot(E::slots_per_epoch()),
                        StateSkipConfig::WithStateRoots,
                    )
                    .unwrap();
                state
                    .build_committee_cache(RelativeEpoch::Current, &self.chain.spec)
                    .unwrap();

                let expected_len = indices
                    .iter()
                    .filter(|i| **i < state.validators.len() as u64)
                    .count();

                assert_eq!(results.len(), expected_len);

                for (indices_set, &i) in indices.iter().enumerate() {
                    if let Some(duty) = state
                        .get_attestation_duties(i as usize, RelativeEpoch::Current)
                        .unwrap()
                    {
                        let expected = AttesterData {
                            pubkey: state.validators[i as usize].pubkey.clone().into(),
                            validator_index: i,
                            committee_index: duty.index,
                            committee_length: duty.committee_len as u64,
                            validator_committee_index: duty.committee_position as u64,
                            slot: duty.slot,
                        };

                        let result = results
                            .iter()
                            .find(|duty| duty.validator_index == i)
                            .unwrap();

                        assert_eq!(
                            *result, expected,
                            "epoch: {}, indices_set: {}",
                            epoch, indices_set
                        );
                    } else {
                        assert!(
                            !results.iter().any(|duty| duty.validator_index == i),
                            "validator index should not exist in response"
                        );
                    }
                }
            }
        }

        // TODO: check empty query param.

        self
    }

    pub async fn test_get_validator_duties_proposer(self) -> Self {
        let current_epoch = self.chain.epoch().unwrap();

        let result = self
            .client
            .get_validator_duties_proposer(current_epoch)
            .await
            .unwrap()
            .data;

        let mut state = self.chain.head_beacon_state().unwrap();
        state
            .build_committee_cache(RelativeEpoch::Current, &self.chain.spec)
            .unwrap();

        let expected = current_epoch
            .slot_iter(E::slots_per_epoch())
            .map(|slot| {
                let index = state
                    .get_beacon_proposer_index(slot, &self.chain.spec)
                    .unwrap();
                let pubkey = state.validators[index].pubkey.clone().into();

                ProposerData { pubkey, slot }
            })
            .collect::<Vec<_>>();

        assert_eq!(result, expected);

        self
    }

    pub async fn test_block_production(self) -> Self {
        let fork = self.chain.head_info().unwrap().fork;
        let genesis_validators_root = self.chain.genesis_validators_root;

        for _ in 0..E::slots_per_epoch() * 3 {
            let slot = self.chain.slot().unwrap();
            let epoch = self.chain.epoch().unwrap();

            let proposer_pubkey_bytes = self
                .client
                .get_validator_duties_proposer(epoch)
                .await
                .unwrap()
                .data
                .into_iter()
                .find(|duty| duty.slot == slot)
                .map(|duty| duty.pubkey)
                .unwrap();
            let proposer_pubkey = (&proposer_pubkey_bytes).try_into().unwrap();

            let sk = self
                .validator_keypairs
                .iter()
                .find(|kp| kp.pk == proposer_pubkey)
                .map(|kp| kp.sk.clone())
                .unwrap();

            let randao_reveal = {
                let domain = self.chain.spec.get_domain(
                    epoch,
                    Domain::Randao,
                    &fork,
                    genesis_validators_root,
                );
                let message = epoch.signing_root(domain);
                sk.sign(message).into()
            };

            let block = self
                .client
                .get_validator_blocks::<E>(slot, randao_reveal, None)
                .await
                .unwrap()
                .data;

            let signed_block = block.sign(&sk, &fork, genesis_validators_root, &self.chain.spec);

            self.client.post_beacon_blocks(&signed_block).await.unwrap();

            assert_eq!(self.chain.head_beacon_block().unwrap(), signed_block);

            self.chain.slot_clock.set_slot(slot.as_u64() + 1);
        }

        self
    }
}

#[tokio::test(core_threads = 2)]
async fn beacon_genesis() {
    ApiTester::new().test_beacon_genesis().await;
}

#[tokio::test(core_threads = 2)]
async fn beacon_states_root() {
    ApiTester::new().test_beacon_states_root().await;
}

#[tokio::test(core_threads = 2)]
async fn beacon_states_fork() {
    ApiTester::new().test_beacon_states_fork().await;
}

#[tokio::test(core_threads = 2)]
async fn beacon_states_finality_checkpoints() {
    ApiTester::new()
        .test_beacon_states_finality_checkpoints()
        .await;
}

#[tokio::test(core_threads = 2)]
async fn beacon_states_validators() {
    ApiTester::new().test_beacon_states_validators().await;
}

#[tokio::test(core_threads = 2)]
async fn beacon_states_committees() {
    ApiTester::new().test_beacon_states_committees().await;
}

#[tokio::test(core_threads = 2)]
async fn beacon_states_validator_id() {
    ApiTester::new().test_beacon_states_validator_id().await;
}

#[tokio::test(core_threads = 2)]
async fn beacon_headers() {
    ApiTester::new()
        .test_beacon_headers_all_slots()
        .await
        .test_beacon_headers_all_parents()
        .await;
}

#[tokio::test(core_threads = 2)]
async fn beacon_headers_block_id() {
    ApiTester::new().test_beacon_headers_block_id().await;
}

#[tokio::test(core_threads = 2)]
async fn beacon_blocks() {
    ApiTester::new().test_beacon_blocks().await;
}

#[tokio::test(core_threads = 2)]
async fn post_beacon_blocks_valid() {
    ApiTester::new().test_post_beacon_blocks_valid().await;
}

#[tokio::test(core_threads = 2)]
async fn post_beacon_blocks_invalid() {
    ApiTester::new().test_post_beacon_blocks_invalid().await;
}

#[tokio::test(core_threads = 2)]
async fn beacon_blocks_root() {
    ApiTester::new().test_beacon_blocks_root().await;
}

#[tokio::test(core_threads = 2)]
async fn beacon_blocks_attestations() {
    ApiTester::new().test_beacon_blocks_attestations().await;
}

#[tokio::test(core_threads = 2)]
async fn beacon_pools_get() {
    ApiTester::new()
        .test_get_beacon_pool_attestations()
        .await
        .test_get_beacon_pool_attester_slashings()
        .await
        .test_get_beacon_pool_proposer_slashings()
        .await
        .test_get_beacon_pool_voluntary_exits()
        .await;
}

#[tokio::test(core_threads = 2)]
async fn beacon_pools_post_attestations_valid() {
    ApiTester::new()
        .test_post_beacon_pool_attestations_valid()
        .await;
}

#[tokio::test(core_threads = 2)]
async fn beacon_pools_post_attestations_invalid() {
    ApiTester::new()
        .test_post_beacon_pool_attestations_invalid()
        .await;
}

#[tokio::test(core_threads = 2)]
async fn beacon_pools_post_attester_slashings_valid() {
    ApiTester::new()
        .test_post_beacon_pool_attester_slashings_valid()
        .await;
}

#[tokio::test(core_threads = 2)]
async fn beacon_pools_post_attester_slashings_invalid() {
    ApiTester::new()
        .test_post_beacon_pool_attester_slashings_invalid()
        .await;
}

#[tokio::test(core_threads = 2)]
async fn beacon_pools_post_proposer_slashings_valid() {
    ApiTester::new()
        .test_post_beacon_pool_proposer_slashings_valid()
        .await;
}

#[tokio::test(core_threads = 2)]
async fn beacon_pools_post_proposer_slashings_invalid() {
    ApiTester::new()
        .test_post_beacon_pool_proposer_slashings_invalid()
        .await;
}

#[tokio::test(core_threads = 2)]
async fn beacon_pools_post_voluntary_exits_valid() {
    let shard_committee_period = E::default_spec().shard_committee_period;

    ApiTester::new()
        // Prevents a "too young to exit" error.
        .increase_slot(shard_committee_period * SLOTS_PER_EPOCH)
        .test_post_beacon_pool_voluntary_exits_valid()
        .await;
}

#[tokio::test(core_threads = 2)]
async fn beacon_pools_post_voluntary_exits_invalid() {
    ApiTester::new()
        .test_post_beacon_pool_voluntary_exits_invalid()
        .await;
}

#[tokio::test(core_threads = 2)]
async fn config_get() {
    ApiTester::new()
        .test_get_config_fork_schedule()
        .await
        .test_get_config_spec()
        .await
        .test_get_config_deposit_contract()
        .await;
}

#[tokio::test(core_threads = 2)]
async fn debug_get() {
    ApiTester::new()
        .test_get_debug_beacon_states()
        .await
        .test_get_debug_beacon_heads()
        .await;
}

#[tokio::test(core_threads = 2)]
async fn get_validator_duties_attester() {
    ApiTester::new().test_get_validator_duties_attester().await;
}

#[tokio::test(core_threads = 2)]
async fn get_validator_duties_proposer() {
    ApiTester::new().test_get_validator_duties_proposer().await;
}

#[tokio::test(core_threads = 2)]
async fn block_production() {
    ApiTester::new().test_block_production().await;
}
