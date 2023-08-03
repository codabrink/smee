use anyhow::bail;
use anyhow::Result;
use byteorder::{LittleEndian, ReadBytesExt};
use crossbeam_channel::{bounded, Receiver};
use librespot::{
  audio::{AudioDecrypt, AudioFile},
  core::{
    authentication::Credentials, config::SessionConfig, session::Session, spotify_id::SpotifyId,
  },
  metadata::audio::AudioFileFormat,
  playback::config::PlayerConfig,
};
use librespot_core::cache::Cache;
use librespot_metadata::audio::{AudioFiles, AudioItem};
use librespot_playback::{
  config::{NormalisationMethod, NormalisationType},
  player::NormalisationData,
};
use rspotify::clients::BaseClient;
use rspotify_model::{FullTrack, SearchResult};
use std::io::{self, Read, Seek, SeekFrom};

pub const DB_VOLTAGE_RATIO: f64 = 20.0;
pub const PCM_AT_0DBFS: f64 = 1.0;
// Spotify inserts a custom Ogg packet at the start with custom metadata values, that you would
// otherwise expect in Vorbis comments. This packet isn't well-formed and players may balk at it.
const SPOTIFY_OGG_HEADER_END: u64 = 0xa7;

pub async fn search(q: impl AsRef<str>) -> Result<Vec<FullTrack>> {
  let rspotify_creds =
    rspotify::Credentials::new(env!("SPOTIFY_API_ID"), env!("SPOTIFY_API_SECRET"));
  let rspotify_client = rspotify::ClientCredsSpotify::new(rspotify_creds);
  rspotify_client.request_token().await.unwrap();

  let search_results = rspotify_client
    .search(
      q.as_ref(),
      rspotify_model::enums::types::SearchType::Track,
      None,
      None,
      None,
      None,
    )
    .await?;

  let SearchResult::Tracks(tracks) = search_results else {
    bail!("No tracks");
  };

  Ok(tracks.items)
}

fn full_track_to_spotify_id(full_track: &FullTrack) -> Result<SpotifyId> {
  let Some(id) = &full_track.id else {
    bail!("No Track id found.")
  };

  Ok(SpotifyId::from_uri(&id.to_string())?)
}

pub async fn dl(track_id: impl AsRef<str>) -> Result<Vec<u8>> {
  let spotify_id = SpotifyId::from_uri(&format!("spotify:track:{}", track_id.as_ref()))?;
  download(spotify_id).await
}

pub fn dl_thread(track_id: impl AsRef<str>) -> Receiver<Vec<u8>> {
  let (tx, rx) = bounded(1);
  let track_id = track_id.as_ref().to_owned();

  let _ = tokio::task::block_in_place(move || {
    tokio::runtime::Handle::current().block_on(async move { tx.send(dl(track_id).await.unwrap()) })
  });

  rx
}

pub async fn dl_search(q: impl AsRef<str>) -> Result<Vec<u8>> {
  let spotify_id = full_track_to_spotify_id(&search(&q).await?[0])?;
  download(spotify_id).await
}

async fn download(spotify_id: SpotifyId) -> Result<Vec<u8>> {
  let cache = Cache::new(Some("spotify_session_cache"), None, None, None)?;
  let credentials = match cache.credentials() {
    Some(credentials) => credentials,
    _ => Credentials::with_password(env!("SPOTIFY_USER"), env!("SPOTIFY_PASS")),
  };
  let session = Session::new(SessionConfig::default(), Some(cache));
  session.connect(credentials, true).await?;

  // TODO: handle unwrap
  let audio_item = AudioItem::get_file(&session, spotify_id).await.unwrap();

  let format = AudioFileFormat::OGG_VORBIS_320;

  // TODO: handle unwrap
  let file_id = audio_item.files.get(&format).unwrap();

  let encrypted_file = AudioFile::open(&session, *file_id, 40).await?;
  let stream_loader_controller = encrypted_file.get_stream_loader_controller()?;
  let key = session.audio_key().request(spotify_id, *file_id).await?;

  let mut decrypted_file = AudioDecrypt::new(Some(key), encrypted_file);

  let is_ogg_vorbis = AudioFiles::is_ogg_vorbis(format);
  let (offset, _normalisation_data) = if is_ogg_vorbis {
    // Spotify stores normalisation data in a custom Ogg packet instead of Vorbis comments.

    let normalisation_data = NormalisationData::parse_from_ogg(&mut decrypted_file).ok();
    (SPOTIFY_OGG_HEADER_END, normalisation_data)
  } else {
    (0, None)
  };

  let mut audio_file = Subfile::new(
    decrypted_file,
    offset,
    stream_loader_controller.len() as u64,
  )?;

  let mut buf = vec![];
  audio_file.read_to_end(&mut buf)?;
  Ok(buf)
}

struct Subfile<T: Read + Seek> {
  stream: T,
  offset: u64,
  length: u64,
}

impl<T: Read + Seek> Subfile<T> {
  pub fn new(mut stream: T, offset: u64, length: u64) -> Result<Subfile<T>, io::Error> {
    let target = SeekFrom::Start(offset);
    stream.seek(target)?;

    Ok(Subfile {
      stream,
      offset,
      length,
    })
  }
}

