mod driver;
mod node;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("node") => node::run(&args).await,
        Some("driver") => driver::run(&args).await,
        _ => anyhow::bail!("usage: e4-presence node --name N [flags] | driver all|s1..s6 [--init-delay MS]"),
    }
}
