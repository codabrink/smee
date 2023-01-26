use anyhow::Result;
use awsregion::Region;
use parking_lot::Mutex;
use s3::{creds::Credentials, Bucket};
use std::{
  path::Path,
  pin::Pin,
  sync::atomic::Ordering::Relaxed,
  sync::{atomic::AtomicU64, Arc},
};
use tokio::{fs::File, io::AsyncRead};

pub async fn put_vid(s3_path: &str, file_path: &Path, progress: Arc<AtomicU64>) -> Result<()> {
  put("v-kota", s3_path, file_path, progress).await
}

pub async fn put(
  bucket: &str,
  s3_path: &str,
  file_path: &Path,
  progress: Arc<AtomicU64>,
) -> Result<()> {
  let region = Region::Custom {
    region: "us-west-001".to_owned(),
    endpoint: "s3.us-west-001.backblazeb2.com".to_owned(),
  };
  let creds = Credentials::new(
    Some(env!("B2_ACCESS_KEY")),
    Some(env!("B2_SECRET_KEY")),
    None,
    None,
    None,
  )
  .unwrap();

  let bucket = Bucket::new(bucket, region, creds).unwrap();

  let mut reader = AsyncReader::new(file_path, progress.clone()).await?;
  // let mut reader = File::open(file_path).await?;
  bucket.put_object_stream(&mut reader, s3_path).await?;

  progress.store(100, Relaxed);

  Ok(())
}

struct AsyncReader {
  file: Mutex<Pin<Box<dyn AsyncRead + Send + Sync>>>,
  cursor: AtomicU64,
  file_size: u64,
  progress: Arc<AtomicU64>,
}

impl AsyncReader {
  async fn new(path: &Path, progress: Arc<AtomicU64>) -> Result<AsyncReader> {
    let file = File::open(path).await?;

    Ok(Self {
      file: Mutex::new(Box::pin(file)),
      cursor: AtomicU64::new(0),
      file_size: std::fs::File::open(path)?.metadata()?.len(),
      progress,
    })
  }

  fn progress(&self) -> f32 {
    self.cursor.load(Relaxed) as f32 / self.file_size as f32
  }
}

impl AsyncRead for AsyncReader {
  fn poll_read(
    self: std::pin::Pin<&mut Self>,
    cx: &mut std::task::Context<'_>,
    buf: &mut tokio::io::ReadBuf<'_>,
  ) -> std::task::Poll<std::io::Result<()>> {
    let result = self.file.lock().as_mut().poll_read(cx, buf);

    self.cursor.fetch_add(buf.filled().len() as u64, Relaxed);
    self.progress.store(self.progress() as u64, Relaxed);

    result
  }
}
