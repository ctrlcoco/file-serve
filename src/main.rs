mod utils;

use axum::{
    body::Body,
    extract::{ConnectInfo, Path as AxumPath, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};

use chrono::{DateTime, Local};
use clap::{Arg, Command};
use log;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};

use std::{
    env,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Mutex,
    time::SystemTime,
};
use tokio::{fs, io::AsyncReadExt};
use tokio_util::io::ReaderStream;

#[derive(Clone)]
struct AppState {
    root: PathBuf,
}

struct FileRow {
    name: String,
    size: u64,
    modified: Option<SystemTime>,
    is_dir: bool,
}

// Global template cache
lazy_static::lazy_static! {
    static ref MAIN_TEMPLATE: Mutex<Option<String>> = Mutex::new(None);
    static ref ERROR_TEMPLATE: Mutex<Option<String>> = Mutex::new(None);
}

fn load_template() -> Result<String, Box<dyn std::error::Error>> {
    let mut template = MAIN_TEMPLATE.lock().unwrap();

    // if not cached load it
    if template.is_none() {
        let template_path = "templates/index.html";
        let content = std::fs::read_to_string(template_path)
            .map_err(|e| format!("Failed to read template file: {}", e))?;
        *template = Some(content);
    }

    template
        .as_ref()
        .cloned()
        .ok_or_else(|| "Template not loaded".into())
}

fn load_error_template() -> Result<String, Box<dyn std::error::Error>> {
    let mut template = ERROR_TEMPLATE.lock().unwrap();

    if template.is_none() {
        let template_path = "templates/error.html";
        let content = std::fs::read_to_string(template_path)
            .map_err(|e| format!("Failed to read error template file: {}", e))?;
        *template = Some(content);
    }

    template
        .as_ref()
        .cloned()
        .ok_or_else(|| "Error template not loaded".into())
}

#[tokio::main]
async fn main() {
    utils::start_logging("logs/file_serve.log");

    let matches = Command::new("file-serve")
        .version("0.6")
        .about("Serve files through your LAN")
        .arg(
            Arg::new("port")
                .short('p')
                .long("port")
                .value_name("P")
                .help("Server port, defaults to 8080."),
        )
        .arg(
            Arg::new("folder")
                .short('f')
                .long("folder")
                .value_name("f")
                .help("Folder to be served, default is current folder."),
        )
        .arg(
            Arg::new("interface")
                .short('i')
                .long("interface")
                .value_name("i")
                .help("Interface to bind, default is first occurring interface."),
        )
        .get_matches();

    let mut port = 8080; // default port
    if let Some(p) = matches.get_one::<String>("port") {
        port = p.parse::<u16>().expect("port must be a number");
    }

    let mut root = env::current_dir().expect("Failed to get current dir");
    if let Some(f) = matches.get_one::<String>("folder") {
        root.push(f.as_str());
    }

    let state = AppState { root };

    // Build router
    let app = Router::new()
        .route("/", get(list_files))
        .route("/browse/{*path}", get(list_files))
        .route("/download/{*path}", get(download_file))
        .with_state(state.clone()); // clone to not consume

    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    let add: String;
    if let Some(f) = matches.get_one::<String>("interface") {
        add = format!("{}", f);
    } else {
        add = get_address()
    }

    let full_link: String = format!("http://{}:{}\n", add, port);

    println!(
        "Serving '{}' on:\n    {}\nPress Ctrl+C to stop.\n{}",
        state.root.display(),
        full_link,
        utils::get_qr_code(&full_link)
    );

    match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        }
        Err(err) => log::error!("Failed to run TCP listener {}\n{}.", addr, err),
    }
}

fn get_address() -> String {
    if let Ok(interfaces) = get_if_addrs::get_if_addrs() {
        for interface in interfaces {
            if interface.is_loopback() {
                continue;
            }
            if let std::net::IpAddr::V4(v4) = interface.ip() {
                return format!("{}", v4); // use the first found
            }
        }
    }
    // loopback for testing purpose
    "127.0.01".to_string()
}

