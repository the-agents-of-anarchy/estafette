use proto::{*, harness::*};
#[global_allocator] static A: Counting = Counting;
struct I;
struct C(proto::viairpc::Client);
impl Caller for C { fn call<'a>(&'a self, r: &'a Request) -> std::pin::Pin<Box<dyn Future<Output = anyhow::Result<Response>> + Send + 'a>> { Box::pin(self.0.call(r)) } }
impl Impl for I {
    const NAME: &'static str = "irpc 0.17.0 + irpc-iroh 0.17.0";
    fn serve(ep: iroh::Endpoint) -> iroh::protocol::Router { proto::viairpc::serve(ep) }
    fn frame(r: &Request) -> Vec<u8> { proto::viairpc::frame(r) }
    async fn client(ep: &iroh::Endpoint, a: iroh::EndpointAddr) -> anyhow::Result<Box<dyn Caller>> { Ok(Box::new(C(proto::viairpc::Client::new(ep, a).await?))) }
}
#[tokio::main] async fn main() { run::<I>().await }
