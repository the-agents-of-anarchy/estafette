mod driver;
mod node;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("node") => node::run(&args).await,
        Some("driver") => driver::run(&args).await,
        _ => anyhow::bail!(
            "usage: e7-admission node --name N [--secret-hex H] [--topic-hex H] [--key-hex H] [--deny ID,..] | driver [scenario]"
        ),
    }
}
