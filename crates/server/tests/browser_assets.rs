#[path = "../examples/browser/assets.rs"]
mod assets;

use axum::{body::Body, http::Request};
use http_body_util::BodyExt;
use std::io;
use tower::ServiceExt;

#[tokio::test]
async fn localhost_browser_assets_serve_the_built_bundle_and_refuse_other_paths(
) -> Result<(), io::Error> {
    let root = tempfile::tempdir()?;
    std::fs::create_dir(root.path().join("dist"))?;
    std::fs::write(root.path().join("index.html"), "<main>Browser calls</main>")?;
    std::fs::write(root.path().join("style.css"), "body { color: black; }")?;
    assert!(assets::router(root.path()).is_err());
    std::fs::write(
        root.path().join("dist/app.js"),
        "export const loaded = true;",
    )?;
    let router = assets::router(root.path())?;
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/app.js")
                .body(Body::empty())
                .map_err(io::Error::other)?,
        )
        .await
        .map_err(io::Error::other)?;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()["Content-Type"],
        "text/javascript; charset=utf-8"
    );
    assert_eq!(
        response
            .into_body()
            .collect()
            .await
            .map_err(io::Error::other)?
            .to_bytes(),
        "export const loaded = true;"
    );
    for path in [
        "/package.json",
        "/../Cargo.toml",
        "/dist/app.js",
        "/unknown",
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .body(Body::empty())
                    .map_err(io::Error::other)?,
            )
            .await
            .map_err(io::Error::other)?;
        assert_eq!(response.status(), 404, "{path}");
    }
    std::fs::write(root.path().join("dist/app.js"), vec![b'x'; 1024 * 1024 + 1])?;
    assert!(assets::router(root.path()).is_err());
    root.close()
}
