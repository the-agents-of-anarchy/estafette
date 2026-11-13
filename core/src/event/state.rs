use serde_json::Value;

use super::{AreaEvent, EventFields};
use crate::net::endpoint::{CounterSnapshot, SnapshotMoment};

const MOMENT_FIELD: &str = "moment";

super::event_names! {
    /// The names of the state events.
    StateEventName {
        Counters => "state.counters",
    }
}

/// What a process records of its socket and gossip counters at start, at each rebind, at stop and at intervals between.
pub enum StateEvent {
    /// The counters iroh keeps for the socket and for gossip, read at one moment.
    Counters(CounterSnapshot),
}

impl AreaEvent for StateEvent {
    type Name = StateEventName;

    fn name(&self) -> StateEventName {
        match self {
            Self::Counters(_) => StateEventName::Counters,
        }
    }

    fn run_record_line(&self) -> Option<String> {
        match self {
            Self::Counters(_) => None,
        }
    }

    fn fields(&self) -> EventFields {
        match self {
            Self::Counters(snapshot) => {
                let moment = match snapshot.moment() {
                    SnapshotMoment::Start => "start",
                    SnapshotMoment::Rebind => "rebind",
                    SnapshotMoment::Interval => "interval",
                    SnapshotMoment::Stop => "stop",
                };
                std::iter::once((MOMENT_FIELD, Value::from(moment)))
                    .chain(
                        snapshot
                            .counters()
                            .iter()
                            .map(|(name, count)| (*name, Value::from(*count))),
                    )
                    .collect()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COUNT_SPREAD: u64 = 0x9E37_79B9_7F4A_7C15;

    /// Sets each named counter field of a metrics group to the next count, giving the name and count of each under its group's name.
    macro_rules! counters_set_apart {
        ($group:literal, $metrics:expr, $next_count:ident, [$($field:ident),* $(,)?]) => {
            vec![$({
                let count = $next_count();
                $metrics.$field.set(count);
                (concat!($group, ".", stringify!($field)), count)
            }),*]
        };
    }

    #[test]
    fn observability_decision_9_each_counter_of_a_snapshot_carries_the_count_iroh_keeps_for_it() {
        let socket = iroh::metrics::SocketMetrics::default();
        let gossip = iroh_gossip::metrics::Metrics::default();
        let mut index = 0;
        let mut next_count = || {
            index += 1;
            COUNT_SPREAD.wrapping_mul(index)
        };
        let mut set = counters_set_apart!(
            "socket",
            socket,
            next_count,
            [
                update_direct_addrs,
                send_ipv4,
                send_ipv6,
                send_relay,
                recv_data_relay,
                recv_data_custom,
                recv_data_ipv4,
                recv_data_ipv6,
                recv_datagrams,
                recv_gro_datagrams,
                relay_home_change,
                relay_conns_success,
                relay_conns_failed,
                relay_conns_closed,
                relay_conns_ratelimited,
                holepunch_attempts,
                paths_direct,
                paths_relay,
                paths_custom,
                num_conns_direct,
                num_conns_opened,
                num_conns_closed,
                transport_ip_paths_added,
                transport_ip_paths_removed,
                transport_relay_paths_added,
                transport_relay_paths_removed,
                transport_custom_paths_added,
                transport_custom_paths_removed,
                actor_tick_main,
                actor_tick_msg,
                actor_tick_re_stun,
                actor_tick_portmap_changed,
                actor_tick_direct_addr_heartbeat,
                actor_link_change,
                actor_tick_other,
            ]
        );
        set.extend(counters_set_apart!(
            "gossip",
            gossip,
            next_count,
            [
                msgs_ctrl_sent,
                msgs_ctrl_recv,
                msgs_data_sent,
                msgs_data_recv,
                msgs_data_sent_size,
                msgs_data_recv_size,
                msgs_ctrl_sent_size,
                msgs_ctrl_recv_size,
                neighbor_up,
                neighbor_down,
                actor_tick_main,
                actor_tick_rx,
                actor_tick_endpoint,
                actor_tick_dialer,
                actor_tick_dialer_success,
                actor_tick_dialer_failure,
                actor_tick_in_event_rx,
                actor_tick_timers,
            ]
        ));

        let fields = StateEvent::Counters(CounterSnapshot::read(
            SnapshotMoment::Interval,
            &socket,
            &gossip,
        ))
        .fields();

        let expected: EventFields = std::iter::once(("moment", Value::from("interval")))
            .chain(set.iter().map(|(name, count)| (*name, Value::from(*count))))
            .collect();
        assert_eq!(fields, expected);
    }

    #[test]
    fn observability_decision_9_each_moment_of_a_snapshot_is_written_as_a_word_of_its_own() {
        let cases = [
            (
                SnapshotMoment::Start,
                vec![("moment", Value::from("start"))],
            ),
            (
                SnapshotMoment::Rebind,
                vec![("moment", Value::from("rebind"))],
            ),
            (
                SnapshotMoment::Interval,
                vec![("moment", Value::from("interval"))],
            ),
            (SnapshotMoment::Stop, vec![("moment", Value::from("stop"))]),
        ];

        crate::net::direct::tests::assert_each_arm(
            &cases,
            |moment| {
                StateEvent::Counters(CounterSnapshot::read(
                    *moment,
                    &iroh::metrics::SocketMetrics::default(),
                    &iroh_gossip::metrics::Metrics::default(),
                ))
                .fields()
                .into_iter()
                .filter(|(name, _)| *name == "moment")
                .collect::<EventFields>()
            },
            cases.len(),
        );
    }
}
