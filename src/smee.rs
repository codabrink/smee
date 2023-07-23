use anyhow::{bail, Context, Result};
use crossbeam_channel::{unbounded, Receiver, Sender};
use glob::glob;
use lazy_static::lazy_static;
use rand::{distributions::Alphanumeric, thread_rng, Rng};
use std::path::PathBuf;
use teloxide::{prelude::*, types::InputFile, utils::command::BotCommands};
use tokio::runtime::Runtime;
use youtube_dl::{YoutubeDl, YoutubeDlOutput};

const TMP_DIR: &str = "video";
const DEFAULT_SIZE_LIMIT_MB: u32 = 50;
const DEFAULT_SIZE_LIMIT: u64 = DEFAULT_SIZE_LIMIT_MB as u64 * 1_000_000;

lazy_static! {
  static ref DELAYED_CMD: (Sender<u64>, Receiver<u64>) = unbounded();
}

pub async fn start() {
  let _ = std::fs::remove_dir_all(TMP_DIR);
  let _ = std::fs::create_dir(TMP_DIR);

  let bot = Bot::new(env!("TELEGRAM_BOT_KEY"));
  // let bot = Bot::new("6326192895:AAHqizQIGCJYoM5gOfqubOYaxwFkOoEhkOE");

  Command::repl(bot, answer).await;
}

async fn answer(bot: Bot, msg: Message, cmd: Command) -> ResponseResult<()> {
  match cmd {
    Command::Help | Command::Start => {
      bot
        .send_message(msg.chat.id, Command::descriptions().to_string())
        .await?;
      return Ok(());
    }
    Command::Video(args) => {
      if let Ok(mut interaction) = Interaction::new(bot.clone(), msg, args).await {
        if let Err(err) = interaction.download_video().await {
          let _ = interaction.edit_response(format!("Oh my.. {err:?}")).await;
        } else {
          let _ = interaction.delete_response().await;
        }
        if let Ok(path) = interaction.file() {
          let _ = std::fs::remove_file(path);
        }
      }
    }
    Command::Audio(args) => {
      if let Ok(mut interaction) = Interaction::new(bot.clone(), msg, args).await {
        let _ = interaction.download_audio().await;
        let _ = interaction.delete_response().await;
        if let Ok(path) = interaction.file() {
          let _ = std::fs::remove_file(path);
        }
      }
    }
    Command::Song(args) => {
      if let Ok(mut interaction) = Interaction::new(bot.clone(), msg, args).await {
        let _ = interaction.download_song().await;
      }
    }
  };

  Ok(())
}

struct Interaction {
  id: String,
  bot: Bot,
  msg: Message,
  size_limit: u32,
  args: Vec<String>,
  response: Option<Message>,
}

impl Interaction {
  async fn new(bot: Bot, msg: Message, args: String) -> Result<Self> {
    let args: Vec<String> = args.split(" ").map(|a| a.to_owned()).collect();

    let mut interaction = Self {
      id: rand_string(5),
      bot,
      msg,
      size_limit: Self::size_limit(&args),
      args,
      response: None,
    };

    if interaction.args[0].trim().is_empty() {
      interaction
        .respond("Oh sir.. did you mean to give a url? I didn't get one.")
        .await
        .unwrap();
      bail!("No URL");
    }

    Ok(interaction)
  }

  async fn respond(&mut self, msg: impl AsRef<str>) -> Result<()> {
    let msg = self
      .bot
      .send_message(self.msg.chat.id, msg.as_ref())
      .await?;
    self.response = Some(msg);

    Ok(())
  }

  async fn edit_response(&mut self, msg: impl AsRef<str>) -> Result<()> {
    if let Some(response) = &self.response {
      let response = self
        .bot
        .edit_message_text(response.chat.id, response.id, msg.as_ref())
        .await?;
      self.response = Some(response);
    } else {
      self.respond(msg).await?;
    }
    Ok(())
  }

  async fn delete_response(&mut self) -> Result<()> {
    if let Some(response) = &self.response {
      self
        .bot
        .delete_message(response.chat.id, response.id)
        .await?;
      self.response = None;
    }
    Ok(())
  }

  fn size_limit(params: &[impl AsRef<str>]) -> u32 {
    if let Some(size_limit) = params.get(1) {
      return size_limit.as_ref().parse().unwrap_or(DEFAULT_SIZE_LIMIT_MB);
    }
    DEFAULT_SIZE_LIMIT_MB
  }

