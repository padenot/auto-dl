#[macro_use]
extern crate rocket;

use crate::rocket::tokio::io::AsyncReadExt;
use chrono::{SecondsFormat, Utc};
use lazy_static::lazy_static;
use rocket::fairing::AdHoc;
use rocket::figment::providers::Format;
use rocket::figment::providers::Serialized;
use rocket::figment::providers::Toml;
use rocket::figment::Figment;
use rocket::form::Form;
use rocket::form::Strict;
use rocket::fs::{relative, FileServer};
use rocket::response::status::NotFound;
use rocket::serde::Deserialize;
use rocket::serde::Serialize;
use rocket::tokio::fs::File;
use rocket::Request;
use rocket::State;
use rocket::{get, post, routes};
use rocket_dyn_templates::{context, Template};
use std::cmp::Reverse;
use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::fs::DirEntry;
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::process::Stdio;
use std::sync::Mutex;
use std::thread;

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(crate = "rocket::serde")]
struct Config {
    log_dir: String,
    yt_dlp_path: String,
    rsync_path: String,
    output_directories: HashMap<String, String>,
    delete_files_after_move: bool,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            log_dir: "./logs/".into(),
            yt_dlp_path: "./yt-dlp".into(),
            rsync_path: "rsync".into(),
            output_directories: HashMap::new(),
            delete_files_after_move: true,
        }
    }
}

lazy_static! {
    static ref TASK_LIST: Mutex<Vec<Task>> = Mutex::new(vec![]);
}

#[derive(Clone, Serialize)]
#[serde(crate = "rocket::serde")]
struct Task {
    id: String,
    url: String,
    audio_only: bool,
    log_file_path: String,
    config: Config,
    output_directory: String,
    move_directory: Option<String>,
    subdir: String,
}

fn command_to_string(command: &Command) -> String {
    let mut rv = String::new();
    rv += &command.get_program().to_string_lossy();
    rv += " ";
    rv += &command
        .get_args()
        .map(|s| s.to_str().unwrap_or("???"))
        .collect::<Vec<&str>>()
        .join(" ");
    // for_each(|s| rv += s.to_str().unwrap_or("???"); rv += " ");
    rv
}

impl Task {
    fn new(request: &DownloadRequest, config: &State<Config>) -> Task {
        let id = date_string();
        let log_file_path = format!("{}/{}.log", config.log_dir, &id);
        let move_directory = match config.output_directories.get(request.output_directory) {
            Some(output_dir) => Some(output_dir.clone()),
            None => {
                error!(
                    "Configuration error {} not found in output directory table.",
                    request.output_directory
                );
                None
            }
        };
        Task {
            id,
            url: request.url.into(),
            audio_only: request.audio_only,
            log_file_path,
            config: config.inner().clone(),
            output_directory: request.output_directory.into(),
            move_directory,
            subdir: request.subdir.into(),
        }
    }

    fn download_task(
        task: &Task,
        fds: (std::fs::File, std::fs::File),
    ) -> Result<(), Box<dyn Error>> {
        let output_format = format!(
            "{}/{}/%(autonumber+0)04d - %(title)s [%(id)s].%(ext)s",
            task.output_directory, task.subdir
        );

        let urls = task.url.split_whitespace().collect::<Vec<&str>>();

        let mut argz: Vec<&str> = vec!["-o", output_format.as_str()];

        if task.audio_only {
            argz.extend(vec![
                "--format",
                "bestaudio",
                "--extract-audio",
                "--audio-format",
                "mp3",
                "--audio-quality",
                "320K",
            ]);
        }

        argz.extend(urls);

        let mut c = Command::new(&task.config.yt_dlp_path);
        let c2 = c.args(&argz);

        write!(fds.0.try_clone()?, "{:?}\n", command_to_string(c2))?;

        c2.stdout(Stdio::from(fds.0))
            .stderr(Stdio::from(fds.1))
            .spawn()?
            .wait_with_output()?;

        Ok(())
    }

    fn move_task(task: &Task, fds: (std::fs::File, std::fs::File)) -> Result<(), Box<dyn Error>> {
        let move_directory = match &task.move_directory {
            Some(d) => d,
            None => {
                write!(
                    fds.0.try_clone()?,
                    "Not moving file for output directory {}",
                    task.output_directory
                )?;
                return Ok(());
            }
        };

        // berk
        if !move_directory.contains("@") {
            fs::create_dir_all(&move_directory)?;
        }

        let mut argz: Vec<&str> = vec!["-v", "--progress", "-r"];
        if task.config.delete_files_after_move {
            argz.push("--remove-source-files");
        }
        let move_path = Path::new(&task.output_directory).join(&task.subdir);
        let source = match move_path.to_str() {
            Some(s) => s.as_ref(),
            None => {
                return Err("Invalid destination directory when attempting to move files.".into());
            }
        };

        argz.push(&source);
        argz.push(&move_directory);

        let mut c = Command::new(&task.config.rsync_path);
        let c2 = c.args(&argz);

        write!(fds.0.try_clone()?, "{:?}\n", command_to_string(c2))?;

        c2.stdout(Stdio::from(fds.0))
            .stderr(Stdio::from(fds.1))
            .spawn()?
            .wait_with_output()?;

        Ok(())
    }

