use hyper::{server::conn::http1, service::service_fn};
use rustls::{OwnedTrustAnchor, ServerName};
use std::{net::SocketAddr, sync::Arc};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsConnector;

use anyhow::Result;

pub async fn forward() -> Result<()> {
  let in_addr: SocketAddr = ([127, 0, 0, 1], 4000).into();

  let listener = TcpListener::bind(in_addr).await?;
  println!("Listening on http://{}", in_addr);

  loop {
    let (stream, _) = listener.accept().await?;

    // This is the `Service` that will handle the connection.
    // `service_fn` is a helper to convert a function that
    // returns a Response into a `Service`.
    let service = service_fn(move |mut req| {
      let uri_string = format!(
        // "https://i-kota.s3.us-west-001.backblazeb2.com{}",
        "https://f001.backblazeb2.com/file/i-kota{}",
        req
          .uri()
          .path_and_query()
          .map(|x| x.as_str())
          .unwrap_or("/")
      );

      dbg!(&uri_string);

      let uri = uri_string.parse().unwrap();
      *req.uri_mut() = uri;

      let host = req.uri().host().expect("uri has no host");
      let port = req.uri().port_u16().unwrap_or(443);
      let addr = format!("{}:{}", host, port);

      let mut root_cert_store = rustls::RootCertStore::empty();
      root_cert_store.add_server_trust_anchors(webpki_roots::TLS_SERVER_ROOTS.0.iter().map(|ta| {
        OwnedTrustAnchor::from_subject_spki_name_constraints(
          ta.subject,
          ta.spki,
          ta.name_constraints,
        )
      }));
      let config = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_root_certificates(root_cert_store)
        .with_no_client_auth();
      let connector = TlsConnector::from(Arc::new(config));

      async move {
        let stream = TcpStream::connect(&addr).await.unwrap();
        let stream = connector
          .connect(
            ServerName::try_from("f001.backblazeb2.com").unwrap(),
            stream,
          )
          .await
          .unwrap();

        let (mut sender, conn) = hyper::client::conn::http1::handshake(stream).await?;
        tokio::task::spawn(async move {
          if let Err(err) = conn.await {
            println!("Connection failed: {:?}", err);
          }
        });

        sender.send_request(req).await
      }
    });

    tokio::task::spawn(async move {
      if let Err(err) = http1::Builder::new()
        .serve_connection(stream, service)
        .await
      {
        println!("Failed to servce connection: {:?}", err);
      }
    });
  }
}
