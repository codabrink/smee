use anyhow::Result;
use awsregion::Region;
use s3::{creds::Credentials, Bucket};
use std::path::Path;
use tokio::fs::File;

pub async fn put_vid(s3_path: &str, file_path: &Path) -> Result<()> {
  put("v-kota", s3_path, file_path).await
}

pub async fn put(bucket: &str, s3_path: &str, file_path: &Path) -> Result<()> {
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
  let mut reader = File::open(file_path).await?;
  bucket.put_object_stream(&mut reader, s3_path).await?;

  Ok(())
}
