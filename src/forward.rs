use lazy_static::lazy_static;
use parking_lot::{Mutex, RwLock};
use std::{
  collections::{HashMap, VecDeque},
  convert::Infallible,
  pin::Pin,
  task::{Context, Poll},
};
use tokio_stream::Stream;
use warp::{
  http::Response,
  hyper::body::{Body, Bytes},
  Filter,
};

const FILESIZE_LIMIT: usize = 5_000_000; // in bytes
const FILE_COUNT_LIMIT: usize = 10;

lazy_static! {
  static ref FILE_CACHE: RwLock<(HashMap<String, Vec<u8>>, VecDeque<String>)> = RwLock::default();
}

pub async fn forward(port: &str) -> anyhow::Result<()> {
  info!("Booting web server...");
  let root = warp::path::end().and_then(root);
  let proxy = warp::path::param().and_then(proxy);

  let routes = root.or(proxy);

  warp::serve(routes)
    .run(([0, 0, 0, 0], port.parse().unwrap()))
    .await;

  Ok(())
}

async fn root() -> Result<impl warp::Reply, Infallible> {
  Ok("Hello there.")
}

async fn proxy(path: String) -> Result<impl warp::Reply, Infallible> {
  if let Some(val) = FILE_CACHE.read().0.get(&path) {
    info!("RETURNED CACHED!");
    return Ok(
      Response::builder()
        .header("Content-Type", "image/png")
        .body(Body::from(val.clone()))
        .unwrap(),
    );
  }

  let response = reqwest::get(format!("https://f001.backblazeb2.com/file/i-kota/{}", path))
    .await
    .unwrap();
  let bytes_stream = response.bytes_stream();
  let cacher = StreamCache {
    stream: Mutex::new(Box::pin(bytes_stream)),
    file: Mutex::default(),
    path,
  };

  let wrapped_stream = Body::wrap_stream(cacher);
  let response_stream = Response::builder()
    .header("Content-Type", "image/png")
    .body(wrapped_stream)
    .unwrap();

  Ok(response_stream)
}

struct StreamCache {
  stream: Mutex<Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send + Sync>>>,
  file: Mutex<Vec<u8>>,
  path: String,
}

impl Stream for StreamCache {
  type Item = Result<Bytes, reqwest::Error>;
  fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
    match self.stream.lock().as_mut().poll_next(cx) {
      Poll::Ready(Some(Ok(val))) => {
        let mut file = self.file.lock();
        if file.len() < FILESIZE_LIMIT {
          file.extend_from_slice(&*val);
        }

        Poll::Ready(Some(Ok(val)))
      }
      v => v,
    }
  }

  fn size_hint(&self) -> (usize, Option<usize>) {
    self.stream.lock().size_hint()
  }
}

impl Drop for StreamCache {
  fn drop(&mut self) {
    let file = std::mem::replace(&mut *self.file.lock(), vec![]);
    if file.len() < FILESIZE_LIMIT {
      info!("Cached: {}: {}", &self.path, file.len());
      let mut cache = FILE_CACHE.write();
      cache.1.push_front(self.path.clone());
      cache.0.entry(self.path.clone()).or_insert(file);

      if cache.1.len() > FILE_COUNT_LIMIT {
        let key = cache.1.pop_back().unwrap();
        cache.0.remove(&key);
      }
    }
  }
}
