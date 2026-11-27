use std::time::Duration;

use iroh::EndpointId;
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use tokio::runtime::Handle;
use tokio::sync::watch;

/// The connections of one dialer that a node leaves unserved, and the way to serve them.
pub struct HeldConnections {
    arrivals: watch::Receiver<bool>,
    release: watch::Sender<bool>,
}

/// The half of `HeldConnections` the node's protocol handler takes, to hold the dialer's connections before serving them.
#[derive(Debug)]
pub struct ConnectionHold {
    dialer: EndpointId,
    arrived: watch::Sender<bool>,
    released: watch::Receiver<bool>,
}

impl HeldConnections {
    /// Starts holding the connections of the dialer, returning the way to wait for one and release them and the half the node's handler takes.
    pub fn of(dialer: EndpointId) -> (Self, ConnectionHold) {
        let (arrived, arrivals) = watch::channel(false);
        let (release, released) = watch::channel(false);
        (
            Self { arrivals, release },
            ConnectionHold {
                dialer,
                arrived,
                released,
            },
        )
    }

    /// Waits on the node's runtime until the node holds a connection of the dialer, failing when none arrives within the bound.
    #[track_caller]
    pub fn wait_for_one(&self, node_runtime: &Handle, within: Duration) {
        let arrived = node_runtime.block_on(async {
            let mut arrivals = self.arrivals.clone();
            tokio::time::timeout(within, arrivals.wait_for(|arrived| *arrived))
                .await
                .is_ok_and(|seen| seen.is_ok())
        });
        assert!(
            arrived,
            "no connection of the dialer reached the holding member within {within:?}"
        );
    }

    /// Has the node serve every connection of the dialer it holds, and each later one at once.
    pub fn release(&self) {
        self.release.send_replace(true);
    }
}

impl ConnectionHold {
    /// The protocol handler that serves a connection of the dialer with `served` only once released, and every other connection at once.
    pub const fn before_serving<Served: ProtocolHandler>(
        self,
        served: Served,
    ) -> HoldingConnectionsOf<Served> {
        HoldingConnectionsOf { hold: self, served }
    }
}

/// The protocol handler of a node that serves a connection of one dialer only once released, and every other connection at once.
#[derive(Debug)]
pub struct HoldingConnectionsOf<Served: ProtocolHandler> {
    hold: ConnectionHold,
    served: Served,
}

impl<Served: ProtocolHandler> ProtocolHandler for HoldingConnectionsOf<Served> {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        if connection.remote_id() == self.hold.dialer {
            self.hold.arrived.send_replace(true);
            drop(
                self.hold
                    .released
                    .clone()
                    .wait_for(|released| *released)
                    .await,
            );
        }
        self.served.accept(connection).await
    }
}
