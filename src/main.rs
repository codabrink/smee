extern crate pretty_env_logger;
#[macro_use]
extern crate log;

use anyhow::Result;
use clap::Parser;
use tokio::join;

mod backblaze;
mod cert;
mod http;
mod music;
mod smee;

#[tokio::main]
pub async fn main() -> Result<()> {
  pretty_env_logger::init();

  let mut args = Args::parse();

  if args.cert {
    // override the port
    args.port = 80;
    std::thread::spawn(|| {
      let _ = cert::request_cert();
    });
  }

  let _ = join!(smee::start(), http::serve(args.port));

  Ok(())
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
  #[arg(short, long, default_value_t = 443)]
  port: u16,

  #[arg(short, long)]
  cert: bool,
}
