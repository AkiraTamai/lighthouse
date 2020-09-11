use eth2::BeaconNodeClient;
use slog::{debug, error, Logger};
use slot_clock::SlotClock;

/// A distance in slots.
const SYNC_TOLERANCE: u64 = 4;

/// Returns `true` if the beacon node is synced and ready for action.
///
/// Returns `false` if:
///
///  - The beacon node is unreachable.
///  - The beacon node indicates that it is syncing **AND** it is more than `SYNC_TOLERANCE` behind
///  the highest known slot.
///
///  The second condition means the even if the beacon node thinks that it's syncing, we'll still
///  try to use it if it's close enough to the head.
pub async fn is_synced<T: SlotClock>(
    beacon_node: &BeaconNodeClient,
    slot_clock: &T,
    log_opt: Option<&Logger>,
) -> bool {
    let resp = match beacon_node.get_node_syncing().await {
        Ok(resp) => resp,
        Err(e) => {
            if let Some(log) = log_opt {
                error!(
                    log,
                    "Unable connect to beacon node";
                    "error" => e.to_string()
                )
            }

            return false;
        }
    };

    let is_synced = !resp.data.is_syncing || (resp.data.sync_distance.as_u64() < SYNC_TOLERANCE);

    if !is_synced {
        if let Some(log) = log_opt {
            debug!(
                log,
                "Beacon node sync status";
                "status" => format!("{:?}", resp),
            );
            error!(
                log,
                "Beacon node is syncing";
                "msg" => "not receiving new duties",
                "sync_distance" => resp.data.sync_distance.as_u64(),
                "head_slot" => resp.data.head_slot.as_u64(),
            );
        }
    }

    is_synced
}
