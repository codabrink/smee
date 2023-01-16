use anyhow::{bail, Result};
use glob::glob;
use rand::{distributions::Alphanumeric, thread_rng, Rng};
use teloxide::{prelude::*, types::InputFile, utils::command::BotCommands};
use youtube_dl::{YoutubeDl, YoutubeDlOutput};

#[tokio::main]
pub async fn main() -> Result<()> {
  let bot = Bot::new(env!("TELEGRAM_BOT_KEY"));

  let _ = std::fs::remove_dir_all("tmp_files");
  let _ = std::fs::create_dir("tmp_files");

  Command::repl(bot, answer).await;

  Ok(())
}

async fn answer(bot: Bot, msg: Message, cmd: Command) -> ResponseResult<()> {
  match cmd {
    Command::Help => {
      bot
        .send_message(msg.chat.id, Command::descriptions().to_string())
        .await?;
    }
    Command::Video(args) => {
      let chat_id = msg.chat.id;
      if let Err(err) = DownloadContext::new(DownloadMode::Video, &bot, msg, args)
        .run()
        .await
      {
        bot
          .send_message(chat_id, format!("Error: {:?}", err))
          .await?;
      };
    }
    Command::Audio(args) => {
      let chat_id = msg.chat.id;
      if let Err(err) = DownloadContext::new(DownloadMode::Audio, &bot, msg, args)
        .run()
        .await
      {
        bot
          .send_message(chat_id, format!("Error: {:?}", err))
          .await?;
      };
    }
  };

  Ok(())
}

const DEFAULT_SIZE_LIMIT: u32 = 100;

struct DownloadContext<'a> {
  id: String,
  bot: &'a Bot,
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

impl<'a> DownloadContext<'a> {
  fn new(mode: DownloadMode, bot: &'a Bot, msg: Message, args: String) -> Self {
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
    DEFAULT_SIZE_LIMIT
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
        }

        bail!("Error: {:?}", err);
      }
    };

    let caption = match result {
      YoutubeDlOutput::SingleVideo(video) => video.title,
      _ => String::from("No Title Found"),
    };

    if let Some(status_msg) = &self.status_msg {
      self
        .bot
        .edit_message_text(status_msg.chat.id, status_msg.id, "Uploading...")
        .await?;
    }

    let Some(file_path) = glob(&format!("tmp_files/{}*", self.id))?.nth(0) else {
      if let Some(status_msg) = &self.status_msg {
        self.bot.edit_message_text(status_msg.chat.id, status_msg.id, "Could not find downloaded file.").await?;
      }
      bail!("Could not find downloaded file.");
    };

    match self.mode {
      DownloadMode::Video => {
        self
          .bot
          .send_video(self.msg.chat.id, InputFile::file(&file_path?))
          .supports_streaming(true)
          .caption(caption)
          .await?;
      }
      DownloadMode::Audio => {
        self
          .bot
          .send_audio(self.msg.chat.id, InputFile::file(&file_path?))
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

  async fn download_video_cmd(&mut self) -> Result<YoutubeDl> {
    let status_msg = self
      .bot
      .send_message(
        self.msg.chat.id,
        format!("Downloading video... ({}MB limit)", self.size_limit),
      )
      .await?;
    self.status_msg = Some(status_msg);

    let outfile = format!("tmp_files/{}", self.id);
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
        format!("Downloading audio... ({}MB limit)", self.size_limit),
      )
      .await?;
    self.status_msg = Some(status_msg);

    let outfile = format!("tmp_files/{}.%(ext)s", self.id);
    let dl_cmd = dl_cmd(&self.url, self.size_limit, &outfile)
      .extract_audio(true)
      .to_owned();

    Ok(dl_cmd)
  }
}

fn dl_cmd(url: &str, size_limit: u32, outfile: &str) -> YoutubeDl {
  let dl_cmd = YoutubeDl::new(url)
    .output_template(outfile)
    .socket_timeout("15")
    .extra_arg(format!("-S filesize:{}M", size_limit))
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
  #[command(description = "extract audio from a video here.")]
  Audio(String),
  #[command(description = "mirror a video here.")]
  Video(String),
}
