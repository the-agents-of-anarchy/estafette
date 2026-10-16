use std::time::Duration;

use iroh::{
    Endpoint, RelayMode, SecretKey,
    address_lookup::{AddrFilter, DnsAddressLookup, PkarrPublisher, PkarrResolver},
    endpoint::presets::{self, Minimal},
};

use crate::{Res, two_phase};

/// `unfiltered`: same services as the N0 preset, but the publisher also publishes IP addresses (no relay to publish).
async fn bind(sk: Option<SecretKey>, alpns: Vec<Vec<u8>>, relay: bool, unfiltered: bool) -> Res<Endpoint> {
    let b = if unfiltered {
        Endpoint::builder(Minimal)
            .relay_mode(RelayMode::Disabled)
            .address_lookup(PkarrPublisher::n0_dns().addr_filter(AddrFilter::unfiltered()))
            .address_lookup(PkarrResolver::n0_dns())
            .address_lookup(DnsAddressLookup::n0_dns())
    } else if relay {
        Endpoint::builder(presets::N0)
    } else {
        Endpoint::builder(presets::N0DisableRelay)
    };
    let b = b.alpns(alpns);
    Ok(match sk { Some(sk) => b.secret_key(sk), None => b }.bind().await?)
}

pub async fn run(run: u32, relay: bool, unfiltered: bool) -> Res<()> {
    let name = match (relay, unfiltered) {
        (true, _) => "n0-preset",
        (false, false) => "n0-preset-norelay",
        (false, true) => "n0-norelay-ip-publish",
    };
    let cap = Duration::from_secs(if relay || unfiltered { 60 } else { 30 });
    two_phase(name, run, cap, Duration::from_secs(5), async |sk, alpns| bind(sk, alpns, relay, unfiltered).await, async |_| {}).await
}
