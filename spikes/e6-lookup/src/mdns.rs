use std::time::Duration;

use iroh::{Endpoint, RelayMode, SecretKey, endpoint::presets::Minimal};
use iroh_mdns_address_lookup::MdnsAddressLookup;

use crate::{Res, two_phase};

async fn bind(sk: Option<SecretKey>, alpns: Vec<Vec<u8>>) -> Res<Endpoint> {
    let b = Endpoint::builder(Minimal)
        .relay_mode(RelayMode::Disabled)
        .alpns(alpns)
        .address_lookup(MdnsAddressLookup::builder().service_name("e6spike"));
    Ok(match sk { Some(sk) => b.secret_key(sk), None => b }.bind().await?)
}

pub async fn run(run: u32) -> Res<()> {
    two_phase("mdns-norelay", run, Duration::from_secs(60), Duration::from_secs(3), bind, async |_| {}).await
}
