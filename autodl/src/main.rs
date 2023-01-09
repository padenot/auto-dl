#[macro_use]
extern crate rocket;

use crate::rocket::tokio::io::AsyncReadExt;
use chrono::{SecondsFormat, Utc};
use lazy_static::lazy_static;
use rocket::fairing::AdHoc;
use rocket::form::Form;
use rocket::form::Strict;
use rocket::fs::{relative, FileServer};
use rocket::serde::Deserialize;
use rocket::serde::Serialize;
use rocket::tokio::fs::File;
use rocket::State;
use rocket::{get, post, routes};
use rocket_dyn_templates::{context, Template};
use std::cmp::Reverse;
use std::error::Error;
use std::fs;
use std::fs::DirEntry;
use std::process::Command;
use std::process::Stdio;
use std::sync::Mutex;
use std::thread;

#[derive(Deserialize, Serialize, Clone)]
#[serde(crate = "rocket::serde")]
struct Config {
    log_dir: String,
    yt_dlp_path: String,
    output_paths: Vec<String>,
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
    log_file: String,
    config: Config,
    output_directory: String,
}

impl Task {
    fn new(request: &DownloadRequest, config: &State<Config>) -> Task {
        let id = date_string();
        let log_file = id.clone() + ".log";
        Task {
            id,
            url: request.url.into(),
            audio_only: request.audio_only,
            log_file,
            config: config.inner().clone(),
            output_directory: request.output_directory.into(),
        }
    }

    fn download_task(task: Task) -> Result<(), Box<dyn Error>> {
        let log_name = format!("{}/{}", task.config.log_dir, task.log_file);
        let log = std::fs::File::create(log_name).expect("failed to open log");
        let errors = log.try_clone()?;

        let output_format = format!(
            "{}/%(autonumber+0)04d - %(title)s [%(id)s].%(ext)s",
            task.output_directory
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

        let mut c = Command::new(task.config.yt_dlp_path);
        let c2 = c.args(&argz);

        info!("{:?}", c2);

        c2.stdout(Stdio::from(log))
            .stderr(Stdio::from(errors))
            .spawn()?
            .wait_with_output()?;

        Ok(())
    }

    fn run(&self) {
        let task_copy = self.clone();
        let id = self.id.clone();
        info!("Starting task {}", self.id);
        thread::spawn(move || {
            if let Err(e) = Task::download_task(task_copy) {
                error!("Task {} failure: {}", id, e.to_string());
            } else {
                info!("Task {} completed", id);
            }
            let mut list = TASK_LIST.lock().unwrap();
            if let Some(index) = list.iter().position(|t| t.id == id) {
                list.swap_remove(index);
            }
            warn!("Task not found?");
        });
    }
}

#[derive(Serialize)]
#[serde(crate = "rocket::serde")]
struct Log {
    id: String,
    text: String,
}

#[get("/logs")]
async fn logs(config: &State<Config>) -> Template {
    let mut logs = Vec::<Log>::new();
    println!("log path: {}", config.log_dir);
    let mut paths: Vec<DirEntry> = match fs::read_dir(&config.log_dir) {
        Ok(p) => p.map(|r| r.unwrap()).collect(),
        Err(e) => {
            logs.push(Log {
                id: "Error".into(),
                text: e.to_string(),
            });
            return Template::render("logs", context! {logs});
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
    Template::render("logs", context! {logs})
}

#[derive(FromForm, Serialize)]
#[serde(crate = "rocket::serde")]
struct DownloadRequest<'r> {
    url: &'r str,
    audio_only: bool,
    output_directory: &'r str,
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
async fn index() -> Template {
    Template::render("index", context! {})
}

#[launch]
fn rocket() -> _ {
    rocket::build()
        .mount("/", routes![index, logs, download])
        .mount("/static", FileServer::from(relative!("static")))
        .attach(AdHoc::config::<Config>())
        .attach(Template::fairing())
}
