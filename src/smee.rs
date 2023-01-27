use anyhow::{bail, Result};
use glob::glob;
use rand::{distributions::Alphanumeric, thread_rng, Rng};
use std::{
  path::PathBuf,
  sync::{
    atomic::{AtomicU64, Ordering::Relaxed},
    Arc,
  },
  time::Duration,
};
use teloxide::{prelude::*, types::InputFile, utils::command::BotCommands};
use tokio::join;
use youtube_dl::{YoutubeDl, YoutubeDlOutput};

const TMP_DIR: &str = "video";
const DEFAULT_SIZE_LIMIT_MB: u32 = 50;
const DEFAULT_SIZE_LIMIT: u64 = DEFAULT_SIZE_LIMIT_MB as u64 * 1_000_000;

pub async fn start() {
  let _ = std::fs::remove_dir_all(TMP_DIR);
  let _ = std::fs::create_dir(TMP_DIR);

  let bot = Bot::new(env!("TELEGRAM_BOT_KEY"));
  Command::repl(bot, answer).await;
}

async fn answer(bot: Bot, msg: Message, cmd: Command) -> ResponseResult<()> {
  let chat_id = msg.chat.id;

  let (mode, args) = match cmd {
    Command::Help | Command::Start => {
      bot
        .send_message(msg.chat.id, Command::descriptions().to_string())
        .await?;
      return Ok(());
    }
    Command::Video(args) => (DownloadMode::Video, args),
    Command::Audio(args) => (DownloadMode::Audio, args),
  };

  let mut context = DownloadContext::new(mode, bot.clone(), msg, args);
  if let Err(err) = context.run().await {
    bot
      .send_message(
        chat_id,
        format!(
          "Oh dear. I'm afraid you'll have to see this, cap'n.\n\n{:?}",
          err
        ),
      )
      .await?;
  }

  // === CLEANUP ===
  // delete status message
  if let Some(msg) = &context.status_msg {
    bot.delete_message(msg.chat.id, msg.id).await?;
  }
  // delete file
  if let Ok(Some(path)) = context.file() {
    let _ = std::fs::remove_file(path);
  }

  Ok(())
}

struct DownloadContext {
  id: String,
  bot: Bot,
  msg: Message,
  size_limit: u32,
  url: String,
  mode: DownloadMode,
  status_msg: Option<Message>,
}

enum DownloadMode {
  Video,
  Audio,
}

impl DownloadContext {
  fn new(mode: DownloadMode, bot: Bot, msg: Message, args: String) -> Self {
    let args: Vec<&str> = args.split(" ").collect();
    Self {
      id: rand_string(5),
      mode,
      bot,
      msg,
      size_limit: Self::size_limit(&args),
      url: args[0].to_owned(),
      status_msg: None,
    }
  }

  fn size_limit(params: &[&str]) -> u32 {
    if let Some(size_limit) = params.get(1) {
      return size_limit.parse().unwrap();
    }
    DEFAULT_SIZE_LIMIT_MB
  }

