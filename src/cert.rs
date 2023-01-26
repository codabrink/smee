use acme_lib::{create_p384_key, persist::FilePersist, Directory, DirectoryUrl};
use anyhow::Result;

pub fn request_cert() -> Result<()> {
  let url = DirectoryUrl::LetsEncrypt;
  let persist = FilePersist::new(".");
  let dir = Directory::from_url(persist, url)?;
  let acc = dir.account("pub@kota.is")?;

  let mut ord_new = acc.new_order("kota.is", &[])?;

  // If the ownership of the domain(s) have already been
  // authorized in a previous order, you might be able to
  // skip validation. The ACME API provider decides.
  let ord_csr = loop {
    // are we done?
    if let Some(ord_csr) = ord_new.confirm_validations() {
      break ord_csr;
    }

    // Get the possible authorizations (for a single domain
    // this will only be one element).
    let auths = ord_new.authorizations()?;

    // For HTTP, the challenge is a text file that needs to
    // be placed in your web server's root:
    //
    // /var/www/.well-known/acme-challenge/<token>
    //
    // The important thing is that it's accessible over the
    // web for the domain(s) you are trying to get a
    // certificate for:
    //
    // http://mydomain.io/.well-known/acme-challenge/<token>
    let chall = auths[0].http_challenge();

    // The token is the filename.
    // let token = chall.http_token();
    // let path = format!(".well-known/acme-challenge/{}", token);

    // The proof is the contents of the file
    *crate::http::ACME_PROOF.lock() = chall.http_proof();

    // Here you must do "something" to place
    // the file/contents in the correct place.
    // update_my_web_server(&path, &proof);

    // After the file is accessible from the web, the calls
    // this to tell the ACME API to start checking the
    // existence of the proof.
    //
    // The order at ACME will change status to either
    // confirm ownership of the domain, or fail due to the
    // not finding the proof. To see the change, we poll
    // the API with 5000 milliseconds wait between.
    chall.validate(5000)?;

    // Update the state against the ACME API.
    ord_new.refresh()?;
  };

  let pkey_pri = create_p384_key();
  let ord_cert = ord_csr.finalize_pkey(pkey_pri, 5000)?;
  let cert = ord_cert.download_and_save_cert()?;

  info!("CERT DONE");

  Ok(())
}
