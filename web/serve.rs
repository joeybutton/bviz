//! Serves a directory over HTTP on the loopback interface, for opening the
//! example page: a browser loads an ES module and its `.wasm` only over HTTP,
//! with `application/wasm` for the latter, never from a `file://` URL.
//!
//! Usage: `serve <dir> [port]`.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("json") => "application/json",
        Some("css") => "text/css; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// The file under `root` a request path names, `None` for one that climbs out
/// of it.
fn resolve(root: &Path, target: &str) -> Option<PathBuf> {
    let path = target.split(['?', '#']).next().unwrap_or("/");
    let mut file = root.to_path_buf();
    for component in Path::new(path.trim_start_matches('/')).components() {
        match component {
            Component::Normal(part) => file.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    if file.is_dir() {
        file.push("index.html");
    }
    Some(file)
}

fn respond(root: &Path, stream: TcpStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream);
    let mut request = String::new();
    reader.read_line(&mut request)?;
    // Drain the headers; nothing here reads them.
    let mut line = String::new();
    while reader.read_line(&mut line)? > 2 {
        line.clear();
    }
    let mut stream = reader.into_inner();
    let mut parts = request.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/"));
    let found = (method == "GET" || method == "HEAD")
        .then(|| resolve(root, target))
        .flatten()
        .and_then(|file| std::fs::read(&file).ok().map(|body| (file, body)));
    match found {
        Some((file, body)) => {
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
                content_type(&file),
                body.len()
            )?;
            if method == "GET" {
                stream.write_all(&body)?;
            }
        }
        None => stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")?,
    }
    eprintln!("{method} {target}");
    Ok(())
}

fn main() {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().expect("usage: serve <dir> [port]"));
    let port = args.next().unwrap_or_else(|| "8080".into());
    let root = root.canonicalize().expect("the directory to serve");
    let listener = TcpListener::bind(("127.0.0.1", port.parse::<u16>().expect("a port")))
        .expect("bind the port");
    println!("Serving {} at http://127.0.0.1:{port}/", root.display());
    for stream in listener.incoming().flatten() {
        let root = root.clone();
        std::thread::spawn(move || {
            if let Err(e) = respond(&root, stream) {
                eprintln!("{e}");
            }
        });
    }
}
