use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use iroh::RelayUrl;

/// How many connections from this machine to each host's relay spec 3.15 test 23 times before any member starts.
pub const CONNECTS_PER_RELAY: usize = 10;
/// How long one timed connection to a relay may take before the reading fails the test: chosen, not measured, far above a round trip between two continents.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long this machine took to open each of its timed TCP connections to one relay.
#[derive(Debug)]
pub struct RelayConnectTimes {
    pub relay: String,
    pub readings: [Duration; CONNECTS_PER_RELAY],
}

/// Times TCP connections from this machine to the relay's port one after another, its name resolved once before the first, each from the start of the connect to the open connection, failing the test naming the relay when its name does not resolve or a connection is not open within the connect timeout.
#[track_caller]
pub fn connect_times_to(relay: &str) -> RelayConnectTimes {
    let address = address_of(relay);
    RelayConnectTimes {
        relay: relay.to_owned(),
        readings: std::array::from_fn(|_| connect_time(relay, address)),
    }
}

/// The first socket address the relay URL's host resolves to at its port.
#[track_caller]
fn address_of(relay: &str) -> SocketAddr {
    let url: RelayUrl = relay
        .parse()
        .unwrap_or_else(|refusal| panic!("relay {relay} is no relay URL: {refusal}"));
    url.socket_addrs(|| None)
        .unwrap_or_else(|failure| panic!("relay {relay} does not resolve: {failure}"))
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("relay {relay} resolves to no address"))
}

/// How long one TCP connection to the relay's address took to open, closed again at once.
fn connect_time(relay: &str, address: SocketAddr) -> Duration {
    let started = Instant::now();
    let connection =
        TcpStream::connect_timeout(&address, CONNECT_TIMEOUT).unwrap_or_else(|failure| {
            panic!(
                "no connection to relay {relay} at {address} within {CONNECT_TIMEOUT:?}: {failure}"
            )
        });
    let took = started.elapsed();
    drop(connection);
    took
}

/// Whether the placement spec 3.15 test 23 stands on holds: the slowest connection to the relay of the host that will be killed is faster than the fastest to the relay of the host that lives, the refusal naming that precondition and every reading.
pub fn killed_relay_answers_faster(
    killed: &RelayConnectTimes,
    living: &RelayConnectTimes,
) -> Result<(), String> {
    let slowest_to_killed = slowest_of(killed);
    let fastest_to_living = fastest_of(living);
    if slowest_to_killed < fastest_to_living {
        Ok(())
    } else {
        Err(format!(
            "spec 3.15 test 23 stands on every member homing on the relay of the host it kills, which this lab holds only when the slowest of {CONNECTS_PER_RELAY} connections from this machine to the relay of the host that will be killed, {}, is faster than the fastest of {CONNECTS_PER_RELAY} to the relay of the host that lives, {}: it is not, slowest {slowest_to_killed:?} against fastest {fastest_to_living:?}; to the killed host's relay: {:?}; to the living host's relay: {:?}",
            killed.relay, living.relay, killed.readings, living.readings
        ))
    }
}

/// The longest of the relay's connect times.
fn slowest_of(times: &RelayConnectTimes) -> Duration {
    times
        .readings
        .into_iter()
        .fold(Duration::ZERO, Duration::max)
}

/// The shortest of the relay's connect times.
fn fastest_of(times: &RelayConnectTimes) -> Duration {
    times
        .readings
        .into_iter()
        .fold(Duration::MAX, Duration::min)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{CONNECTS_PER_RELAY, RelayConnectTimes, killed_relay_answers_faster};

    const KILLED_RELAY: &str = "https://killed.example/";
    const LIVING_RELAY: &str = "https://living.example/";

    /// Readings of the relay, one per millisecond figure.
    fn times(relay: &str, millis: [u64; CONNECTS_PER_RELAY]) -> RelayConnectTimes {
        RelayConnectTimes {
            relay: relay.to_owned(),
            readings: millis.map(Duration::from_millis),
        }
    }

    #[test]
    fn spec_3_15_test_23_holds_when_the_slowest_connection_to_the_killed_relay_beats_the_fastest_to_the_living_one()
     {
        let killed = times(KILLED_RELAY, [31, 39, 30, 32, 33, 34, 35, 36, 37, 38]);
        let living = times(LIVING_RELAY, [49, 41, 42, 40, 43, 44, 45, 46, 47, 48]);

        assert_eq!(killed_relay_answers_faster(&killed, &living), Ok(()));
    }

    #[test]
    fn spec_3_15_test_23_is_refused_when_the_slowest_connection_to_the_killed_relay_equals_the_fastest_to_the_living_one()
     {
        let killed = times(KILLED_RELAY, [31, 40, 30, 32, 33, 34, 35, 36, 37, 38]);
        let living = times(LIVING_RELAY, [49, 41, 42, 40, 43, 44, 45, 46, 47, 48]);

        assert!(killed_relay_answers_faster(&killed, &living).is_err());
    }

    #[test]
    fn spec_3_15_test_23_is_refused_when_only_the_slowest_connection_to_the_killed_relay_is_slower_than_the_fastest_to_the_living_one()
     {
        let killed = times(KILLED_RELAY, [10, 10, 10, 10, 50, 10, 10, 10, 10, 10]);
        let living = times(
            LIVING_RELAY,
            [100, 100, 100, 100, 100, 100, 45, 100, 100, 100],
        );

        assert!(killed_relay_answers_faster(&killed, &living).is_err());
    }

    #[test]
    fn spec_3_15_test_23_a_refused_placement_names_the_precondition_and_every_reading() {
        let killed = times(KILLED_RELAY, [61, 69, 60, 62, 63, 64, 65, 66, 67, 68]);
        let living = times(LIVING_RELAY, [59, 51, 52, 50, 53, 54, 55, 56, 57, 58]);

        let refusal = killed_relay_answers_faster(&killed, &living).unwrap_err();

        assert!(
            refusal.contains(&format!(
                "the slowest of {CONNECTS_PER_RELAY} connections from this machine to the relay of the host that will be killed, {KILLED_RELAY}, is faster than the fastest of {CONNECTS_PER_RELAY} to the relay of the host that lives, {LIVING_RELAY}"
            )),
            "{refusal}"
        );
        assert!(
            refusal.contains(&format!("slowest {:?}", Duration::from_millis(69))),
            "{refusal}"
        );
        assert!(
            refusal.contains(&format!("fastest {:?}", Duration::from_millis(50))),
            "{refusal}"
        );
        assert!(
            refusal.contains(&format!("{:?}", killed.readings)),
            "{refusal}"
        );
        assert!(
            refusal.contains(&format!("{:?}", living.readings)),
            "{refusal}"
        );
    }
}