    fn run_thread(task: &Task) -> Result<(), Box<dyn Error>> {
        let mut log = std::fs::File::create(&task.log_file_path)
            .expect("failed to create log file in download task");

        if let Err(e) = Task::download_task(&task, (log.try_clone()?, log.try_clone()?)) {
            write!(log, "Download {} failure: {}", task.id, e.to_string())?;
            error!("Task {} error", task.id);
        } else {
            info!("Download {} completed", task.id);
        }

        if let Err(e) = Task::move_task(&task, (log.try_clone()?, log.try_clone()?)) {
            return Err(format!("File move {} failure: {}", task.id, e.to_string()).into());
        } else {
            info!("Download {} completed", task.id);
        }
        let mut list = TASK_LIST.lock().unwrap();
        if let Some(index) = list.iter().position(|t| t.id == task.id) {
            list.swap_remove(index);
        } else {
            warn!("Task not found?");
        }

        Ok(())
    }

    fn run(&self) {
        let task_copy = self.clone();
        info!("Starting task {}", self.id);
        thread::spawn(move || match Task::run_thread(&task_copy) {
            Ok(_) => {
                info!("Task {} finished successfuly", task_copy.id);
            }
            Err(e) => {
                error!("Task {} error: {}", task_copy.id, e.to_string());
            }
        });
    }
}

#[derive(Serialize)]
#[serde(crate = "rocket::serde")]
struct Log {
    id: String,
    text: String,
}

fn list_log_files(log_dir: &str) -> Result<Vec<DirEntry>, Box<dyn Error>> {
    let mut paths = Vec::<DirEntry>::new();
    let dir = fs::read_dir(log_dir)?;
    for entry in dir {
        if let Ok(e) = entry {
            paths.push(e);
        }
    }
    Ok(paths)
}

#[get("/logs")]
async fn logs(config: &State<Config>) -> Result<Template, NotFound<String>> {
    let mut logs = Vec::<Log>::new();
    info!("Log path: {}", config.log_dir);

    let mut paths = match list_log_files(&config.log_dir) {
        Ok(p) => p,
        Err(e) => {
            return Err(NotFound(e.to_string()));
        }
    };

    paths.sort_by_key(|dir| Reverse(dir.path()));

    for path in paths {
        let mut buf = String::new();
        if let Ok(mut content) = File::open(&path.path()).await {
            let log = match content.read_to_string(&mut buf).await {
                Ok(_) => Log {
                    id: path.file_name().to_string_lossy().into_owned(),
                    text: buf,
                },
                Err(e) => Log {
                    id: "Error".into(),
                    text: e.to_string(),
                },
            };
            logs.push(log);
        }
    }
    Ok(Template::render("logs", context! {logs}))
}

#[derive(FromForm, Serialize)]
#[serde(crate = "rocket::serde")]
struct DownloadRequest<'r> {
    url: &'r str,
    audio_only: bool,
    output_directory: &'r str,
    subdir: &'r str,
}

#[post("/download", data = "<download_request>")]
fn download(
    download_request: Form<Strict<DownloadRequest<'_>>>,
    config: &State<Config>,
) -> Template {
    let request = download_request.into_inner().into_inner();
    let t = Task::new(&request, config);
    t.run();
    TASK_LIST.lock().unwrap().push(t);
    Template::render("download", context! {download_request: request})
}

fn date_string() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[get("/")]
async fn index(config: &State<Config>) -> Template {
    let task_list = TASK_LIST.lock().unwrap().clone();
    Template::render(
        "index",
        context! {output_directories: &config.output_directories.keys().collect::<Vec<&String>>(), task_list},
    )
}

#[catch(500)]
fn internal_error() -> Template {
    Template::render("error", context! { message: "500, didn't work mate, sorry"})
}

#[catch(404)]
fn not_found(req: &Request) -> Template {
    Template::render(
        "error",
        context! { message: format!("404, couln't find {}, sorry", req.uri())},
    )
}

#[launch]
fn rocket() -> _ {
    let figment = Figment::from(rocket::Config::default())
        .merge(Toml::file("autodl.toml").nested())
        .join(Serialized::defaults(Config::default()));

    let mut config: Config = figment.extract().unwrap_or(Config::default());
    if config.output_directories.is_empty() {
        config.output_directories.insert(".".into(), ".".into());
    }

    rocket::custom(figment)
        .mount("/", routes![index, logs, download])
        .mount("/static", FileServer::from(relative!("static")))
        .register("/", catchers![internal_error, not_found])
        .attach(AdHoc::config::<Config>())
        .attach(Template::fairing())
}
