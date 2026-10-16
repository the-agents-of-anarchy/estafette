use std::time::Duration;

use iroh::{
    Endpoint, RelayMode, SecretKey,
    address_lookup::{AddrFilter, DnsAddressLookup, PkarrPublisher, PkarrResolver},
    dns::DnsResolver,
    endpoint::presets::Minimal,
};

use crate::{Res, relay_mode, spawn_relay, two_phase};

const PKARR: &str = "http://127.0.0.1:8080/pkarr";
const ORIGIN: &str = "irohdns.example.";

/// Locally run `iroh-dns-server` (dev config, mainline off) as publisher, pkarr resolver and DNS resolver.
pub async fn run(run: u32, relay: bool) -> Res<()> {
    let srv = if relay { Some(spawn_relay(0).await?) } else { None };
    let mode = srv.as_ref().map_or(RelayMode::Disabled, |s| relay_mode(&format!("http://{}", s.http_addr().unwrap()).parse().unwrap()));
    let mk = async |sk: Option<SecretKey>, alpns: Vec<Vec<u8>>| -> Res<Endpoint> {
        let publisher = PkarrPublisher::builder(PKARR.parse()?);
        let publisher = if relay { publisher } else { publisher.addr_filter(AddrFilter::unfiltered()) };
        let dns = DnsResolver::with_nameserver("127.0.0.1:5300".parse()?);
        // E6_SELF=pkarr|dns selects one resolver; default both.
        let which = std::env::var("E6_SELF").unwrap_or_default();
        let mut b = Endpoint::builder(Minimal).relay_mode(mode.clone()).alpns(alpns).address_lookup(publisher);
        if which != "dns" {
            b = b.address_lookup(PkarrResolver::builder(PKARR.parse()?));
        }
        if which != "pkarr" {
            b = b.address_lookup(DnsAddressLookup::builder(ORIGIN.to_string()).dns_resolver(dns));
        }
        Ok(match sk { Some(sk) => b.secret_key(sk), None => b }.bind().await?)
    };
    let name = format!("selfhost-{}-{}", std::env::var("E6_SELF").unwrap_or_else(|_| "both".into()), if relay { "relay" } else { "norelay" });
    two_phase(&name, run, Duration::from_secs(60), Duration::from_secs(5), mk, async |_| {}).await?;
    if let Some(s) = srv {
        s.shutdown().await?;
    }
    Ok(())
}