impl<T: Read + Seek> Read for Subfile<T> {
  fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
    self.stream.read(buf)
  }
}

impl<T: Read + Seek> Seek for Subfile<T> {
  fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
    let pos = match pos {
      SeekFrom::Start(offset) => SeekFrom::Start(offset + self.offset),
      SeekFrom::End(offset) => {
        if (self.length as i64 - offset) < self.offset as i64 {
          return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "newpos would be < self.offset",
          ));
        }
        pos
      }
      _ => pos,
    };

    let newpos = self.stream.seek(pos)?;
    Ok(newpos - self.offset)
  }
}

trait NormalisationDataImportTrait {
  fn parse_from_ogg<T: Read + Seek>(file: T) -> io::Result<NormalisationData>;
  fn get_factor(config: &PlayerConfig, data: NormalisationData) -> f64;
}
impl NormalisationDataImportTrait for NormalisationData {
  fn parse_from_ogg<T: Read + Seek>(mut file: T) -> io::Result<NormalisationData> {
    const SPOTIFY_NORMALIZATION_HEADER_START_OFFSET: u64 = 144;
    let newpos = file.seek(SeekFrom::Start(SPOTIFY_NORMALIZATION_HEADER_START_OFFSET))?;
    if newpos != SPOTIFY_NORMALIZATION_HEADER_START_OFFSET {
      error!(
        "NormalisationData::parse_from_file seeking to {} but position is now {}",
        SPOTIFY_NORMALIZATION_HEADER_START_OFFSET, newpos
      );
      error!("Falling back to default (non-track and non-album) normalisation data.");
      return Ok(NormalisationData::default());
    }

    let track_gain_db = file.read_f32::<LittleEndian>()? as f64;
    let track_peak = file.read_f32::<LittleEndian>()? as f64;
    let album_gain_db = file.read_f32::<LittleEndian>()? as f64;
    let album_peak = file.read_f32::<LittleEndian>()? as f64;

    let r = NormalisationData {
      track_gain_db,
      track_peak,
      album_gain_db,
      album_peak,
    };

    Ok(r)
  }

  fn get_factor(config: &PlayerConfig, data: NormalisationData) -> f64 {
    if !config.normalisation {
      return 1.0;
    }

    let (gain_db, gain_peak) = if config.normalisation_type == NormalisationType::Album {
      (data.album_gain_db, data.album_peak)
    } else {
      (data.track_gain_db, data.track_peak)
    };

    // As per the ReplayGain 1.0 & 2.0 (proposed) spec:
    // https://wiki.hydrogenaud.io/index.php?title=ReplayGain_1.0_specification#Clipping_prevention
    // https://wiki.hydrogenaud.io/index.php?title=ReplayGain_2.0_specification#Clipping_prevention
    let normalisation_factor = if config.normalisation_method == NormalisationMethod::Basic {
      // For Basic Normalisation, factor = min(ratio of (ReplayGain + PreGain), 1.0 / peak level).
      // https://wiki.hydrogenaud.io/index.php?title=ReplayGain_1.0_specification#Peak_amplitude
      // https://wiki.hydrogenaud.io/index.php?title=ReplayGain_2.0_specification#Peak_amplitude
      // We then limit that to 1.0 as not to exceed dBFS (0.0 dB).
      let factor = f64::min(
        db_to_ratio(gain_db + config.normalisation_pregain_db),
        PCM_AT_0DBFS / gain_peak,
      );

      if factor > PCM_AT_0DBFS {
        info!(
                  "Lowering gain by {:.2} dB for the duration of this track to avoid potentially exceeding dBFS.",
                  ratio_to_db(factor)
              );

        PCM_AT_0DBFS
      } else {
        factor
      }
    } else {
      // For Dynamic Normalisation it's up to the player to decide,
      // factor = ratio of (ReplayGain + PreGain).
      // We then let the dynamic limiter handle gain reduction.
      let factor = db_to_ratio(gain_db + config.normalisation_pregain_db);
      let threshold_ratio = db_to_ratio(config.normalisation_threshold_dbfs);

      if factor > PCM_AT_0DBFS {
        let factor_db = gain_db + config.normalisation_pregain_db;
        let limiting_db = factor_db + config.normalisation_threshold_dbfs.abs();

        warn!(
                  "This track may exceed dBFS by {:.2} dB and be subject to {:.2} dB of dynamic limiting at it's peak.",
                  factor_db, limiting_db
              );
      } else if factor > threshold_ratio {
        let limiting_db =
          gain_db + config.normalisation_pregain_db + config.normalisation_threshold_dbfs.abs();

        info!(
          "This track may be subject to {:.2} dB of dynamic limiting at it's peak.",
          limiting_db
        );
      }

      factor
    };

    debug!("Normalisation Data: {:?}", data);
    debug!(
      "Calculated Normalisation Factor for {:?}: {:.2}%",
      config.normalisation_type,
      normalisation_factor * 100.0
    );

    normalisation_factor
  }
}

pub fn db_to_ratio(db: f64) -> f64 {
  f64::powf(10.0, db / DB_VOLTAGE_RATIO)
}

pub fn ratio_to_db(ratio: f64) -> f64 {
  ratio.log10() * DB_VOLTAGE_RATIO
}