  async fn download_song(&mut self) -> Result<()> {
    self
      .respond(format!(
        r#"Aye-aye cap'n! Let me ask the crew if they've heard of a song by the name "{}""#,
        self.args.join(" ")
      ))
      .await?;

    let bot = self.bot.clone();
    let chat_id = self.msg.chat.id;
    let args = self.args.clone();

    std::thread::spawn(move || -> Result<()> {
      let rt = Runtime::new().unwrap();

      rt.block_on(async {
        let song = crate::music::dl_search(args.join(" ")).await.unwrap();
        let input_file = InputFile::memory(song);
        bot
          .send_audio(chat_id, input_file)
          .caption("song.ogg")
          .await
          .unwrap();
      });

      Ok(())
    });

    Ok(())
  }

  async fn download_audio(&mut self) -> Result<()> {
    self
      .respond(format!(
        "Oh sure, cap'n! I'll get that for you. ({}MB limit)",
        self.size_limit
      ))
      .await?;

    let outfile = format!("{}/{}.%(ext)s", TMP_DIR, self.id);
    let dl_cmd = dl_cmd(&self.args[0], self.size_limit, &outfile)
      .extract_audio(true)
      .to_owned();

    let (file_path, caption) = self.run_download(&dl_cmd).await?;

    let file = std::fs::File::open(&file_path)?;
    let filesize = file.metadata().unwrap().len();

    if filesize >= DEFAULT_SIZE_LIMIT {
      let host_msg =
        "Oh Cap'n, this file is too large for Telegram. Let me host it for you!\n\nUploading...";
      self.edit_response(host_msg).await?;

      let extension = file_path.extension().unwrap().to_string_lossy();

      let s3_path = format!("{}.{extension}", self.id);
      crate::backblaze::put_vid(&s3_path, &file_path).await?;

      self
        .edit_response(format!(
          "Here it is, Cap'n! https://kota.is/v/{}.{extension}",
          self.id
        ))
        .await?;
      self.response = None;

      return Ok(());
    }

    self
      .edit_response("I got the file, sir! Sending it now...")
      .await?;

    self
      .bot
      .send_audio(self.msg.chat.id, InputFile::file(&file_path))
      .caption(caption)
      .await?;

    self.delete_response().await?;

    Ok(())
  }

  async fn download_video(&mut self) -> Result<()> {
    self
      .respond(format!(
        "Aye-aye cap'n! Downloading video with a {}MB filesize limit.",
        self.size_limit
      ))
      .await?;

    let outfile = format!("{}/{}.%(ext)s", TMP_DIR, self.id);
    let dl_cmd = dl_cmd(&self.args[0], self.size_limit, &outfile)
      .format("mp4")
      .to_owned();

    let (file_path, caption) = self.run_download(&dl_cmd).await?;

    let file = std::fs::File::open(&file_path)?;
    let filesize = file.metadata().unwrap().len();

    if filesize >= DEFAULT_SIZE_LIMIT {
      let host_msg =
        "Oh Cap'n, this file is too large for Telegram. Let me host it for you!\n\nUploading...";
      self.edit_response(host_msg).await?;

      let s3_path = format!("{}.mp4", self.id);
      crate::backblaze::put_vid(&s3_path, &file_path).await?;

      self
        .edit_response(format!(
          "Here it is, Cap'n! https://kota.is/v/{}.mp4",
          self.id
        ))
        .await?;
      self.response = None;

      return Ok(());
    }

    self
      .edit_response("I got the file, sir! Sending it now...")
      .await?;

    self
      .bot
      .send_video(self.msg.chat.id, InputFile::file(&file_path))
      .supports_streaming(true)
      .caption(caption)
      .await?;

    Ok(())
  }

  async fn run_download(&mut self, dl_cmd: &YoutubeDl) -> Result<(PathBuf, String)> {
    let result = dl_cmd.run()?;

    let mut caption: String = match result {
      YoutubeDlOutput::SingleVideo(video) => video.title,
      _ => String::from("No Title Found"),
    };
    caption.truncate(200);

    Ok((self.file()?, caption))
  }

  fn file(&self) -> Result<PathBuf> {
    glob(&format!("{}/{}*", TMP_DIR, self.id))?
      .nth(0)
      .map(|pb| pb.unwrap())
      .context("No download found.")
  }
}

fn dl_cmd(url: &str, size_limit: u32, outfile: &str) -> YoutubeDl {
  let dl_cmd = YoutubeDl::new(url)
    .cookies("cookies.txt")
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
  #[command(description = "does... something?")]
  Song(String),
}
