use std::io::{BufRead, BufReader, Write};
use tokio::runtime::Builder;
use tracing::error;

mod handler;
mod rpc;

fn main() -> anyhow::Result<()> {
    // Logs go to stderr — stdout is reserved for the JSON wire protocol.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let rt = Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async_main())
}

async fn async_main() -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break; // EOF
        }
        let request: rpc::Request = match serde_json::from_str(line.trim()) {
            Ok(req) => req,
            Err(e) => {
                error!("failed to parse request: {e}");
                continue;
            }
        };

        let response = handler::dispatch(&request).await;
        serde_json::to_writer(&mut writer, &response)?;
        writeln!(&mut writer)?;
        writer.flush()?;
    }

    Ok(())
}
