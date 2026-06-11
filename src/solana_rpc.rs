use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use solana_client::rpc_client::RpcClient;
use solana_pubkey::Pubkey;
use std::str::FromStr;

/// Nominal slot duration used to convert between slots and wall-clock time.
/// Actual slot times vary slightly; callers should build their own margin in.
pub const MS_PER_SLOT: u64 = 400;

/// Snapshot of where the validator identity sits in the leader schedule.
#[derive(Debug, Clone, Copy)]
pub struct LeaderWindowInfo {
    pub current_slot: u64,
    /// Next absolute slot at which the identity is scheduled to be leader
    /// (current slot included, i.e. `Some(current_slot)` means "leader right
    /// now"). `None` when the identity has no known upcoming leader slots —
    /// either it has none left this epoch and none in the next epoch's
    /// schedule, or it simply carries no stake.
    pub next_leader_slot: Option<u64>,
}

impl LeaderWindowInfo {
    /// Slots until the next leader slot; `None` when no upcoming leader slot
    /// is known (an effectively unbounded idle window).
    pub fn idle_slots(&self) -> Option<u64> {
        self.next_leader_slot
            .map(|slot| slot.saturating_sub(self.current_slot))
    }
}

/// Earliest absolute leader slot at or after `current_slot`, given the
/// epoch-relative slot indexes returned by `getLeaderSchedule`.
pub fn next_leader_slot_after(
    slot_indexes: &[usize],
    epoch_start_slot: u64,
    current_slot: u64,
) -> Option<u64> {
    slot_indexes
        .iter()
        .map(|&idx| epoch_start_slot + idx as u64)
        .filter(|&slot| slot >= current_slot)
        .min()
}

