use slashing_protection::interchange::{
    Interchange, InterchangeData, InterchangeFormat, InterchangeMetadata, MinimalInterchangeData,
};
use slashing_protection::test_utils::pubkey;
use slashing_protection::{SlashingDatabase, SUPPORTED_INTERCHANGE_FORMAT_VERSION};
use tempfile::tempdir;
use types::{Epoch, Hash256, PublicKey, Slot};

#[test]
fn import_increases_lower_bound() {
    let import_data = vec![MinimalInterchangeData {
        pubkey: pubkey(0),
        last_signed_block_slot: Some(Slot::new(127)),
        last_signed_attestation_source_epoch: Some(Epoch::new(3)),
        last_signed_attestation_target_epoch: Some(Epoch::new(4)),
    }];

    export_test(
        vec![(pubkey(0), 126, 0)],
        vec![
            (pubkey(0), 0, 1, 0),
            (pubkey(0), 1, 2, 0),
            (pubkey(0), 2, 3, 1),
        ],
        import_data.clone(),
        vec![],
        vec![],
        import_data.clone(),
    );
}

#[test]
fn attestations_and_blocks_increase_lower_bound() {
    export_test(
        vec![],
        vec![
            (pubkey(0), 0, 1, 0),
            (pubkey(0), 1, 2, 0),
            (pubkey(0), 2, 3, 1),
        ],
        vec![MinimalInterchangeData {
            pubkey: pubkey(0),
            last_signed_block_slot: None,
            last_signed_attestation_source_epoch: Some(Epoch::new(3)),
            last_signed_attestation_target_epoch: Some(Epoch::new(4)),
        }],
        vec![(pubkey(0), 245, 0)],
        vec![
            (pubkey(0), 4, 5, 0),
            (pubkey(0), 5, 6, 0),
            (pubkey(0), 6, 8, 0),
        ],
        vec![MinimalInterchangeData {
            pubkey: pubkey(0),
            last_signed_block_slot: Some(Slot::new(245)),
            last_signed_attestation_source_epoch: Some(Epoch::new(6)),
            last_signed_attestation_target_epoch: Some(Epoch::new(8)),
        }],
    );
}

#[test]
fn multi_mix() {
    export_test(
        vec![(pubkey(1), 2, 0)],
        vec![
            (pubkey(0), 0, 1, 0),
            (pubkey(1), 0, 1, 0),
            (pubkey(0), 2, 3, 1),
            (pubkey(0), 3, 4, 1),
        ],
        vec![
            MinimalInterchangeData {
                pubkey: pubkey(0),
                last_signed_block_slot: Some(Slot::new(255)),
                last_signed_attestation_source_epoch: None,
                last_signed_attestation_target_epoch: None,
            },
            MinimalInterchangeData {
                pubkey: pubkey(1),
                last_signed_block_slot: None,
                last_signed_attestation_source_epoch: Some(Epoch::new(3)),
                last_signed_attestation_target_epoch: Some(Epoch::new(4)),
            },
        ],
        vec![(pubkey(0), 299, 0), (pubkey(1), 307, 0)],
        vec![
            (pubkey(1), 4, 5, 0),
            (pubkey(1), 5, 6, 0),
            (pubkey(1), 6, 7, 0),
        ],
        vec![
            MinimalInterchangeData {
                pubkey: pubkey(0),
                last_signed_block_slot: Some(Slot::new(299)),
                last_signed_attestation_source_epoch: Some(Epoch::new(3)),
                last_signed_attestation_target_epoch: Some(Epoch::new(4)),
            },
            MinimalInterchangeData {
                pubkey: pubkey(1),
                last_signed_block_slot: Some(Slot::new(307)),
                last_signed_attestation_source_epoch: Some(Epoch::new(6)),
                last_signed_attestation_target_epoch: Some(Epoch::new(7)),
            },
        ],
    );
}

fn export_test(
    // Blocks to apply before import as `(pubkey, slot, block_root)`
    pre_blocks: Vec<(PublicKey, u64, u64)>,
    // Attestations to apply before import, as `(pubkey, source, target, block_root)`
    pre_attestations: Vec<(PublicKey, u64, u64, u64)>,
    import_data: Vec<MinimalInterchangeData>,
    post_blocks: Vec<(PublicKey, u64, u64)>,
    post_attestations: Vec<(PublicKey, u64, u64, u64)>,
    expected: Vec<MinimalInterchangeData>,
) {
    let dir = tempdir().unwrap();
    let slashing_db_file = dir.path().join("slashing_protection.sqlite");
    let slashing_db = SlashingDatabase::create(&slashing_db_file).unwrap();

    let genesis_validators_root = Hash256::from_low_u64_be(66);

    let apply_attestations = |attestations| {
        for (public_key, source, target, block_root) in attestations {
            slashing_db
                .check_and_insert_attestation_signing_root(
                    &public_key,
                    Epoch::new(source),
                    Epoch::new(target),
                    Hash256::from_low_u64_be(block_root),
                )
                .unwrap();
        }
    };
    let apply_blocks = |blocks| {
        for (public_key, slot, block_root) in blocks {
            slashing_db
                .check_and_insert_block_signing_root(
                    &public_key,
                    Slot::new(slot),
                    Hash256::from_low_u64_be(block_root),
                )
                .unwrap();
        }
    };

    // Register all validators.
    slashing_db
        .register_validators(
            pre_attestations
                .iter()
                .chain(post_attestations.iter())
                .map(|(pubkey, _, _, _)| pubkey)
                .chain(
                    pre_blocks
                        .iter()
                        .chain(post_blocks.iter())
                        .map(|(pubkey, _, _)| pubkey),
                ),
        )
        .unwrap();

    // Apply pre blocks.
    apply_blocks(pre_blocks);

    // Apply pre attestations.
    apply_attestations(pre_attestations);

    // Import minimal interchange data.
    let metadata = InterchangeMetadata {
        interchange_format: InterchangeFormat::Minimal,
        interchange_format_version: SUPPORTED_INTERCHANGE_FORMAT_VERSION,
        genesis_validators_root,
    };

    let to_import = Interchange {
        metadata: metadata.clone(),
        data: InterchangeData::Minimal(import_data),
    };

    slashing_db
        .import_interchange_info(&to_import, genesis_validators_root)
        .unwrap();

    // Apply post blocks
    apply_blocks(post_blocks);

    // Apply post attestations.
    apply_attestations(post_attestations);

    // Verify that exported interchange is as expected.
    let exported = slashing_db
        .export_minimal_interchange_info(genesis_validators_root)
        .unwrap();

    let expected_interchange = Interchange {
        metadata: metadata.clone(),
        data: InterchangeData::Minimal(expected),
    };
    assert!(exported.equiv(&expected_interchange));
}