async fn list_files(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    path: Option<AxumPath<String>>,
) -> impl IntoResponse {
    log::info!(
        "[LIST] Client: {} | UA: {}",
        addr,
        headers
            .get(header::USER_AGENT)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("-")
    );

    // Determine the directory to list
    let current_path = if let Some(ref path) = path {
        state.root.join(path.as_str())
    } else {
        state.root.clone()
    };

    let mut entries = match fs::read_dir(&current_path).await {
        Ok(rd) => rd,
        Err(e) => {
            let msg = format!("Failed to read directory: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, Html(error_page(&msg)));
        }
    };

    let mut rows: Vec<FileRow> = Vec::new();

    while let Ok(Some(entry)) = entries.next_entry().await {
        let file_name = match entry.file_name().into_string() {
            Ok(s) => s,
            Err(_) => continue, // skip non-utf8 names
        };
        let meta = match entry.metadata().await {
            Ok(m) => m,
            Err(_) => continue,
        };
        let is_dir = meta.is_dir();
        let size = if is_dir { 0 } else { meta.len() };
        let modified: Option<SystemTime> = meta.modified().ok();
        rows.push(FileRow {
            name: file_name,
            size,
            modified,
            is_dir,
        });
    }

    // Sort by name ascending, directories first
    rows.sort_by(|a, b| {
        if a.is_dir != b.is_dir {
            return b.is_dir.cmp(&a.is_dir); // directories first
        }
        a.name.to_lowercase().cmp(&b.name.to_lowercase())
    });

    let current_path_str = path.as_deref().map_or("", |v| v);
    (StatusCode::OK, Html(render_index(rows, current_path_str)))
}

fn render_index(rows: Vec<FileRow>, current_path: &str) -> String {
    let mut file_rows = String::new();
    for row in rows {
        let encoded = utf8_percent_encode(&row.name, NON_ALPHANUMERIC).to_string();

        let name_display = if row.is_dir {
            format!("📁 {}", utils::html_escape(&row.name))
        } else {
            format!("📄 {}", utils::html_escape(&row.name))
        };

        let size_str = if row.is_dir {
            "-".to_string()
        } else {
            utils::bytes_to_human_size(row.size)
        };

        let modified_str = row
            .modified
            .and_then(|st| {
                Some(
                    DateTime::<Local>::from(st)
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string(),
                )
            })
            .unwrap_or_else(|| "-".to_string());

        let element_path = if current_path.is_empty() {
            encoded.clone()
        } else {
            format!("{}/{}", current_path, encoded)
        };

        let action = if row.is_dir {
            format!("browse/{}\">Open", element_path)
        } else {
            format!("download/{}\">Download", element_path)
        };

        file_rows.push_str(&format!(
            "<tr>\n  <td class=\"truncate\">{}</td>\n  <td>{}</td>\n  <td>{}</td>\n  <td><a class=\"btn\"href=\"/{}<a></td>\n</tr>",
            name_display, size_str, modified_str, action
        ));
    }

    // Generate breadcrumb navigation
    let breadcrumb = generate_breadcrumb(current_path);

    // Load and render template
    match load_template() {
        Ok(template) => {
            let title_suffix = if current_path.is_empty() {
                " - home".to_string()
            } else {
                format!(" - {}", current_path)
            };
            // Compute back button (only if inside a subfolder)
            let back_button = if current_path.is_empty() {
                String::new()
            } else {
                let mut parts: Vec<&str> =
                    current_path.split('/').filter(|s| !s.is_empty()).collect();
                let _ = parts.pop();
                let href = if parts.is_empty() {
                    "/".to_string()
                } else {
                    format!("/browse/{}", parts.join("/"))
                };
                format!(
                    "<p><a class=\"btn btn-secondary\" href=\"{}\">← Back</a></p>",
                    href
                )
            };

            // loading data into template
            template
                .replace("{title_suffix}", &title_suffix)
                .replace("{breadcrumb}", &breadcrumb)
                .replace("{back_button}", &back_button)
                .replace("{file_rows}", &file_rows)
        }
        Err(e) => {
            log::error!("Error loading template: {}", e);
            // Fallback to simple error page
            format!(
                "<h1>Error</h1><p>Failed to load template: {}</p>",
                utils::html_escape(&e.to_string())
            )
        }
    }
}

fn generate_breadcrumb(current_path: &str) -> String {
    let mut breadcrumb = String::from("<a href=\"/\">Home</a>");

    if current_path.is_empty() {
        return breadcrumb;
    }

    let path_parts: Vec<&str> = current_path.split('/').filter(|s| !s.is_empty()).collect();

    let mut current_breadcrumb_path = String::new();
    for (i, part) in path_parts.iter().enumerate() {
        current_breadcrumb_path.push_str("/");
        current_breadcrumb_path.push_str(part);

        let _encoded_part = utf8_percent_encode(part, NON_ALPHANUMERIC).to_string();
        let encoded_path =
            utf8_percent_encode(&current_breadcrumb_path, NON_ALPHANUMERIC).to_string();

        breadcrumb.push_str(" / ");
        if i == path_parts.len() - 1 {
            // Last part is not clickable
            breadcrumb.push_str(&utils::html_escape(part));
        } else {
            breadcrumb.push_str(&format!(
                "<a href=\"/browse{}\">{}</a>",
                encoded_path,
                utils::html_escape(part)
            ));
        }
    }

    breadcrumb
}

async fn download_file(
    State(state): State<AppState>,
    AxumPath(path): AxumPath<String>,
) -> Response {
    // Security check: prevent directory traversal attacks
    if path.contains("..") || path.starts_with('/') || path.starts_with('\\') {
        return (StatusCode::BAD_REQUEST, "Invalid file path").into_response();
    }

    let file_path: PathBuf = state.root.join(&path);

    match safe_open(&state.root, &file_path).await {
        Ok((file, mime)) => {
            let stream = ReaderStream::new(file);
            let body = Body::from_stream(stream);
            let mut res = Response::new(body);
            let headers = res.headers_mut();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_str(&mime)
                    .unwrap_or(HeaderValue::from_static("application/octet-stream")),
            );

            // Extract just the filename for the download
            let filename = file_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("download");
            let disposition = format!("attachment; filename=\"{}\"", filename);
            headers.insert(
                header::CONTENT_DISPOSITION,
                HeaderValue::from_str(&disposition).unwrap(),
            );

            log::info!("downloading file: {}", &file_path.display());
            res
        }
        Err((status, msg)) => (status, msg).into_response(),
    }
}

