#[macro_use]
extern crate rocket;

use crate::rocket::tokio::io::AsyncReadExt;
use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use chrono::{SecondsFormat, Utc};
use lazy_static::lazy_static;
use path_clean::PathClean;
use rocket::fairing::AdHoc;
use rocket::figment::providers::Format;
use rocket::figment::providers::Serialized;
use rocket::figment::providers::Toml;
use rocket::figment::Figment;
use rocket::form::Form;
use rocket::form::Strict;
use rocket::fs::FileServer;
use rocket::response::status::NotFound;
use rocket::response::Redirect;
use rocket::serde::Deserialize;
use rocket::serde::Serialize;
use rocket::tokio::fs::File;
use rocket::Request;
use rocket::State;
use rocket::{get, post, routes};
use rocket_dyn_templates::{context, Template};
use std::cmp::Reverse;
use std::fs;
use std::fs::DirEntry;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::sync::Mutex;
use std::thread;

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(crate = "rocket::serde")]
struct RsyncSpec {
    destination: String,
    extra_args: String,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(crate = "rocket::serde")]
struct FileMoveSpec {
    source: String,
    destination_local: Option<String>,
    destination_remote: Option<RsyncSpec>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(crate = "rocket::serde")]
struct Config {
    log_dir: String,
    ytdlp_path: String,
    rsync_path: String,
    rsync_extra_args: String,
    output_directories: Vec<FileMoveSpec>,
    delete_files_after_move: bool,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            log_dir: "./logs/".into(),
            ytdlp_path: "./yt-dlp".into(),
            rsync_path: "rsync".into(),
            rsync_extra_args: "".into(),
            output_directories: Vec::<FileMoveSpec>::new(),
            delete_files_after_move: true,
        }
    }
}

lazy_static! {
    static ref TASK_LIST: Mutex<Vec<Task>> = Mutex::new(vec![]);
}

#[derive(Clone, Serialize)]
#[serde(crate = "rocket::serde")]
struct DownloadSpec {
    url: String,
    audio_only: bool,
    output_directory: String,
    file_move_spec: FileMoveSpec,
    subdir: String,
}

#[derive(Clone, Serialize)]
#[serde(crate = "rocket::serde")]
enum TaskData {
    Download { spec: DownloadSpec },
    Update,
}

#[derive(Clone, Serialize)]
#[serde(crate = "rocket::serde")]
struct Task {
    id: String,
    log_file_path: String,
    config: Config,
    data: TaskData,
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
    rv
}

impl Task {
    fn new(request: &TaskData, config: &State<Config>) -> Result<Task> {
        let id = date_string();
        let log_file_path = format!("{}/{}.log", config.log_dir, &id);
        match request {
            TaskData::Download { spec } => {
                let file_move_spec = config
                    .output_directories
                    .iter()
                    .find(|e| e.source == spec.output_directory)
                    .with_context(|| {
                        format!(
                            "Bad request -- {} not found in config",
                            spec.output_directory
                        )
                    })?;

                // Validate output directory
                let output_dir = std::fs::canonicalize(&file_move_spec.source)?;
                let mut final_output_path = PathBuf::from(&output_dir);
                final_output_path.push(&spec.subdir);
                let cleaned = final_output_path.clean();
                if !cleaned.starts_with(&output_dir) {
                    return Err(anyhow!(
                        "Bad request -- something funny in the path {}",
                        spec.subdir
                    ));
                }
                Ok(Task {
                    id,
                    log_file_path,
                    config: config.inner().clone(),
                    data: TaskData::Download {
                        spec: {
                            DownloadSpec {
                                url: spec.url.clone(),
                                audio_only: spec.audio_only,
                                output_directory: spec.output_directory.clone(),
                                file_move_spec: file_move_spec.clone(),
                                subdir: spec.subdir.clone(),
                            }
                        },
                    },
                })
            }
            TaskData::Update => Ok(Task {
                id,
                log_file_path,
                config: config.inner().clone(),
                data: TaskData::Update,
            }),
        }
    }

    fn download_task(
        task: &DownloadSpec,
        fds: (std::fs::File, std::fs::File),
        config: &Config,
    ) -> Result<()> {
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

        let mut c = Command::new(&config.ytdlp_path);
        let c2 = c.args(&argz);

        write!(fds.0.try_clone()?, "\n{:?}\n", command_to_string(c2))?;

        c2.stdout(Stdio::from(fds.0))
            .stderr(Stdio::from(fds.1))
            .spawn()?
            .wait_with_output()?;

        Ok(())
    }

    fn move_task(
        task: &DownloadSpec,
        fds: (std::fs::File, std::fs::File),
        config: &Config,
    ) -> Result<()> {
        if task.file_move_spec.destination_local.is_none()
            && task.file_move_spec.destination_remote.is_none()
        {
            return Ok(());
        }
        if let Some(local_dir) = &task.file_move_spec.destination_local {
            fs::create_dir_all(local_dir)?;
        }

        let mut argz: Vec<&str> = vec!["-v", "--progress", "-rp"];
        if config.delete_files_after_move {
            argz.push("--remove-source-files");
        }
        let move_path = Path::new(&task.output_directory).join(&task.subdir);
        let p = move_path.clone();
        let source = p.to_str().context("Invalid destination directory when attempting to move files.")?;


        let p = move_path.clone();
        let path_str = p.into_os_string().into_string().unwrap();
        let mut chmod_cmd = Command::new("chmod");
        chmod_cmd.args(["-R", "a+r", &path_str]);
        writeln!(fds.0.try_clone()?, "{:?}", command_to_string(&chmod_cmd))?;

        chmod_cmd.stdout(Stdio::from(fds.0.try_clone()?))
            .stderr(Stdio::from(fds.1.try_clone()?))
            .spawn()?
            .wait_with_output()?;

        if let Some(rsync_opts) = &task.file_move_spec.destination_remote {
            argz.push(&rsync_opts.extra_args);
            argz.push(source);
            argz.push(&rsync_opts.destination);
        } else if let Some(local_dir) = &task.file_move_spec.destination_local {
            argz.push(source);
            argz.push(local_dir);
        }

        let mut c = Command::new(&config.rsync_path);
        c.args(&argz);

        writeln!(fds.0.try_clone()?, "{:?}", command_to_string(&c))?;

        c.stdout(Stdio::from(fds.0))
            .stderr(Stdio::from(fds.1))
            .spawn()?
            .wait_with_output()?;

        Ok(())
    }

