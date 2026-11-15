#[path = "../examples/browser/assets.rs"]
mod assets;

use axum::{body::Body, http::Request};
use http_body_util::BodyExt;
use std::io;
use std::path::Path;
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

#[tokio::test]
async fn public_browser_bundle_preserves_the_complete_publisher_license() -> Result<(), io::Error> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/browser");
    let publisher = std::fs::read_to_string(root.join("node_modules/sip.js/LICENSE.md"))?;
    let response = assets::router(&root)?
        .oneshot(
            Request::builder()
                .uri("/app.js")
                .body(Body::empty())
                .map_err(io::Error::other)?,
        )
        .await
        .map_err(io::Error::other)?;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["X-Content-Type-Options"], "nosniff");
    let bytes = response
        .into_body()
        .collect()
        .await
        .map_err(io::Error::other)?
        .to_bytes();
    let body = std::str::from_utf8(&bytes).map_err(io::Error::other)?;
    let normalized = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(normalized(body).contains(&normalized(&publisher)));
    Ok(())
}
