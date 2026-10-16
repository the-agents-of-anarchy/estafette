use crate::*;
use iroh::{Endpoint, EndpointAddr, endpoint::Connection, protocol::{AcceptError, ProtocolHandler, Router}};

#[derive(Debug)]
struct H;
impl ProtocolHandler for H {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let remote = conn.remote_id();
        while let Ok((mut tx, mut rx)) = conn.accept_bi().await {
            tokio::spawn(async move {
                let req = match rx.read_to_end(LIMIT).await {
                    Ok(b) => postcard::from_bytes::<Request>(&b).map_err(|e| format!("decode: {e}")),
                    Err(e) => Err(format!("read: {e}")),
                };
                match req {
                    Ok(r) => {
                        tx.write_all(&postcard::to_stdvec(&answer(remote, r)).unwrap()).await.ok();
                        tx.finish().ok();
                    }
                    Err(e) => {
                        println!("  [server] stream rejected: {e}");
                        rx.stop(1u32.into()).ok();
                        tx.reset(1u32.into()).ok();
                    }
                }
            });
        }
        Ok(())
    }
}

pub fn serve(ep: Endpoint) -> Router { Router::builder(ep).accept(ALPN, H).spawn() }

pub struct Client(pub Connection);
impl Client {
    pub async fn new(ep: &Endpoint, addr: EndpointAddr) -> anyhow::Result<Self> { Ok(Self(ep.connect(addr, ALPN).await?)) }
    pub async fn call(&self, req: &Request) -> anyhow::Result<Response> {
        let (mut tx, mut rx) = self.0.open_bi().await?;
        tx.write_all(&postcard::to_stdvec(req)?).await?;
        tx.finish()?;
        Ok(postcard::from_bytes(&rx.read_to_end(LIMIT).await?)?)
    }
}
pub fn frame(req: &Request) -> Vec<u8> { postcard::to_stdvec(req).unwrap() }
