//! The client trust path a user meets on a host with no certificate store
//! (#293). The real binary runs against a local TLS server whose CA is only
//! reachable through the knob under test, so every assertion is about what a
//! user or the process observes — not about the builder's internals.
//!
//! Each test generates its own CA and leaf. Generated, not committed, because
//! Apple's verifier rejects a leaf valid for more than 825 days (`-67901`,
//! "not standards compliant"), and a committed short-lived pair would be a
//! time bomb.

use crate::common::*;
use rcgen::{BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose};
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Output;
use std::sync::Arc;
use time::{Duration, OffsetDateTime};

/// A loopback TLS endpoint and the CA that signs it.
struct Tls {
    port: u16,
    ca_pem: String,
    ca_der: Vec<u8>,
}

/// A one-request-at-a-time HTTPS endpoint on loopback, good enough for the
/// binary's catalog fetch. The thread is a daemon: the test process exits
/// with it, and each test gets its own port.
fn serve(name: &str) -> Tls {
    construct_core::llm::ensure_crypto();
    let now = OffsetDateTime::now_utc();

    let ca_key = KeyPair::generate().expect("generate a CA key");
    let mut ca_params = CertificateParams::new(Vec::new()).expect("CA params");
    ca_params.distinguished_name.push(DnType::CommonName, format!("construct test CA {name}"));
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    ca_params.not_before = now - Duration::days(1);
    ca_params.not_after = now + Duration::days(3650);
    let ca = ca_params.self_signed(&ca_key).expect("self-signed CA");

    let leaf_key = KeyPair::generate().expect("generate a leaf key");
    let mut leaf_params =
        CertificateParams::new(vec!["127.0.0.1".to_string(), "localhost".to_string()]).expect("leaf params");
    leaf_params.distinguished_name.push(DnType::CommonName, format!("construct test leaf {name}"));
    leaf_params.use_authority_key_identifier_extension = true;
    leaf_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    // Apple's platform verifier rejects a leaf valid for more than 825 days.
    leaf_params.not_before = now - Duration::days(1);
    leaf_params.not_after = now + Duration::days(365);
    let leaf = leaf_params.signed_by(&leaf_key, &ca, &ca_key).expect("signed leaf");

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![leaf.der().clone()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
                leaf_key.serialize_der(),
            )),
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
    Tls { port, ca_pem: ca.pem(), ca_der: ca.der().to_vec() }
}

/// Write a CA into the test's scratch dir for `SSL_CERT_FILE`/`CONSTRUCT_CA_FILE`.
fn ca_file(env: &Env, name: &str, pem: &str) -> String {
    let path = env.dir.join(format!("{name}.pem"));
    std::fs::write(&path, pem).expect("write the test CA");
    path.display().to_string()
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
    let env = Env::new("tls-ssl-cert-file", json!({ "steps": [] }));
    let tls = serve("1");
    let ca = ca_file(&env, "ca1", &tls.ca_pem);
    let out = list(&env, tls.port, &[("SSL_CERT_FILE", &ca)]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("test/model"));
}

/// Native roots and `CONSTRUCT_CA_FILE` are merged: the managed host's CA
/// keeps working and the private one is added beside it. Linux only, because
/// `SSL_CERT_FILE` is how this test supplies the "native" root.
#[cfg(target_os = "linux")]
#[test]
fn native_and_extra_roots_are_both_trusted() {
    let env = Env::new("tls-merge", json!({ "steps": [] }));
    let one = serve("1");
    let two = serve("2");
    let ca1 = ca_file(&env, "ca1", &one.ca_pem);
    let ca2 = ca_file(&env, "ca2", &two.ca_pem);

    // CA2 alone is not in the platform bundle, so the extra is what makes it work.
    let out = list(&env, two.port, &[("SSL_CERT_FILE", &ca1)]);
    assert!(!out.status.success(), "unconfigured private CA must not validate");

    let out = list(&env, one.port, &[("SSL_CERT_FILE", &ca1), ("CONSTRUCT_CA_FILE", &ca2)]);
    assert!(out.status.success(), "native root kept: {}", stderr(&out));
    let out = list(&env, two.port, &[("SSL_CERT_FILE", &ca1), ("CONSTRUCT_CA_FILE", &ca2)]);
    assert!(out.status.success(), "extra root added: {}", stderr(&out));
}

/// The scratch case: no platform store at all, one private CA in the knob.
#[test]
fn a_scratch_host_trusts_construct_ca_file() {
    let env = Env::new("tls-extra-only", json!({ "steps": [] }));
    let tls = serve("2");
    let ca = ca_file(&env, "ca2", &tls.ca_pem);
    let out = list(
        &env,
        tls.port,
        &[
            ("SSL_CERT_FILE", "/nonexistent/construct/ca-certificates.crt"),
            ("SSL_CERT_DIR", "/nonexistent/construct/certs"),
            ("CONSTRUCT_CA_FILE", &ca),
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("test/model"));
}

/// A single DER certificate is a bundle too; `ca_certs` supports both forms.
#[test]
fn construct_ca_file_accepts_a_der_certificate() {
    let env = Env::new("tls-der", json!({ "steps": [] }));
    let tls = serve("2");
    let path = env.dir.join("ca2.der");
    std::fs::write(&path, &tls.ca_der).expect("write the DER test CA");
    let ca = path.display().to_string();
    let out = list(
        &env,
        tls.port,
        &[
            ("SSL_CERT_FILE", "/nonexistent/construct/ca-certificates.crt"),
            ("SSL_CERT_DIR", "/nonexistent/construct/certs"),
            ("CONSTRUCT_CA_FILE", &ca),
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
