use crate::music::{dl_thread, search};
use lazy_static::lazy_static;
use parking_lot::{Mutex, RwLock};
use rspotify_model::idtypes::Id;
use std::path::Path;
use std::{
  collections::{HashMap, VecDeque},
  convert::Infallible,
  pin::Pin,
  task::{Context, Poll},
  thread,
  time::Duration,
};
use tokio_stream::Stream;
use warp::{
  http::Response,
  hyper::body::{Body, Bytes},
  Filter,
};

const FILESIZE_LIMIT: usize = 5_000_000; // in bytes
const FILE_COUNT_LIMIT: usize = 10;
const LETS_ENCRYPT_ACCOUNT: &str = "17287977548916597336";

lazy_static! {
  static ref FILE_CACHE: RwLock<(HashMap<String, Vec<u8>>, VecDeque<String>)> = RwLock::default();
  pub static ref ACME_PROOF: Mutex<String> = Mutex::new(String::from("DEFAULT"));
}

pub async fn serve(port: u16) -> anyhow::Result<()> {
  info!("Booting web server...");

  // build routes
  let root = warp::path::end().and_then(root);
  let acme_challenge = warp::path(".well-known")
    .and(warp::path("acme-challenge"))
    .and(warp::path::param())
    .map(|_p: String| {
      thread::spawn(|| {
        thread::sleep(Duration::from_secs(1));
        std::process::exit(0);
      });
      ACME_PROOF.lock().clone()
    });
  let img = warp::path::param().and_then(img);
  let vid = warp::path("v").and(warp::path::param()).and_then(vid);
  let song = warp::path("song-priv")
    .and(warp::query::<HashMap<String, String>>())
    .and_then(song);
  let dl_song = warp::path("song-priv")
    .and(warp::path::param())
    .and_then(dl_song);

  // collect routes
  let routes = warp::get().and(root.or(dl_song).or(song).or(acme_challenge).or(vid).or(img));

  let server = warp::serve(routes);

  let cert_file = format!("{LETS_ENCRYPT_ACCOUNT}_crt_kota_is.crt");
  let key_file = format!("{LETS_ENCRYPT_ACCOUNT}_key_kota_is.key");
  if Path::new(&cert_file).exists() && Path::new(&key_file).exists() {
    let server = server.tls().cert_path(cert_file).key_path(key_file);
    server.run(([0, 0, 0, 0], port)).await;
  } else {
    server.run(([0, 0, 0, 0], port)).await;
  }

  Ok(())
}

async fn root() -> Result<impl warp::Reply, Infallible> {
  Ok("Hello there.")
}

async fn img(path: String) -> Result<Response<Body>, Infallible> {
  proxy("i-kota", &path).await
}
async fn vid(path: String) -> Result<Response<Body>, Infallible> {
  proxy("v-kota", &path).await
}

async fn dl_song(track: String) -> Result<Response<Body>, Infallible> {
  let rx = dl_thread(track);
  let sleep_for = Duration::from_secs(1);

  let ogg = loop {
    if let Ok(ogg) = rx.try_recv() {
      break ogg;
    }
    tokio::time::sleep(sleep_for).await;
  };

  let response = Response::builder()
    .header("Content-Type", "audio/ogg")
    .body(Body::from(ogg))
    .unwrap();
  Ok(response)
}

const SONG_HTML: &'static str = include_str!("web/song.html");

async fn song(params: HashMap<String, String>) -> Result<Response<Body>, Infallible> {
  // let song_html = std::fs::read_to_string("src/web/song.html").unwrap();
  let results = match params.get("q") {
    Some(q) => {
      let results = search(q).await.unwrap();
      results
        .iter()
        .map(|r| {
          format!(
            r#"<a href="/song-priv/{}" class="p-2 rounded-md cursor-pointer transition-all hover:bg-slate-500 hover:text-white">{} - {}</a>"#,
            r.id.as_ref().map(|t| t.id()).unwrap_or(""),
            r.artists
              .iter()
              .map(|a| a.name.clone())
              .collect::<Vec<String>>()
              .join(", "),
            r.name
          )
        })
        .collect()
    }
    _ => vec![],
  };

  let results = results.join("<br />");

  let song_html = SONG_HTML.replace("{results}", &results);

  Ok(Response::builder().body(Body::from(song_html)).unwrap())
}

async fn proxy(bucket: &str, path: &str) -> Result<Response<Body>, Infallible> {
  let guess = mime_guess::from_path(path).first_or(
    "application/octet-stream"
      .parse::<mime_guess::mime::Mime>()
      .unwrap(),
  );
  let content_type = format!("{}/{}", guess.type_(), guess.subtype());

  if let Some(val) = FILE_CACHE.read().0.get(path) {
    info!("RETURNED CACHED!");
    return Ok(
      Response::builder()
        .header("Content-Type", content_type)
        .body(Body::from(val.clone()))
        .unwrap(),
    );
  }

  println!("{path}");

  let response = reqwest::get(format!("https://f001.backblazeb2.com/file/{bucket}/{path}"))
    .await
    .unwrap();

  let content_length = response.content_length();
  let bytes_stream = response.bytes_stream();
  let cacher = StreamCache {
    stream: Mutex::new(Box::pin(bytes_stream)),
    file: Mutex::default(),
    path: path.to_owned(),
    content_length,
  };

  let wrapped_stream = Body::wrap_stream(cacher);
  let response_stream = Response::builder()
    .header("Content-Type", content_type)
    .body(wrapped_stream)
    .unwrap();

  Ok(response_stream)
}

struct StreamCache {
  stream: Mutex<Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send + Sync>>>,
  file: Mutex<Vec<u8>>,
  path: String,
  content_length: Option<u64>,
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

    if let Some(content_length) = self.content_length {
      if content_length == file.len() as u64 && file.len() < FILESIZE_LIMIT {
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
}
