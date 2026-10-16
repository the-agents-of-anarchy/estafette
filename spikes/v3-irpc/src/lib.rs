//! Shared: types, limit, counting allocator, scenario harness. Excluded from line counts.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use serde::{Deserialize, Serialize};

pub const LIMIT: usize = 1_114_112;
pub const ALPN: &[u8] = b"estafette-proto/1";

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Message { pub v: u8, pub id: [u8; 16], pub body: String, pub mac: [u8; 32] }
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Probe { pub v: u8, pub nonce: [u8; 16], pub mac: [u8; 32] }
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Introduce { pub v: u8, pub public: Vec<u8>, pub mac: [u8; 32] }
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MessageAck { pub v: u8, pub id: [u8; 16], pub accepted: bool, pub reason: Option<String> }
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ProbeAck { pub v: u8, pub nonce: [u8; 16], pub mac: [u8; 32] }
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IntroduceAck { pub v: u8, pub public: Vec<u8>, pub mac: [u8; 32] }

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Request { Message(Message), Probe(Probe), Introduce(Introduce) }
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Response { Message(MessageAck), Probe(ProbeAck), Introduce(IntroduceAck) }

// Handler logic shared by both paths. `remote` is the authenticated remote node id.
pub fn ack_message(remote: iroh::EndpointId, m: &Message) -> MessageAck {
    MessageAck { v: 1, id: m.id, accepted: m.v == 1, reason: Some(format!("remote={remote} body={}", m.body.len())) }
}
pub fn ack_probe(_remote: iroh::EndpointId, p: &Probe) -> ProbeAck { ProbeAck { v: 1, nonce: p.nonce, mac: p.mac } }
pub fn ack_introduce(_remote: iroh::EndpointId, i: &Introduce) -> IntroduceAck { IntroduceAck { v: 1, public: i.public.clone(), mac: i.mac } }
pub fn answer(remote: iroh::EndpointId, r: Request) -> Response {
    match r {
        Request::Message(m) => Response::Message(ack_message(remote, &m)),
        Request::Probe(p) => Response::Probe(ack_probe(remote, &p)),
        Request::Introduce(i) => Response::Introduce(ack_introduce(remote, &i)),
    }
}

// Counting allocator
pub struct Counting;
pub static LIVE: AtomicUsize = AtomicUsize::new(0);
pub static PEAK: AtomicUsize = AtomicUsize::new(0);
pub static MAX_SINGLE: AtomicUsize = AtomicUsize::new(0);
fn add(n: usize) {
    let l = LIVE.fetch_add(n, Relaxed) + n;
    PEAK.fetch_max(l, Relaxed);
    MAX_SINGLE.fetch_max(n, Relaxed);
}
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 { add(l.size()); unsafe { System.alloc(l) } }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 { add(l.size()); unsafe { System.alloc_zeroed(l) } }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) { LIVE.fetch_sub(l.size(), Relaxed); unsafe { System.dealloc(p, l) } }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        LIVE.fetch_sub(l.size(), Relaxed); add(n); unsafe { System.realloc(p, l, n) }
    }
}
pub fn reset() -> usize { let b = LIVE.load(Relaxed); PEAK.store(b, Relaxed); MAX_SINGLE.store(0, Relaxed); b }
pub fn report(tag: &str, base: usize) {
    println!("  [alloc {tag}] max_single={} peak_live_delta={}", MAX_SINGLE.load(Relaxed), PEAK.load(Relaxed).saturating_sub(base));
}

pub mod harness;
pub mod hand;
#[cfg(feature = "irpc_path")]
pub mod viairpc;
