use axum::{
    body::Body,
    extract::{ConnectInfo, Path as AxumPath, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use chrono::{DateTime, Local};
use clap::{Arg, Command};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use qrcode::{render::svg, render::unicode, QrCode};
use std::{
    env,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Mutex,
    time::SystemTime,
};
use log::{error, info};
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

fn start_logging(output_path: &str) {
    use log::LevelFilter;
    use log4rs::append::file::FileAppender;
    use log4rs::config::{Appender, Config, Root};
    use log4rs::encode::pattern::PatternEncoder;

    let logfile = FileAppender::builder()
        .encoder(Box::new(PatternEncoder::new(
            "[{d(%d-%m-%y %H:%M:%S)}] {l} - {m}{n}",
        )))
        .build(output_path)
        .unwrap();

    let config = Config::builder()
        .appender(Appender::builder().build("logfile", Box::new(logfile)))
        .build(Root::builder().appender("logfile").build(LevelFilter::Info))
        .unwrap();

    log4rs::init_config(config).unwrap();
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{:.2} {}", size, UNITS[unit])
    }
}

// Global template cache
lazy_static::lazy_static! {
    static ref TEMPLATE: Mutex<Option<String>> = Mutex::new(None);
}

fn load_template() -> Result<String, Box<dyn std::error::Error>> {
    let mut template = TEMPLATE.lock().unwrap();

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

#[tokio::main]
async fn main() {
    start_logging("output/output.log");

    let matches = Command::new("file-serve")
        .version("0.6")
        .about("Serve files through your LAN")
        .arg(
            Arg::new("port")
                .short('p')
                .long("port")
                .value_name("P")
                .help("Server port"),
        )
        .arg(
            Arg::new("folder")
                .short('f')
                .long("folder")
                .value_name("f")
                .help("Share folder"),
        )
        .get_matches();

    let mut port = 8080; // default
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

    let full_link = lan_urls(port);

    println!(
        "Serving '{}' on:\n{}\nPress Ctrl+C to stop.\n",
        state.root.display(),
        full_link
    );

    show_qr_code(&full_link);

    match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        }
        Err(_) => error!("Failed to bind to {}, port already in use.", addr),
    }
}

fn lan_urls(port: u16) -> String {
    let mut out = String::new();

    if let Ok(interfaces) = get_if_addrs::get_if_addrs() {
        for interface in interfaces {
            if interface.is_loopback() {
                continue;
            }
            if let std::net::IpAddr::V4(v4) = interface.ip() {
                out.push_str(&format!("   http://{}:{}\n", v4, port));
                break; // use the first found
            }
        }
    }
    if out.is_empty() {
        out.push_str(&format!("   http://127.0.01:{}\n", port));
    }
    out
}

async fn list_files(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    path: Option<AxumPath<String>>,
) -> impl IntoResponse {
    info!(
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

        let size_str = if row.is_dir {
            "<span style=\"color: #666;\">-</span>".to_string()
        } else {
            human_size(row.size)
        };

        let action = if row.is_dir {
            let folder_path = if current_path.is_empty() {
                encoded.clone()
            } else {
                format!("{}/{}", current_path, encoded)
            };
            format!("<a class=\"btn\" href=\"/browse/{}\">Open</a>", folder_path)
        } else {
            let file_path = if current_path.is_empty() {
                encoded.clone()
            } else {
                format!("{}/{}", current_path, encoded)
            };
            format!(
                "<a class=\"btn\" href=\"/download/{}\">Download</a>",
                file_path
            )
        };

        let name_display = if row.is_dir {
            format!("📁 {}", html_escape(&row.name))
        } else {
            format!("📄 {}", html_escape(&row.name))
        };

        file_rows.push_str(&format!(
            "<tr>\n  <td class=\"truncate\">{}</td>\n  <td>{}</td>\n  <td>{}</td>\n  <td>{}</td>\n</tr>",
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

            template
                .replace("{title_suffix}", &title_suffix)
                .replace("{breadcrumb}", &breadcrumb)
                .replace("{file_rows}", &file_rows)
        }
        Err(e) => {
            error!("Error loading template: {}", e);
            // Fallback to simple error page
            format!(
                "<h1>Error</h1><p>Failed to load template: {}</p>",
                html_escape(&e.to_string())
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
            breadcrumb.push_str(&html_escape(part));
        } else {
            breadcrumb.push_str(&format!(
                "<a href=\"/browse{}\">{}</a>",
                encoded_path,
                html_escape(part)
            ));
        }
    }

    breadcrumb
}

fn html_escape(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '<' => "&lt;".to_string(),
            '>' => "&gt;".to_string(),
            '"' => "&quot;".to_string(),
            '\'' => "&#39;".to_string(),
            '&' => "&amp;".to_string(),
            _ => c.to_string(),
        })
        .collect()
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

            info!("downloading file: {}", &file_path.display());
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
    let _ = f.read(&mut _buf).await;

    let mime = mime_guess::from_path(&canonical_target)
        .first_or_octet_stream()
        .to_string();
    Ok((f, mime))
}

fn error_page(msg: &str) -> String {
    format!(
        "<h1>Error while loading page.</h1><p>{}</p>",
        html_escape(msg)
    )
}

fn terminal_supports_images() -> Option<&'static str> {
    match env::var("TERM_PROGRAM") {
        Ok(val) if val == "iTerm.app" => return Some("iterm2"),
        _ => {}
    }

    match env::var("TERM") {
        Ok(val) if val.contains("kitty") => return Some("kitty"),
        _ => {}
    }

    None
}

fn show_qr_code(text: &str) {
    let code = QrCode::new(text).unwrap();

    match terminal_supports_images() {
        Some("iterm2") => {
            // Render SVG and encode for iTerm2 inline image protocol
            let svg = code.render::<svg::Color>().min_dimensions(200, 200).build();

            let encoded = STANDARD.encode(svg.as_bytes());

            println!(
                "\x1b]1337;File=inline=1;width=auto;height=auto;preserveAspectRatio=1:{}\x07",
                encoded
            );
        }
        Some("kitty") => {
            // Kitty uses its own graphics protocol
            let svg = code.render::<svg::Color>().min_dimensions(200, 200).build();

            let encoded = STANDARD.encode(svg.as_bytes());

            println!("\x1b_Gf=100,t=d,A=T,width=200,height=200;{}\x1b\\", encoded);
        }
        _ => {
            // Fallback to ASCII QR for others terminal
            let ascii_qr = code.render::<unicode::Dense1x2>().quiet_zone(true).build();
            println!("{}", ascii_qr);
        }
    }
}
