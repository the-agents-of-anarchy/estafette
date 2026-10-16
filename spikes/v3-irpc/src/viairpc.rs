use crate::*;
use iroh::{Endpoint, EndpointAddr, endpoint::Connection, protocol::{AcceptError, ProtocolHandler, Router}};
use irpc::{WithChannels, channel::oneshot, rpc_requests};
use serde::{Deserialize, Serialize};

#[rpc_requests(message = Msg)]
#[derive(Serialize, Deserialize, Debug)]
enum Proto {
    #[rpc(tx = oneshot::Sender<MessageAck>)]
    Deliver(Message),
    #[rpc(tx = oneshot::Sender<ProbeAck>)]
    Probe(Probe),
    #[rpc(tx = oneshot::Sender<IntroduceAck>)]
    Introduce(Introduce),
}

#[derive(Debug)]
struct H;
impl ProtocolHandler for H {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let remote = conn.remote_id();
        loop {
            let msg = match irpc_iroh::read_request::<Proto>(&conn).await {
                Ok(Some(m)) => m,
                Ok(None) => return Ok(()),
                Err(e) => { println!("  [server] request rejected: {e}"); return Err(AcceptError::from_err(e)); }
            };
            match msg {
                Msg::Deliver(WithChannels { inner, tx, .. }) => { tx.send(ack_message(remote, &inner)).await.ok(); }
                Msg::Probe(WithChannels { inner, tx, .. }) => { tx.send(ack_probe(remote, &inner)).await.ok(); }
                Msg::Introduce(WithChannels { inner, tx, .. }) => { tx.send(ack_introduce(remote, &inner)).await.ok(); }
            }
        }
    }
}

pub fn serve(ep: Endpoint) -> Router { Router::builder(ep).accept(ALPN, H).spawn() }

pub struct Client(irpc::Client<Proto>);
impl Client {
    pub async fn new(ep: &Endpoint, addr: EndpointAddr) -> anyhow::Result<Self> { Ok(Self(irpc_iroh::client::<Proto>(ep.clone(), addr, ALPN))) }
    pub async fn call(&self, req: &Request) -> anyhow::Result<Response> {
        Ok(match req.clone() {
            Request::Message(m) => Response::Message(self.0.rpc(m).await?),
            Request::Probe(p) => Response::Probe(self.0.rpc(p).await?),
            Request::Introduce(i) => Response::Introduce(self.0.rpc(i).await?),
        })
    }
}
pub fn frame(req: &Request) -> Vec<u8> {
    let body = postcard::to_stdvec(req).unwrap();
    let mut out = postcard::to_stdvec(&(body.len() as u64)).unwrap();
    out.extend(body);
    out
}