/// Fetch the next scheduled leader slot for `identity_pubkey`, looking at the
/// remainder of the current epoch and, if nothing is left there, the next
/// epoch's schedule (which is already determined one epoch in advance).
pub async fn fetch_leader_window(rpc_url: &str, identity_pubkey: &str) -> Result<LeaderWindowInfo> {
    use solana_client::rpc_config::RpcLeaderScheduleConfig;
    use std::time::Duration;

    if rpc_url.is_empty() {
        return Err(anyhow!("RPC URL is empty"));
    }

    let rpc_client = RpcClient::new_with_timeout(rpc_url.to_string(), Duration::from_secs(5));

    let epoch_info = rpc_client
        .get_epoch_info()
        .map_err(|e| anyhow!("Failed to get epoch info: {}", e))?;
    let current_slot = epoch_info.absolute_slot;
    let epoch_start_slot = current_slot.saturating_sub(epoch_info.slot_index);

    let config = RpcLeaderScheduleConfig {
        identity: Some(identity_pubkey.to_string()),
        commitment: None,
    };

    let identity_slots = |schedule: Option<std::collections::HashMap<String, Vec<usize>>>| {
        schedule
            .and_then(|mut map| map.remove(identity_pubkey))
            .unwrap_or_default()
    };

    let current_epoch_schedule = rpc_client
        .get_leader_schedule_with_config(Some(current_slot), config.clone())
        .map_err(|e| anyhow!("Failed to get leader schedule: {}", e))?;
    let mut next_leader_slot = next_leader_slot_after(
        &identity_slots(current_epoch_schedule),
        epoch_start_slot,
        current_slot,
    );

    if next_leader_slot.is_none() {
        // Nothing left this epoch — we might be close to the boundary with
        // leader slots early in the next epoch. Best-effort: if the next
        // epoch's schedule isn't available, treat it as no upcoming slots.
        let next_epoch_start_slot = epoch_start_slot + epoch_info.slots_in_epoch;
        if let Ok(schedule) =
            rpc_client.get_leader_schedule_with_config(Some(next_epoch_start_slot), config)
        {
            next_leader_slot = next_leader_slot_after(
                &identity_slots(schedule),
                next_epoch_start_slot,
                current_slot,
            );
        }
    }

    Ok(LeaderWindowInfo {
        current_slot,
        next_leader_slot,
    })
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoteAccountInfo {
    pub vote_pubkey: String,
    pub validator_identity: String,
    pub activated_stake: u64,
    pub commission: u8,
    pub root_slot: u64,
    pub last_vote: u64,
    pub credits: u64,
    pub recent_timestamp: Option<String>,
    pub current_slot: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentVote {
    pub slot: u64,
    pub confirmation_count: u32,
    /// Slots between the voted-on slot and the slot the vote landed in, as
    /// recorded on-chain — the number TVC credits are paid on. 0 = unknown
    /// (vote predates latency recording, or degraded fallback path).
    pub latency: u64,
}

#[derive(Debug, Clone)]
pub struct TvcPerformanceMetrics {
    pub tvc_rank: u32,
    pub total_validators: u32,
    pub avg_vote_latency: f64,
    pub missed_votes: u64,
    pub missed_votes_window: u64,
}

#[derive(Debug, Clone)]
pub struct ValidatorVoteData {
    #[allow(dead_code)]
    pub vote_account_info: VoteAccountInfo,
    pub recent_votes: Vec<RecentVote>,
    pub is_voting: bool,
    pub tvc_metrics: Option<TvcPerformanceMetrics>,
}

fn compute_tvc_rank(
    vote_account: &solana_client::rpc_response::RpcVoteAccountStatus,
    vote_pubkey_str: &str,
) -> Option<(u32, u32)> {
    let mut epoch_credits: Vec<(String, u64)> = vote_account
        .current
        .iter()
        .chain(vote_account.delinquent.iter())
        .filter_map(|acct| {
            acct.epoch_credits
                .last()
                .map(|&(_, credits, prev)| (acct.vote_pubkey.clone(), credits.saturating_sub(prev)))
        })
        .collect();

    epoch_credits.sort_by_key(|&(_, credits)| std::cmp::Reverse(credits));
    let total = epoch_credits.len() as u32;
    let rank = epoch_credits
        .iter()
        .position(|(pk, _)| pk == vote_pubkey_str)
        .map(|pos| (pos as u32) + 1)?;
    Some((rank, total))
}

fn compute_avg_vote_latency(recent_votes: &[RecentVote]) -> Option<f64> {
    // Latency 0 means the vote predates on-chain latency recording (or comes
    // from the degraded fallback path) — exclude those from the average.
    let latencies: Vec<u64> = recent_votes
        .iter()
        .map(|v| v.latency)
        .filter(|&l| l > 0)
        .collect();
    if latencies.is_empty() {
        return None;
    }
    Some(latencies.iter().sum::<u64>() as f64 / latencies.len() as f64)
}

fn compute_missed_votes(
    votes: &std::collections::VecDeque<solana_vote_interface::state::LandedVote>,
    current_slot: u64,
    max_window: u64,
) -> (u64, u64) {
    if votes.is_empty() {
        return (0, 0);
    }
    let voted_slots: std::collections::HashSet<u64> =
        votes.iter().map(|l| l.lockout.slot()).collect();
    let oldest_slot = votes
        .front()
        .map(|l| l.lockout.slot())
        .unwrap_or(current_slot);
    let raw_window = current_slot.saturating_sub(oldest_slot) + 1;
    let effective_window = raw_window.min(max_window);
    let window_start = current_slot.saturating_sub(effective_window - 1);
    let voted_in_window = voted_slots
        .iter()
        .filter(|&&s| s >= window_start && s <= current_slot)
        .count() as u64;
    let missed = effective_window.saturating_sub(voted_in_window);
    (missed, effective_window)
}

pub async fn fetch_vote_account_data(
    rpc_url: &str,
    vote_pubkey_str: &str,
) -> Result<ValidatorVoteData> {
    use std::time::Duration;

    // Validate RPC URL
    if rpc_url.is_empty() {
        return Err(anyhow!("RPC URL is empty"));
    }

    // Log the RPC URL being used (for debugging)
    // eprintln!("Using RPC URL: {}", rpc_url);
    // eprintln!("Looking for vote account: {}", vote_pubkey_str);

    let rpc_client = RpcClient::new_with_timeout(rpc_url.to_string(), Duration::from_secs(3));
    let vote_pubkey =
        Pubkey::from_str(vote_pubkey_str).map_err(|e| anyhow!("Invalid vote pubkey: {}", e))?;

    // Get vote account info
    let vote_account = rpc_client
        .get_vote_accounts()
        .map_err(|e| anyhow!("Failed to get vote accounts: {}", e))?;

    // Find our specific vote account in current or delinquent
    let vote_info = vote_account
        .current
        .iter()
        .chain(vote_account.delinquent.iter())
        .find(|account| account.vote_pubkey == vote_pubkey_str)
        .ok_or_else(|| {
            let total_accounts = vote_account.current.len() + vote_account.delinquent.len();
            anyhow!("Vote account {} not found among {} vote accounts. Make sure the RPC endpoint matches the network (mainnet/testnet/devnet) where this vote account exists.", vote_pubkey_str, total_accounts)
        })?;

    // Get detailed vote account data. We still ask for this because the
    // deserialized vote state gives us a richer view (recent votes list with
    // per-vote latency, credits, last_timestamp). VoteStateV4::deserialize
    // understands every on-chain format (V1_14_11, V3, V4) and converts it to
    // V4, so this covers Agave 2.x+ / Firedancer 0.5+ accounts too. Should a
    // future format appear that it can't decode, we fall back to the lighter
    // view derivable from `vote_info`, which is enough to keep delinquency
    // detection working.
    let account_data = rpc_client
        .get_account(&vote_pubkey)
        .map_err(|e| anyhow!("Failed to get vote account data: {}", e))?;

    let vote_state =
        solana_vote_interface::state::VoteStateV4::deserialize(&account_data.data, &vote_pubkey)
            .ok();

    let current_slot = rpc_client
        .get_slot()
        .map_err(|e| anyhow!("Failed to get current slot: {}", e))?;

    // Build the recent_votes list. Prefer the rich VoteState path; fall back
    // to a single synthesized entry from vote_info.last_vote when the on-chain
    // format is newer than what the SDK can decode.
    let mut recent_votes = Vec::new();
    if let Some(ref vs) = vote_state {
        // Get the most recent votes (up to 31 as shown in the example).
        // The votes are stored in order, with most recent at the end.
        //
        // `latency` is the value recorded on-chain: the number of slots
        // between the voted-on slot and the slot the vote transaction landed
        // in — the same number TVC credits are paid on. Votes cast before
        // validators recorded latencies carry 0 ("unknown").
        for (i, landed) in vs.votes.iter().rev().take(31).enumerate() {
            recent_votes.push(RecentVote {
                slot: landed.slot(),
                confirmation_count: (i + 1) as u32,
                latency: landed.latency as u64,
            });
        }
    } else {
        // Fallback path: we couldn't decode the on-chain vote state (a format
        // newer than VoteStateV4). vote_info.last_vote is still trustworthy
        // because it comes from get_vote_accounts() and doesn't require account
        // data decoding on our side. One entry is enough to drive delinquency
        // detection in status_ui_v2; the richer UI columns (latency over the
        // last 31 votes, missed-vote window, etc.) simply degrade.

        // Do not write directly to stderr here: this function runs while the
        // terminal UI is active, and direct stderr writes corrupt the TUI. The
        // degraded state is represented by missing latency/missed-vote metrics;
        // if we need operator-facing diagnostics later, route them through the
        // UI log_sender instead of eprintln!.

        recent_votes.push(RecentVote {
            slot: vote_info.last_vote,
            confirmation_count: 1,
            latency: 0, // unknown — same convention as pre-latency-tracking votes
        });
    }

    // Compute TVC performance metrics from already-fetched data
    let tvc_metrics = {
        let rank_data = compute_tvc_rank(&vote_account, vote_pubkey_str);
        let avg_latency = compute_avg_vote_latency(&recent_votes);
        // Missed-vote counting needs the full lockout history; we only have
        // that on the rich path. On the fallback path we report
        // (missed=0, window=0) which the UI can interpret as "no data".
        let (missed, window) = if let Some(ref vs) = vote_state {
            compute_missed_votes(&vs.votes, current_slot, 500)
        } else {
            (0, 0)
        };

        match (rank_data, avg_latency) {
            (Some((rank, total)), Some(latency)) => Some(TvcPerformanceMetrics {
                tvc_rank: rank,
                total_validators: total,
                avg_vote_latency: latency,
                missed_votes: missed,
                missed_votes_window: window,
            }),
            _ => None,
        }
    };

    // Determine if validator is voting: its most recent vote landed within
    // the last 150 slots (~1 minute).
    let is_voting = recent_votes
        .first()
        .map(|v| current_slot.saturating_sub(v.slot) < 150)
        .unwrap_or(false);

    // Pull credits and timestamp from the rich path when we have it, otherwise
    // fall back: epoch_credits is part of vote_info and gives the cumulative
    // credit count without needing to decode the account data ourselves.
    let credits = if let Some(ref vs) = vote_state {
        vs.credits()
    } else {
        vote_info
            .epoch_credits
            .last()
            .map(|(_, credits, _)| *credits)
            .unwrap_or(0)
    };

    let recent_timestamp = vote_state.as_ref().map(|vs| {
        chrono::DateTime::<chrono::Utc>::from_timestamp(vs.last_timestamp.timestamp, 0)
            .unwrap_or_default()
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    });

    Ok(ValidatorVoteData {
        vote_account_info: VoteAccountInfo {
            vote_pubkey: vote_pubkey_str.to_string(),
            validator_identity: vote_info.node_pubkey.clone(),
            activated_stake: vote_info.activated_stake,
            commission: vote_info.commission,
            root_slot: vote_info.root_slot,
            last_vote: vote_info.last_vote,
            credits,
            recent_timestamp,
            current_slot: Some(current_slot),
        },
        recent_votes,
        is_voting,
        tvc_metrics,
    })
}

#[cfg(test)]
mod vote_latency_tests {
    use super::*;

    fn vote(slot: u64, latency: u64) -> RecentVote {
        RecentVote {
            slot,
            confirmation_count: 1,
            latency,
        }
    }

    #[test]
    fn test_avg_latency_uses_recorded_values() {
        let votes = vec![vote(103, 1), vote(102, 2), vote(101, 3)];
        assert_eq!(compute_avg_vote_latency(&votes), Some(2.0));
    }

    #[test]
    fn test_avg_latency_ignores_unknown_zero_latency() {
        // 0 means "recorded before latency tracking" and must not drag the
        // average down.
        let votes = vec![vote(103, 2), vote(102, 4), vote(101, 0)];
        assert_eq!(compute_avg_vote_latency(&votes), Some(3.0));
    }

    #[test]
    fn test_avg_latency_none_when_all_unknown() {
        let votes = vec![vote(103, 0), vote(102, 0)];
        assert_eq!(compute_avg_vote_latency(&votes), None);
    }

    #[test]
    fn test_avg_latency_none_when_empty() {
        assert_eq!(compute_avg_vote_latency(&[]), None);
    }

    #[test]
    fn test_avg_latency_single_vote() {
        // The degraded fallback path synthesizes one vote with latency 0;
        // a single *known* latency is still a valid average.
        assert_eq!(compute_avg_vote_latency(&[vote(103, 2)]), Some(2.0));
        assert_eq!(compute_avg_vote_latency(&[vote(103, 0)]), None);
    }
}

#[cfg(test)]
mod leader_window_tests {
    use super::*;

    #[test]
    fn test_next_leader_slot_skips_past_slots() {
        // Epoch starts at 1000; leader at indexes 5..8 and 100..103.
        let indexes = vec![5, 6, 7, 8, 100, 101, 102, 103];
        // Current slot 1050 — the first group is in the past.
        assert_eq!(next_leader_slot_after(&indexes, 1000, 1050), Some(1100));
    }

    #[test]
    fn test_next_leader_slot_includes_current_slot() {
        // Being leader *right now* must count as an upcoming leader slot.
        let indexes = vec![5, 6, 7, 8];
        assert_eq!(next_leader_slot_after(&indexes, 1000, 1006), Some(1006));
    }

    #[test]
    fn test_next_leader_slot_none_when_all_past() {
        let indexes = vec![5, 6, 7, 8];
        assert_eq!(next_leader_slot_after(&indexes, 1000, 2000), None);
    }

    #[test]
    fn test_next_leader_slot_empty_schedule() {
        assert_eq!(next_leader_slot_after(&[], 1000, 1050), None);
    }

    #[test]
    fn test_idle_slots() {
        let window = LeaderWindowInfo {
            current_slot: 100,
            next_leader_slot: Some(250),
        };
        assert_eq!(window.idle_slots(), Some(150));

        let leading_now = LeaderWindowInfo {
            current_slot: 100,
            next_leader_slot: Some(100),
        };
        assert_eq!(leading_now.idle_slots(), Some(0));

        let no_slots = LeaderWindowInfo {
            current_slot: 100,
            next_leader_slot: None,
        };
        assert_eq!(no_slots.idle_slots(), None);
    }
}
