use std::{
    collections::BTreeMap,
    fmt::Write as FmtWrite,
    io::Write,
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    extract::State,
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::get,
};
use canterlot_ctl::{
    cli::Version,
    error::CtlError,
    release::{ReleaseSource, fetch, resolve, verify},
};
use flate2::{Compression, write::GzEncoder};
use serde_json::json;
use sha2::{Digest, Sha256};

#[derive(Default)]
struct ServerState {
    latest: Mutex<String>,
    bodies: Mutex<BTreeMap<String, Vec<u8>>>,
    hits: Mutex<Vec<String>>,
}

async fn handle(State(state): State<Arc<ServerState>>, uri: Uri) -> Response {
    let path = uri.path().to_string();
    if path.ends_with("/releases/latest") {
        let tag = state.latest.lock().unwrap().clone();
        return Json(json!({ "tag_name": tag })).into_response();
    }
    state.hits.lock().unwrap().push(path.clone());
    let name = path.rsplit('/').next().unwrap_or("").to_string();
    let body = state.bodies.lock().unwrap().get(&name).cloned();
    match body {
        Some(bytes) => {
            ([(header::CONTENT_TYPE, "application/octet-stream")], bytes).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn spawn_server() -> (ReleaseSource, Arc<ServerState>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let state = Arc::new(ServerState::default());
    let app = Router::new()
        .route("/", get(handle))
        .fallback(get(handle))
        .with_state(state.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let source = ReleaseSource {
        api_base: format!("http://{addr}/repos/").parse().unwrap(),
        download_base: format!("http://{addr}/").parse().unwrap(),
        repo: "Fenicu/canterlot".into(),
    };
    (source, state, task)
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn sums(lines: &[(&str, &[u8])]) -> Vec<u8> {
    let mut text = String::new();
    for (name, bytes) in lines {
        let _ = writeln!(text, "{}  {name}", hex(&Sha256::digest(bytes)));
    }
    text.into_bytes()
}

/// Собирает tar вручную: так можно записать пути, которые сама библиотека tar не создаёт.
fn tar_gz(name: &str, kind: u8, link: &str, data: &[u8]) -> Vec<u8> {
    let mut header = [0u8; 512];
    header[..name.len()].copy_from_slice(name.as_bytes());
    header[100..108].copy_from_slice(b"0000644\0");
    header[108..116].copy_from_slice(b"0000000\0");
    header[116..124].copy_from_slice(b"0000000\0");
    header[124..136].copy_from_slice(format!("{:011o}\0", data.len()).as_bytes());
    header[136..148].copy_from_slice(b"00000000000\0");
    header[148..156].fill(b' ');
    header[156] = kind;
    header[157..157 + link.len()].copy_from_slice(link.as_bytes());
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    let checksum: u32 = header.iter().map(|&b| u32::from(b)).sum();
    header[148..156].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());
    let mut raw = header.to_vec();
    raw.extend_from_slice(data);
    let padding = (512 - data.len() % 512) % 512;
    raw.extend(std::iter::repeat_n(0u8, padding));
    raw.extend_from_slice(&[0u8; 1024]);
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&raw).unwrap();
    encoder.finish().unwrap()
}

#[test]
fn verify_requires_exact_entry() {
    let dir = tempfile::tempdir().unwrap();
    let body = b"asset body";
    let other = b"other line";
    let mut checksums = sums(&[("asset.txt", body)]);
    checksums.extend(sums(&[("ignored.txt", other)]));
    std::fs::write(dir.path().join("SHA256SUMS"), &checksums).unwrap();
    std::fs::write(dir.path().join("asset.txt"), body).unwrap();

    verify(dir.path(), &dir.path().join("SHA256SUMS"), &["asset.txt"]).unwrap();

    match verify(dir.path(), &dir.path().join("SHA256SUMS"), &["absent.txt"]) {
        Err(CtlError::ChecksumMissing(name)) => assert_eq!(name, "absent.txt"),
        other => panic!("expected checksum_missing, got {other:?}"),
    }

    std::fs::write(dir.path().join("asset.txt"), b"tampered").unwrap();
    match verify(dir.path(), &dir.path().join("SHA256SUMS"), &["asset.txt"]) {
        Err(CtlError::ChecksumMismatch(name)) => assert_eq!(name, "asset.txt"),
        other => panic!("expected checksum_mismatch, got {other:?}"),
    }
}

#[test]
fn extract_rejects_parent_and_absolute_and_escaping_links() {
    let cases: [(&str, Vec<u8>); 3] = [
        ("absolute", tar_gz("/srv/evil.txt", b'0', "", b"bad")),
        ("parent", tar_gz("../evil.txt", b'0', "", b"bad")),
        ("symlink", tar_gz("dir/link", b'2', "../../outside", b"")),
    ];
    for (name, archive) in cases {
        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join("archive.tar.gz");
        let dest = dir.path().join("dest");
        std::fs::write(&archive_path, &archive).unwrap();
        std::fs::create_dir_all(&dest).unwrap();
        match canterlot_ctl::release::extract_tar_gz(&archive_path, &dest) {
            Err(CtlError::UnsafeArchive(reason)) => assert!(!reason.is_empty(), "{name}"),
            other => panic!("{name}: expected unsafe_archive_entry, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn fetch_resolves_latest_and_downloads_one_tag() {
    let (source, state, task) = spawn_server().await;
    *state.latest.lock().unwrap() = "v1.2.3".into();
    let body = b"utility tarball";
    state
        .bodies
        .lock()
        .unwrap()
        .insert("asset.txt".into(), body.to_vec());
    state
        .bodies
        .lock()
        .unwrap()
        .insert("SHA256SUMS".into(), sums(&[("asset.txt", body)]));

    let wanted = resolve(&source, None).await.unwrap();
    assert_eq!(wanted, Version::parse("1.2.3").unwrap());
    // Между разрешением и загрузкой «последний» релиз поменялся.
    *state.latest.lock().unwrap() = "v9.9.9".into();

    let dir = tempfile::tempdir().unwrap();
    let staged = fetch(&source, &wanted, &["asset.txt", "SHA256SUMS"], dir.path())
        .await
        .unwrap();
    assert_eq!(staged.version, wanted);
    assert_eq!(
        std::fs::read(&staged.files["asset.txt"]).unwrap(),
        body.to_vec()
    );
    let hits = state.hits.lock().unwrap().clone();
    assert!(!hits.is_empty());
    for hit in hits {
        assert!(hit.contains("/v1.2.3/"), "mixed tags: {hit}");
        assert!(!hit.contains("/v9.9.9/"), "mixed tags: {hit}");
    }
    task.abort();
}
