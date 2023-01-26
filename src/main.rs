extern crate pretty_env_logger;
#[macro_use]
extern crate log;

use anyhow::Result;
use clap::Parser;
use tokio::join;

mod backblaze;
mod cert;
mod http;
mod smee;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
pub async fn main() -> Result<()> {
  pretty_env_logger::init();

  let args = Args::parse();

  if args.cert {
    std::thread::spawn(|| {
      let _ = cert::request_cert();
    });
  }
  let _ = join!(smee::start(), http::serve(args.port), async {});

  Ok(())
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
  #[arg(short, long, default_value_t = 80)]
  port: u16,

  #[arg(short, long)]
  cert: bool,
}