async fn safe_open(root: &Path, target: &Path) -> Result<(fs::File, String), (StatusCode, String)> {
    let canonical_root = root.canonicalize().map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to access root".to_string(),
        )
    })?;
    let canonical_target = target
        .canonicalize()
        .map_err(|_| (StatusCode::NOT_FOUND, "File not found".to_string()))?;
    if !canonical_target.starts_with(&canonical_root) {
        return Err((StatusCode::FORBIDDEN, "Access denied".to_string()));
    }
    let mut f = fs::File::open(&canonical_target)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "File not found".to_string()))?;

    let mut _buf = [0u8; 0];
    match f.read(&mut _buf).await {
        Ok(_) => {
            let mime = mime_guess::from_path(&canonical_target)
                .first_or_octet_stream()
                .to_string();
            Ok((f, mime))
        }

        Err(err) => {
            log::error!("cannot open file {}\n{}", &canonical_root.display(), err);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "Cannot open desired file.".to_string(),
            ))
        }
    }
}

fn error_page(msg: &str) -> String {
    match load_error_template() {
        Ok(template) => template.replace("{error_message}", &utils::html_escape(msg)),
        Err(e) => {
            log::error!("Error loading error template: {}", e);
            // Fallback to simple error page
            format!(
                "<h1>Error</h1><p>Failed to load error template: {}</p><p>Error: {}</p>",
                utils::html_escape(&e.to_string()),
                utils::html_escape(msg)
            )
        }
    }
}
