use axum::{
    http::{header, StatusCode},
    routing::get,
    Router,
};
use std::{
    io::{self, Read},
    path::Path,
};

fn read(root: &Path, relative: &str) -> Result<String, io::Error> {
    let path = root.join(relative);
    let metadata = std::fs::symlink_metadata(&path)?;
    if !metadata.file_type().is_file() || metadata.len() > 1024 * 1024 {
        return Err(io::Error::other(format!(
            "browser asset must be a regular file no larger than one mebibyte: {}",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&path)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() > 1024 * 1024 {
        return Err(io::Error::other(format!(
            "browser asset is empty or exceeds capacity: {}",
            path.display()
        )));
    }
    String::from_utf8(bytes).map_err(io::Error::other)
}

pub fn router(root: &Path) -> Result<Router, io::Error> {
    let index = read(root, "index.html")?;
    let styles = read(root, "style.css")?;
    let bundle = read(root, "dist/app.js")?;
    Ok(Router::new()
        .route("/favicon.ico", get(|| async { StatusCode::NO_CONTENT }))
        .route(
            "/",
            get(move || {
                let body = index.clone();
                async move {
                    (
                        [
                            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                        ],
                        body,
                    )
                }
            }),
        )
        .route(
            "/style.css",
            get(move || {
                let body = styles.clone();
                async move {
                    (
                        [
                            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
                            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                        ],
                        body,
                    )
                }
            }),
        )
        .route(
            "/app.js",
            get(move || {
                let body = bundle.clone();
                async move {
                    (
                        [
                            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
                            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                        ],
                        body,
                    )
                }
            }),
        ))
}
