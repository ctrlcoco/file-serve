use base64::{engine::general_purpose::STANDARD, Engine};
use qrcode::{render::svg, render::unicode, QrCode};
use std::env;
pub fn get_qr_code(text: &str) -> String {
    let code = QrCode::new(text).unwrap();

    match terminal_supports_images() {
        Some("iterm2") => {
            // Render SVG and encode for iTerm2 inline image protocol
            let svg = code.render::<svg::Color>().min_dimensions(200, 200).build();

            let encoded = STANDARD.encode(svg.as_bytes());

            format!(
                "\x1b]1337;File=inline=1;width=auto;height=auto;preserveAspectRatio=1:{}\x07",
                encoded
            )
        }
        Some("kitty") => {
            // Kitty uses its own graphics protocol
            let svg = code.render::<svg::Color>().min_dimensions(200, 200).build();
            let encoded = STANDARD.encode(svg.as_bytes());

            format!("\x1b_Gf=100,t=d,A=T,width=200,height=200;{}\x1b\\", encoded)
        }
        _ => {
            // Fallback to ASCII QR for others terminal
            let ascii_qr = code.render::<unicode::Dense1x2>().quiet_zone(true).build();
            format!("{}", ascii_qr)
        }
    }
}

pub fn html_escape(s: &str) -> String {
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

pub fn bytes_to_human_size(bytes: u64) -> String {
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

pub fn start_logging(output_path: &str) {
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