    fn run_thread(task: &Task) -> Result<()> {
        let mut log = std::fs::File::create(&task.log_file_path)
            .expect("failed to create log file in download task");

        match &task.data {
            TaskData::Download { spec } => {
                std::fs::create_dir_all(&spec.output_directory)
                    .expect("failed to created output dir in download task");
                if let Err(e) =
                    Task::download_task(spec, (log.try_clone()?, log.try_clone()?), &task.config)
                {
                    write!(log, "Download {} failure: {}", task.id, e)?;
                    error!("Task {} error", task.id);
                } else {
                    info!("Download {} completed", task.id);
                }

                Task::move_task(spec, (log.try_clone()?, log.try_clone()?), &task.config)
                    .with_context(|| format!("Error for file move {}", task.id))?;
            }
            TaskData::Update => {
                let log2 = log.try_clone()?;
                let argz: Vec<&str> = vec!["-U"];
                let mut c = Command::new(&task.config.ytdlp_path);
                let c2 = c.args(&argz);
                write!(log.try_clone()?, "\n{:?}\n", command_to_string(c2))?;
                c2.stdout(Stdio::from(log))
                    .stderr(Stdio::from(log2))
                    .spawn()?
                    .wait_with_output()?;
            }
        };

        info!("Download {} completed", task.id);
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

fn list_log_files(log_dir: &str) -> Result<Vec<DirEntry>> {
    let mut paths = Vec::<DirEntry>::new();
    let dir = fs::read_dir(log_dir)?;
    for entry in dir.flatten() {
        paths.push(entry);
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

fn remove_all_logs(log_dir: &str) -> Result<()> {
    for file in list_log_files(log_dir)? {
        std::fs::remove_file(file.path())?;
    }
    Ok(())
}

#[delete("/logs/<id>")]
async fn delete_logs(id: String, config: &State<Config>) -> Redirect {
    info!("DELETE /logs");

    match id.as_str() {
        "all" => {
            if let Err(e) = remove_all_logs(&config.log_dir) {
                error!("Error when removing log files: {}", e.to_string());
            }
        }
        _ => {
            let mut log_path = PathBuf::from(&config.log_dir);
            log_path.push(&id);
            if let Err(e) = std::fs::remove_file(log_path) {
                error!("Error when removing log file: {}: {}", id, e.to_string());
            }
        }
    };

    Redirect::to(uri!("/logs"))
}

#[get("/update-yt-dlp")]
async fn update_yt_dlp(config: &State<Config>) -> Redirect {
    info!("GET /update-yt-dlp");

    match Task::new(&TaskData::Update, config) {
        Ok(t) => {
            t.run();
            TASK_LIST.lock().unwrap().push(t);
            Template::render("update", context! {});
        }
        Err(e) => {
            Template::render("error", context! {message: e.to_string()});
        }
    }

    Redirect::to(uri!("/logs"))
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
    let taskdata = TaskData::Download {
        spec: DownloadSpec {
            url: request.url.to_string(),
            audio_only: request.audio_only,
            output_directory: request.output_directory.to_string(),
            file_move_spec: FileMoveSpec {
                source: "()".to_string(),
                destination_local: None,
                destination_remote: None,
            },
            subdir: request.subdir.to_string(),
        },
    };
    match Task::new(&taskdata, config) {
        Ok(t) => {
            t.run();
            TASK_LIST.lock().unwrap().push(t);
            Template::render("download", context! {download_request: request})
        }
        Err(e) => {
            Template::render("error", context! {message: e.to_string()})
        }
    }
}

fn date_string() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[get("/")]
async fn index(config: &State<Config>) -> Template {
    let task_list = TASK_LIST.lock().unwrap().clone();
    Template::render(
        "index",
        context! {output_directories: &config.output_directories.iter().map(|e| e.source.clone()).collect::<Vec<String>>(), task_list},
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

fn check_ytdlp_path_valid(ytdlp_path: &str) -> Result<std::process::Output> {
    let mut c = Command::new(ytdlp_path);
    let c = c.args(vec!["--help"]);
    c.output()
        .with_context(|| format!("yt-dlp could't be run from path {}", ytdlp_path))
}

#[launch]
fn rocket() -> _ {
    let figment = Figment::from(rocket::Config::default())
        .merge(Toml::file("autodl.toml").nested())
        .join(Serialized::defaults(Config::default()));

    let mut config: Config = figment.extract().unwrap_or_default();
    if config.output_directories.is_empty() {
        config.output_directories.push(FileMoveSpec {
            destination_local: Some(".".into()),
            destination_remote: None,
            source: ".".into(),
        });
    }

    if let Err(e) = check_ytdlp_path_valid(&config.ytdlp_path) {
        panic!("Error while checking configuration: {}", e);
    }

    rocket::custom(figment)
        .mount(
            "/",
            routes![index, logs, delete_logs, download, update_yt_dlp],
        )
        .mount("/static", FileServer::from("static"))
        .register("/", catchers![internal_error, not_found])
        .attach(AdHoc::config::<Config>())
        .attach(Template::fairing())
}
