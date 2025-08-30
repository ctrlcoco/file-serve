use axum::{
    body::Body,
    extract::{ConnectInfo, Path as AxumPath, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};

use chrono::{DateTime, Local};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};

use std::{
    env,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::SystemTime,
};

use tokio::{fs, io::AsyncReadExt};

use tokio_util::io::ReaderStream;

use base64::{engine::general_purpose::STANDARD, Engine};

use qrcode::{render::svg, render::unicode, QrCode};

use clap::{Arg, Command};

#[derive(Clone)]
struct AppState {
    root: PathBuf,
}

struct FileRow {
    name: String,
    size: u64,
    modified: Option<SystemTime>,
}

#[tokio::main]
async fn main() {
    let matches = Command::new("file-serve")
        .version("0.6")
        .about("Terminal countdown timer with days, hours, minutes, seconds")
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
        .route("/download/{name}", get(download_file))
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
        Err(_) => println!("Failed to bind to {}, port already in use.", addr),
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
) -> impl IntoResponse {
    // loggo le info del cliente
    println!(
        "[LIST] Client: {} | UA: {}",
        addr,
        headers
            .get(header::USER_AGENT)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("-")
    );

    // Read current directory (non-recursive)
    let mut entries = match fs::read_dir(&state.root).await {
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
        if !meta.is_file() {
            continue;
        }
        let size = meta.len();
        let modified: Option<SystemTime> = meta.modified().ok();
        rows.push(FileRow {
            name: file_name,
            size,
            modified,
        });
    }

    // Sort by name ascending
    rows.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    (StatusCode::INTERNAL_SERVER_ERROR, Html(render_index(rows)))
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

fn render_index(rows: Vec<FileRow>) -> String {
    let mut body = String::new();
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
        body.push_str(&format!(
            "<tr>\n  <td class=\"truncate\">{}</td>\n  <td>{}</td>\n  <td>{}</td>\n  <td><a class=\"btn\" href=\"/download/{}\">Download</a></td>\n</tr>",
            html_escape(&row.name), human_size(row.size), modified_str, encoded
        ));
    }

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>LAN File Server</title>
  <style>
    body {{ font-family: system-ui, -apple-system, Segoe UI, Roboto, sans-serif; margin: 2rem; }}
    h1 {{ margin-bottom: 1rem; }}
    table {{ width: 100%; border-collapse: collapse; }}
    th, td {{ border-bottom: 1px solid #ddd; padding: 0.6rem; text-align: left; }}
    th {{ background: #f7f7f7; position: sticky; top: 0; }}
    .truncate {{ max-width: 40vw; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }}
    .btn {{ display: inline-block; padding: 0.4rem 0.8rem; border: 1px solid #333; border-radius: 8px; text-decoration: none; }}
    .footer {{ margin-top: 1rem; color: #666; font-size: 0.9rem; }}
  </style>
</head>
<body>
  <h1>Files listing</h1>
  <table>
    <thead>
      <tr>
        <th>Name</th>
        <th>Size</th>
        <th>Modified</th>
        <th>Action</th>
      </tr>
    </thead>
    <tbody>
      {body}
    </tbody>
  </table>
  <div class="footer">Accessible over LAN. Keep this window running.</div>
</body>
</html>"#
    )
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
    AxumPath(name): AxumPath<String>,
) -> Response {
    if name.contains('/') || name.contains('\\') {
        return (StatusCode::BAD_REQUEST, "Invalid file name").into_response();
    }

    let path = state.root.join(&name);

    match safe_open(&state.root, &path).await {
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
            let disposition = format!("attachment; filename=\"{}\"", name);
            headers.insert(
                header::CONTENT_DISPOSITION,
                HeaderValue::from_str(&disposition).unwrap(),
            );

            // TODO scrivere log carino
            println!("downloading file: {}", &path.display());
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
            let ascii = code.render::<unicode::Dense1x2>().quiet_zone(true).build();
            println!("{}", ascii);
        }
    }
}
