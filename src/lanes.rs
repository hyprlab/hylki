/**
 * @file lanes.rs
 * @author Damien O'Brien (Narcarsiss) - https://github.com/narcarsiss/hylki
 * @brief Re-exports and lane aliases matching the performance package architecture.
 */

pub use crate::connection_pool::{
    ConnectionPool, IdleState, LaneKind, LaneSlot,
};
