use rusterisk_client::Error;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

const PINNED: &str = include_str!("../../../api/tests/fixtures/asterisk-22.9.0-routes.tsv");

#[derive(Deserialize)]
struct Index {
    apis: Vec<Resource>,
}

#[derive(Deserialize)]
struct Resource {
    path: String,
}

#[derive(Deserialize)]
struct Document {
    apis: Vec<Path>,
    models: Models,
}

#[derive(Deserialize)]
struct Models {
    #[serde(rename = "Event")]
    event: Option<EventModel>,
}

#[derive(Deserialize)]
struct EventModel {
    #[serde(rename = "subTypes")]
    types: BTreeSet<String>,
}

#[derive(Deserialize)]
struct Path {
    path: String,
    operations: Vec<Operation>,
}

#[derive(Deserialize)]
struct Operation {
    #[serde(rename = "httpMethod")]
    method: String,
    nickname: String,
    #[serde(rename = "responseClass")]
    response: String,
    #[serde(default)]
    parameters: Vec<Parameter>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Parameter {
    name: String,
    param_type: String,
    data_type: String,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    allow_multiple: bool,
}

async fn compare_options(
    http: &reqwest::Client,
    native: &str,
    reference: &str,
    routes: &BTreeMap<String, BTreeSet<String>>,
) -> Result<(), Error> {
    for (path, methods) in routes {
        let allow = ["OPTIONS", "GET", "POST", "PUT", "DELETE"]
            .into_iter()
            .filter(|method| {
                *method == "OPTIONS" || (path != "/events" && methods.contains(*method))
            })
            .collect::<Vec<_>>()
            .join(",");
        for base in [reference, native] {
            let response = http
                .request(reqwest::Method::OPTIONS, format!("{base}/ari{path}"))
                .basic_auth("app", Some("test-password"))
                .send()
                .await?;
            if base == reference && path.split('/').nth(1) == Some("mailboxes") {
                assert_eq!(response.status(), 404, "unloaded mailbox module: {path}");
                assert!(response.headers().get(reqwest::header::ALLOW).is_none());
            } else {
                assert_eq!(response.status(), 204, "OPTIONS {base}/ari{path}");
                assert_eq!(
                    response
                        .headers()
                        .get(reqwest::header::ALLOW)
                        .and_then(|value| value.to_str().ok()),
                    Some(allow.as_str()),
                    "OPTIONS {base}/ari{path}"
                );
            }
        }
    }
    Ok(())
}

pub(super) async fn compare(native: &str, reference: &str) -> Result<(), Error> {
    let http = reqwest::Client::builder()
        .timeout(rusterisk_server::test_time::Wait::Ready.duration())
        .build()?;
    let index: Index = http
        .get(format!("{reference}/ari/api-docs/resources.json"))
        .basic_auth("app", Some("test-password"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let mut rows = BTreeSet::new();
    let mut parameters = BTreeSet::new();
    let mut routes: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for resource in index.apis {
        let name = resource
            .path
            .strip_prefix("/api-docs/")
            .and_then(|name| name.strip_suffix(".{format}"))
            .ok_or(Error::Invalid("reference resource path"))?;
        let document: Document = http
            .get(format!("{reference}/ari/api-docs/{name}.json"))
            .basic_auth("app", Some("test-password"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if name == "events" {
            let expected: BTreeSet<_> =
                include_str!("../../../api/tests/fixtures/asterisk-22.9.0-events.txt")
                    .lines()
                    .map(str::to_owned)
                    .collect();
            let model = document
                .models
                .event
                .ok_or(Error::Invalid("reference event model"))?;
            assert_eq!(model.types, expected, "published Asterisk event types");
        }
        for path in document.apis {
            let concrete = path
                .path
                .split('/')
                .map(|part| {
                    if part.starts_with('{') {
                        "route-probe"
                    } else {
                        part
                    }
                })
                .collect::<Vec<_>>()
                .join("/");
            let methods = routes.entry(concrete).or_default();
            for op in path.operations {
                let row = format!(
                    "{}\t{}\t{}\t{}",
                    op.method, path.path, op.nickname, op.response
                );
                assert!(rows.insert(row), "duplicate reference operation");
                for parameter in op.parameters {
                    let row = format!(
                        "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                        op.method,
                        path.path,
                        parameter.name,
                        parameter.param_type,
                        parameter.data_type,
                        parameter.required,
                        parameter.allow_multiple
                    );
                    assert!(parameters.insert(row), "duplicate reference parameter");
                }
                methods.insert(op.method);
            }
        }
    }
    let pinned: BTreeSet<_> = PINNED.lines().map(str::to_owned).collect();
    assert_eq!(rows, pinned, "published Asterisk REST operations");
    let expected: BTreeSet<_> =
        include_str!("../../../api/tests/fixtures/asterisk-22.9.0-parameters.tsv")
            .lines()
            .map(str::to_owned)
            .collect();
    assert_eq!(parameters, expected, "published Asterisk REST parameters");
    compare_options(&http, native, reference, &routes).await?;

    for (method, path) in [
        (reqwest::Method::DELETE, "/channels/externalMedia"),
        (reqwest::Method::DELETE, "/channels/%63reate"),
        (reqwest::Method::GET, "/%61sterisk/ping"),
        (reqwest::Method::GET, "/channels/"),
        (reqwest::Method::POST, "/channels/create/answer"),
        (reqwest::Method::OPTIONS, "/undefined-resource"),
    ] {
        let mut observed = Vec::new();
        for base in [reference, native] {
            let response = http
                .request(method.clone(), format!("{base}/ari{path}"))
                .basic_auth("app", Some("test-password"))
                .send()
                .await?;
            let status = response.status();
            let allow = response
                .headers()
                .get(reqwest::header::ALLOW)
                .map(|value| value.as_bytes().to_vec());
            let body = if status.is_client_error() {
                Some(
                    response
                        .json::<rusterisk_client::api::ApiError>()
                        .await?
                        .message,
                )
            } else {
                None
            };
            observed.push((status, allow, body));
        }
        assert_eq!(observed[0], observed[1], "{method} {path}");
    }
    Ok(())
}