  async fn run(&mut self) -> Result<()> {
    let dl_cmd = match self.mode {
      DownloadMode::Video => self.download_video_cmd().await?,
      DownloadMode::Audio => self.download_audio_cmd().await?,
    };

    let result = match dl_cmd.run() {
      Ok(result) => result,
      Err(err) => {
        if let Some(status_msg) = &self.status_msg {
          self
            .bot
            .edit_message_text(
              status_msg.chat.id,
              status_msg.id,
              format!("yt-dlp error: {:?}", err),
            )
            .await?;

          return Ok(());
        }

        bail!("Oh dear! Oh no.. Cap'n, look:\n\n{:?}", err);
      }
    };

    let caption = match result {
      YoutubeDlOutput::SingleVideo(video) => video.title,
      _ => String::from("No Title Found"),
    };

    let Some(file_path) = self.file()? else {
      bail!("Oh dear.. we lost the downloaded file, cap'n.");
    };

    if let Some(status_msg) = &self.status_msg {
      let file = std::fs::File::open(&file_path)?;
      let filesize = file.metadata().unwrap().len();

      warn!("FILESIZE: {}", filesize);

      if filesize >= DEFAULT_SIZE_LIMIT {
        let host_msg = "Oh Cap'n, this file is too large for Telegram. Let me host it for you!";
        let large_msg = self.bot.send_message(status_msg.chat.id, host_msg).await?;

        let progress = Arc::new(AtomicU64::new(0));

        let update = {
          let progress = progress.clone();

          let bot = self.bot.clone();
          async move {
            loop {
              let progress = progress.as_ref().load(Relaxed);
              if progress == 100 {
                break;
              }

              let _ = bot
                .edit_message_text(
                  large_msg.chat.id,
                  large_msg.id,
                  format!("{}\n\n{}%", host_msg, progress),
                )
                .await;
              tokio::time::sleep(Duration::from_millis(200)).await;
            }
          }
        };

        let s3_path = format!("{}.mp4", self.id);
        let put_vid = crate::backblaze::put_vid(&s3_path, &file_path, progress);

        let _ = join!(update, put_vid);

        self
          .bot
          .edit_message_text(
            large_msg.chat.id,
            large_msg.id,
            format!("https://kota.is/v/{}.mp4", self.id),
          )
          .await?;

        return Ok(());
      }

      self
        .bot
        .edit_message_text(
          status_msg.chat.id,
          status_msg.id,
          "I got the file, sir! Sending it now...",
        )
        .await?;
    }

    match self.mode {
      DownloadMode::Video => {
        self
          .bot
          .send_video(self.msg.chat.id, InputFile::file(&file_path))
          .supports_streaming(true)
          .caption(caption)
          .await?;
      }
      DownloadMode::Audio => {
        self
          .bot
          .send_audio(self.msg.chat.id, InputFile::file(&file_path))
          .caption(caption)
          .await?;
      }
    }

    if let Some(status_msg) = &self.status_msg {
      self
        .bot
        .delete_message(status_msg.chat.id, status_msg.id)
        .await?;
    }

    Ok(())
  }

  fn file(&self) -> Result<Option<PathBuf>> {
    Ok(
      glob(&format!("{}/{}*", TMP_DIR, self.id))?
        .nth(0)
        .map(|pb| pb.unwrap()),
    )
  }

  async fn download_video_cmd(&mut self) -> Result<YoutubeDl> {
    let status_msg = self
      .bot
      .send_message(
        self.msg.chat.id,
        format!(
          "Aye-aye cap'n! Downloading video with a {}MB filesize limit.",
          self.size_limit
        ),
      )
      .await?;
    self.status_msg = Some(status_msg);

    let outfile = format!("{}/{}", TMP_DIR, self.id);
    let dl_cmd = dl_cmd(&self.url, self.size_limit, &outfile)
      .format("mp4")
      .to_owned();

    Ok(dl_cmd)
  }

  async fn download_audio_cmd(&mut self) -> Result<YoutubeDl> {
    let status_msg = self
      .bot
      .send_message(
        self.msg.chat.id,
        format!(
          "Oh sure, cap'n! I'll get that for you. ({}MB limit)",
          self.size_limit
        ),
      )
      .await?;
    self.status_msg = Some(status_msg);

    let outfile = format!("{}/{}.%(ext)s", TMP_DIR, self.id);
    let dl_cmd = dl_cmd(&self.url, self.size_limit, &outfile)
      .extract_audio(true)
      .to_owned();

    Ok(dl_cmd)
  }
}

fn dl_cmd(url: &str, size_limit: u32, outfile: &str) -> YoutubeDl {
  let dl_cmd = YoutubeDl::new(url)
    .socket_timeout("15")
    .extra_arg(format!("-S filesize:{}M", size_limit))
    .output_template(outfile)
    .download(true)
    .to_owned();

  dl_cmd
}

fn rand_string(len: usize) -> String {
  thread_rng()
    .sample_iter(&Alphanumeric)
    .take(len)
    .map(char::from)
    .collect()
}

#[derive(BotCommands, Clone)]
#[command(
  rename_rule = "lowercase",
  description = "These commands are supported:"
)]
enum Command {
  #[command(description = "display this text.")]
  Help,
  #[command(description = "display this text.")]
  Start,
  #[command(description = "extract audio from a video here.")]
  Audio(String),
  #[command(description = "mirror a video here.")]
  Video(String),
}
