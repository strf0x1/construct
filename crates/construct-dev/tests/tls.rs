//! The client trust path a user meets on a host with no certificate store
//! (#293). The real binary runs against a local TLS server whose CA is only
//! reachable through the knob under test, so every assertion is about what a
//! user or the process observes — not about the builder's internals.
//!
//! Fixtures under `fixtures/tls/` are two self-signed CAs and a leaf each.
//! They expire in 100 years; regenerate with the commands in the PR if a
//! `NotAfter` ever shows up in a failure.

use crate::common::*;
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Output;
use std::sync::Arc;

/// CA1 is supplied through `SSL_CERT_FILE`, which only Linux's
/// `rustls-native-certs` reads, so its server is Linux-only.
#[cfg(target_os = "linux")]
const LEAF1: &[u8] = include_bytes!("fixtures/tls/leaf1.der");
#[cfg(target_os = "linux")]
const LEAF1_KEY: &[u8] = include_bytes!("fixtures/tls/leaf1-key.der");
const LEAF2: &[u8] = include_bytes!("fixtures/tls/leaf2.der");
const LEAF2_KEY: &[u8] = include_bytes!("fixtures/tls/leaf2-key.der");

fn fixture(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tls").join(name).display().to_string()
}

/// A one-request-at-a-time HTTPS endpoint on loopback, good enough for the
/// binary's catalog fetch. The thread is a daemon: the test process exits
/// with it, and each test gets its own port.
fn serve(cert: &'static [u8], key: &'static [u8]) -> u16 {
    construct_core::llm::ensure_crypto();
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![rustls::pki_types::CertificateDer::from(cert)],
            rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(key)),
        )
        .expect("test certificate and key agree");
    let config = Arc::new(config);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback TLS server");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for tcp in listener.incoming().flatten() {
            let mut tcp = tcp;
            let Ok(mut conn) = rustls::ServerConnection::new(config.clone()) else { continue };
            let mut tls = rustls::Stream::new(&mut conn, &mut tcp);
            let mut request = [0u8; 4096];
            let _ = tls.read(&mut request);
            let body = r#"{"data":[{"id":"test/model"}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = tls.write_all(response.as_bytes());
            let _ = tls.flush();
        }
    });
    port
}

/// `models --refresh` against `port`; `extra` overrides the ambient knobs.
fn list(env: &Env, port: u16, extra: &[(&str, &str)]) -> Output {
    let url = format!("https://127.0.0.1:{port}/v1");
    let mut vars = vec![("CONSTRUCT_BASE_URL", url.as_str()), ("NO_PROXY", "127.0.0.1,localhost")];
    vars.extend_from_slice(extra);
    env.run_with(&vars, &["models", "--refresh"])
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The opaque `builder error` is what a rescuer could not act on. Linux only:
/// `rustls-native-certs` honors `SSL_CERT_FILE` there, and musl builds have
/// baked roots, so their store is never empty.
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
#[test]
fn an_empty_store_says_how_to_fix_it() {
    let env = Env::new("tls-no-roots", json!({ "steps": [] }));
    let out = list(
        &env,
        1,
        &[
            ("SSL_CERT_FILE", "/nonexistent/construct/ca-certificates.crt"),
            ("SSL_CERT_DIR", "/nonexistent/construct/certs"),
        ],
    );
    assert!(!out.status.success(), "a client with no roots must not start");
    let err = stderr(&out);
    assert!(err.contains("no CA certificates found"), "{err}");
    assert!(err.contains("SSL_CERT_FILE") && err.contains("CONSTRUCT_CA_FILE"), "{err}");
    assert!(!err.contains("builder error"), "the opaque error is replaced: {err}");
}

/// `SSL_CERT_FILE` is the platform store: no construct-specific knob needed.
/// Linux only; Apple's store ignores the variable.
#[cfg(target_os = "linux")]
#[test]
fn ssl_cert_file_is_the_platform_store() {
    let port = serve(LEAF1, LEAF1_KEY);
    let env = Env::new("tls-ssl-cert-file", json!({ "steps": [] }));
    let out = list(&env, port, &[("SSL_CERT_FILE", &fixture("ca1.crt"))]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("test/model"));
}

/// Native roots and `CONSTRUCT_CA_FILE` are merged: the managed host's CA
/// keeps working and the private one is added beside it. Linux only, because
/// `SSL_CERT_FILE` is how this test supplies the "native" root.
#[cfg(target_os = "linux")]
#[test]
fn native_and_extra_roots_are_both_trusted() {
    let port1 = serve(LEAF1, LEAF1_KEY);
    let port2 = serve(LEAF2, LEAF2_KEY);
    let env = Env::new("tls-merge", json!({ "steps": [] }));
    let ca1 = fixture("ca1.crt");
    let ca2 = fixture("ca2.crt");

    // CA2 alone is not in the platform bundle, so the extra is what makes it work.
    let out = list(&env, port2, &[("SSL_CERT_FILE", &ca1)]);
    assert!(!out.status.success(), "unconfigured private CA must not validate");

    let out = list(&env, port1, &[("SSL_CERT_FILE", &ca1), ("CONSTRUCT_CA_FILE", &ca2)]);
    assert!(out.status.success(), "native root kept: {}", stderr(&out));
    let out = list(&env, port2, &[("SSL_CERT_FILE", &ca1), ("CONSTRUCT_CA_FILE", &ca2)]);
    assert!(out.status.success(), "extra root added: {}", stderr(&out));
}

/// The scratch case: no platform store at all, one private CA in the knob.
#[test]
fn a_scratch_host_trusts_construct_ca_file() {
    let port = serve(LEAF2, LEAF2_KEY);
    let env = Env::new("tls-extra-only", json!({ "steps": [] }));
    let out = list(
        &env,
        port,
        &[
            ("SSL_CERT_FILE", "/nonexistent/construct/ca-certificates.crt"),
            ("SSL_CERT_DIR", "/nonexistent/construct/certs"),
            ("CONSTRUCT_CA_FILE", &fixture("ca2.crt")),
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("test/model"));
}

/// A single DER certificate is a bundle too; `ca_certs` supports both forms.
#[test]
fn construct_ca_file_accepts_a_der_certificate() {
    let port = serve(LEAF2, LEAF2_KEY);
    let env = Env::new("tls-der", json!({ "steps": [] }));
    let out = list(
        &env,
        port,
        &[
            ("SSL_CERT_FILE", "/nonexistent/construct/ca-certificates.crt"),
            ("SSL_CERT_DIR", "/nonexistent/construct/certs"),
            ("CONSTRUCT_CA_FILE", &fixture("ca2.der")),
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
}

/// A mistyped path names the variable and the path, not a TLS handshake.
#[test]
fn a_bad_construct_ca_file_is_named() {
    let env = Env::new("tls-bad-path", json!({ "steps": [] }));
    let out = list(&env, 1, &[("CONSTRUCT_CA_FILE", "/nonexistent/construct/ca.pem")]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("CONSTRUCT_CA_FILE=/nonexistent/construct/ca.pem"), "{err}");
    assert!(err.contains("could not read the file"), "{err}");
}
