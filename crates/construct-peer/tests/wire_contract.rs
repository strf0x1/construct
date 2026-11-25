//! Contract tests for `construct-peer` (#368).
//!
//! Pins the wire guarantees required by #368:
//! - Noise IK against the wrong static key fails;
//! - Replayed message 1 cannot reach the authenticated state without 1-RTT `Hello`;
//! - A tampered encrypted transport frame fails AEAD;
//! - Envelopes larger than 64 KiB round-trip cleanly on every channel (`acp`,
//!   `control`, `llm`);
//! - Oversized unauthenticated (pre-pin) envelopes are rejected;
//! - Keep-alive answers `Ping`/`Pong` and times out a silent peer;
//! - Dropped links reconnect with jittered exponential backoff;
//! - Both `ws://` and `wss://` (with ephemeral CA + rustls) transports work on loopback.

use construct_peer::{
    Backoff, Channel, Control, Envelope, FrameStream, Hello, KeepAlive, KeepAliveState, KeyDecision, Keypair,
    LengthPrefixed, PINNED_MAX_ENVELOPE, PRE_PIN_MAX_ENVELOPE, PeerLink, WireError, accept_ws, connect_with_backoff,
    connect_ws, connect_ws_with_tls, handshake_initiator, handshake_responder, recv_with_keepalive,
};
use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair as RcKeyPair, SanType};
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use time::{Duration as TimeDuration, OffsetDateTime};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

fn mem_pair() -> (LengthPrefixed<DuplexStream>, LengthPrefixed<DuplexStream>) {
    let (a, b) = tokio::io::duplex(256 * 1024);
    (LengthPrefixed::new(a), LengthPrefixed::new(b))
}

async fn establish_pair(
    max_responder_envelope: usize,
) -> (PeerLink<LengthPrefixed<DuplexStream>>, PeerLink<LengthPrefixed<DuplexStream>>, Hello) {
    let hub = Keypair::generate().unwrap();
    let peer = Keypair::generate().unwrap();
    let (a, b) = mem_pair();
    let hello = Hello::new("old-pc").with_join_token("invite-abc");
    let hub_pub = hub.public();

    let init = tokio::spawn(async move { handshake_initiator(a, &peer, hub_pub, &hello, PINNED_MAX_ENVELOPE).await });
    let resp = handshake_responder(b, &hub, |_| KeyDecision::Accept { max_envelope_bytes: max_responder_envelope })
        .await
        .unwrap();
    let client = init.await.unwrap().unwrap();
    (client, resp.0, resp.1)
}

#[tokio::test]
async fn handshake_against_wrong_static_key_fails_and_silently_drops() {
    let hub = Keypair::generate().unwrap();
    let wrong_hub = Keypair::generate().unwrap();
    let peer = Keypair::generate().unwrap();
    let (a, b) = mem_pair();
    let hello = Hello::new("old-pc");

    let wrong_pub = wrong_hub.public();
    let init = tokio::spawn(async move { handshake_initiator(a, &peer, wrong_pub, &hello, PINNED_MAX_ENVELOPE).await });

    let err = match handshake_responder(b, &hub, |_| KeyDecision::Accept { max_envelope_bytes: PRE_PIN_MAX_ENVELOPE })
        .await
    {
        Ok(_) => panic!("expected handshake to fail with wrong static key"),
        Err(e) => e,
    };
    assert!(matches!(err, WireError::Crypto(_)), "{err}");
    assert!(err.is_silent_drop());

    let client_err = match init.await.unwrap() {
        Ok(_) => panic!("expected initiator to fail when responder drops"),
        Err(e) => e,
    };
    assert!(matches!(client_err, WireError::Closed | WireError::Io(_) | WireError::Crypto(_)));
}

#[tokio::test]
async fn replayed_msg1_cannot_reach_authenticated_state_without_1rtt_hello() {
    let hub = Keypair::generate().unwrap();
    let peer = Keypair::generate().unwrap();

    // Capture a real Noise IK msg 1 from an honest initiator.
    let (capture_a, mut capture_b) = mem_pair();
    let hub_pub = hub.public();
    let peer_clone = peer.clone();
    let capture_task = tokio::spawn(async move {
        let _ = handshake_initiator(capture_a, &peer_clone, hub_pub, &Hello::new("old-pc"), PINNED_MAX_ENVELOPE).await;
    });
    let captured_msg1 = capture_b.recv_frame().await.unwrap();
    drop(capture_b);
    let _ = capture_task.await;

    // Replay captured msg 1 on a fresh connection, then send bogus transport bytes
    // (attacker does not hold the ephemeral private key for `e`).
    let (mut attacker, responder_stream) = mem_pair();
    let replay_task = tokio::spawn(async move {
        attacker.send_frame(&captured_msg1).await.unwrap();
        // Responder replies with fresh msg 2.
        let _msg2 = attacker.recv_frame().await.unwrap();
        // Attacker tries to forge/replay a transport frame without knowing `e`.
        attacker.send_frame(&[0x42u8; 64]).await.unwrap();
    });

    let err = match handshake_responder(responder_stream, &hub, |pk| {
        assert_eq!(pk, peer.public());
        KeyDecision::Accept { max_envelope_bytes: PRE_PIN_MAX_ENVELOPE }
    })
    .await
    {
        Ok(_) => panic!("replayed msg 1 must not authenticate"),
        Err(e) => e,
    };
    assert!(matches!(err, WireError::Crypto(_)), "{err}");
    replay_task.await.unwrap();
}

#[tokio::test]
async fn responder_exposes_matching_handshake_hash_and_hello() {
    let (client, server, hello) = establish_pair(PINNED_MAX_ENVELOPE).await;
    assert_eq!(hello.peer_id, "old-pc");
    assert_eq!(hello.join_token.as_deref(), Some("invite-abc"));
    assert_eq!(client.handshake_hash(), server.handshake_hash());
    assert_eq!(client.handshake_hash().sas_code(), server.handshake_hash().sas_code());
}

#[tokio::test]
async fn responder_rejection_stops_handshake_before_msg2() {
    let hub = Keypair::generate().unwrap();
    let peer = Keypair::generate().unwrap();
    let (a, b) = mem_pair();
    let hub_pub = hub.public();

    let init = tokio::spawn(async move {
        handshake_initiator(a, &peer, hub_pub, &Hello::new("revoked-pc"), PINNED_MAX_ENVELOPE).await
    });

    let err = match handshake_responder(b, &hub, |_| KeyDecision::Reject("revoked".into())).await {
        Ok(_) => panic!("expected rejection"),
        Err(e) => e,
    };
    assert!(matches!(&err, WireError::Rejected(r) if r == "revoked"), "{err}");
    assert!(init.await.unwrap().is_err());
}

#[tokio::test]
async fn tampered_transport_frame_fails_aead() {
    let hub = Keypair::generate().unwrap();
    let peer = Keypair::generate().unwrap();
    let (client_io, mut mitm_client) = tokio::io::duplex(64 * 1024);
    let (mut mitm_server, server_io) = tokio::io::duplex(64 * 1024);

    // MitM forwards handshake frames (msg 1, msg 2, Hello) verbatim, then flips
    // one bit in the next client->server transport frame.
    let mitm = tokio::spawn(async move {
        async fn forward_one(src: &mut DuplexStream, dst: &mut DuplexStream, corrupt: bool) -> std::io::Result<()> {
            let mut hdr = [0u8; 2];
            src.read_exact(&mut hdr).await?;
            let len = u16::from_be_bytes(hdr) as usize;
            let mut body = vec![0u8; len];
            src.read_exact(&mut body).await?;
            if corrupt && !body.is_empty() {
                body[0] ^= 0x01;
            }
            dst.write_all(&hdr).await?;
            dst.write_all(&body).await?;
            dst.flush().await
        }

        // msg 1: client -> server
        forward_one(&mut mitm_client, &mut mitm_server, false).await.unwrap();
        // msg 2: server -> client
        forward_one(&mut mitm_server, &mut mitm_client, false).await.unwrap();
        // Hello: client -> server
        forward_one(&mut mitm_client, &mut mitm_server, false).await.unwrap();
        // Tampered data frame: client -> server
        forward_one(&mut mitm_client, &mut mitm_server, true).await.unwrap();
    });

    let hub_pub = hub.public();
    let init = tokio::spawn(async move {
        let mut link = handshake_initiator(
            LengthPrefixed::new(client_io),
            &peer,
            hub_pub,
            &Hello::new("old-pc"),
            PINNED_MAX_ENVELOPE,
        )
        .await
        .unwrap();
        link.send(&Envelope::new(Channel::Acp, b"{\"jsonrpc\":\"2.0\"}".to_vec())).await.unwrap();
    });

    let (mut server, _hello) = handshake_responder(LengthPrefixed::new(server_io), &hub, |_| KeyDecision::Accept {
        max_envelope_bytes: PINNED_MAX_ENVELOPE,
    })
    .await
    .unwrap();

    let err = server.recv().await.unwrap_err();
    assert!(matches!(err, WireError::Crypto(_)), "{err}");
    init.await.unwrap();
    mitm.await.unwrap();
}

#[tokio::test]
async fn envelopes_larger_than_64kib_round_trip_on_all_channels() {
    let (mut client, mut server, _hello) = establish_pair(PINNED_MAX_ENVELOPE).await;

    for (i, channel) in [Channel::Acp, Channel::Control, Channel::Llm].into_iter().enumerate() {
        // 192 KiB spans 3+ Noise 65,535-byte frames.
        let len = 192 * 1024 + i * 137;
        let payload: Vec<u8> = (0..len).map(|n| ((n.wrapping_mul(31) + i) % 256) as u8).collect();
        let sent = Envelope::new(channel, payload);

        let send_clone = sent.clone();
        let sender = tokio::spawn(async move {
            client.send(&send_clone).await.unwrap();
            client
        });
        let got = server.recv().await.unwrap();
        client = sender.await.unwrap();

        assert_eq!(got.channel, channel);
        assert_eq!(got.bytes, sent.bytes);
    }
}

#[tokio::test]
async fn oversized_unauthenticated_envelope_is_rejected() {
    // Responder accepts with the tight pre-pin 4 KiB cap.
    let (mut client, mut server, _hello) = establish_pair(PRE_PIN_MAX_ENVELOPE).await;
    let huge = Envelope::new(Channel::Control, vec![b'x'; PRE_PIN_MAX_ENVELOPE + 1]);

    let sender = tokio::spawn(async move {
        let _ = client.send(&huge).await;
    });
    let err = server.recv().await.unwrap_err();
    assert!(
        matches!(
            err,
            WireError::EnvelopeTooLarge { declared, limit }
                if declared == PRE_PIN_MAX_ENVELOPE + 1 && limit == PRE_PIN_MAX_ENVELOPE
        ),
        "{err}"
    );
    sender.await.unwrap();
}

#[tokio::test]
async fn oversized_hello_during_handshake_is_rejected_before_pinning() {
    let hub = Keypair::generate().unwrap();
    let peer = Keypair::generate().unwrap();
    let (a, b) = mem_pair();
    let hub_pub = hub.public();
    let huge_hello = Hello::new("x".repeat(PRE_PIN_MAX_ENVELOPE + 512));

    let init = tokio::spawn(async move {
        let _ = handshake_initiator(a, &peer, hub_pub, &huge_hello, PINNED_MAX_ENVELOPE).await;
    });

    // Even if the responder plans to grant PINNED_MAX_ENVELOPE after Hello,
    // the Hello frame itself is capped at PRE_PIN_MAX_ENVELOPE.
    let err =
        match handshake_responder(b, &hub, |_| KeyDecision::Accept { max_envelope_bytes: PINNED_MAX_ENVELOPE }).await {
            Ok(_) => panic!("oversized Hello must be rejected"),
            Err(e) => e,
        };
    assert!(matches!(err, WireError::EnvelopeTooLarge { .. }), "{err}");
    assert!(err.is_silent_drop());
    init.await.unwrap();
}

#[tokio::test]
async fn keepalive_answers_ping_and_times_out_silent_peer() {
    let (mut client, mut server, _hello) = establish_pair(PINNED_MAX_ENVELOPE).await;

    let ka = KeepAlive { interval: Duration::from_millis(30), timeout: Duration::from_millis(120) };

    // Server runs recv_with_keepalive; client waits past one ping interval,
    // handles the ping with its own keepalive recv, then sends a real ACP message.
    let server_task = tokio::spawn(async move {
        let mut st = KeepAliveState::new();
        recv_with_keepalive(&mut server, ka, &mut st).await
    });

    // Receive server's Ping and reply with Pong via client's helper or directly.
    let ping = client.recv().await.unwrap();
    let Control::Ping { nonce } = ping.decode_control().unwrap() else {
        panic!("expected Ping, got {ping:?}");
    };
    client.send(&Envelope::control(&Control::Pong { nonce }).unwrap()).await.unwrap();
    client.send(&Envelope::new(Channel::Acp, b"alive".to_vec())).await.unwrap();

    let delivered = server_task.await.unwrap().unwrap();
    assert_eq!(delivered.channel, Channel::Acp);
    assert_eq!(delivered.bytes, b"alive");

    // Now client stays silent: server times out waiting for Pong.
    let (mut silent_client, mut watchdog_server, _hello) = establish_pair(PINNED_MAX_ENVELOPE).await;
    let err = recv_with_keepalive(&mut watchdog_server, ka, &mut KeepAliveState::new()).await.unwrap_err();
    assert!(matches!(err, WireError::Timeout("keep-alive pong")), "{err}");
    let _ = silent_client.close(None).await;
}

#[tokio::test]
async fn dropped_link_reconnects_with_backoff() {
    let hub = Keypair::generate().unwrap();
    let peer = Keypair::generate().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("ws://{addr}/peer");

    // Server refuses the first 2 TCP connections (simulating outage / drop),
    // then completes Noise IK + Hello on attempt 3 and echoes one envelope.
    let hub_clone = hub.clone();
    let server_task = tokio::spawn(async move {
        for _ in 0..2 {
            let (sock, _) = listener.accept().await.unwrap();
            drop(sock);
        }
        let (sock, _) = listener.accept().await.unwrap();
        let ws = accept_ws(sock).await.unwrap();
        let (mut link, hello) =
            handshake_responder(ws, &hub_clone, |_| KeyDecision::Accept { max_envelope_bytes: PINNED_MAX_ENVELOPE })
                .await
                .unwrap();
        assert_eq!(hello.peer_id, "reconnect-peer");
        let msg = link.recv().await.unwrap();
        link.send(&msg).await.unwrap();
    });

    let attempts = Arc::new(AtomicUsize::new(0));
    let mut backoff = Backoff::with_seed(Duration::from_millis(10), Duration::from_millis(50), 7);
    let hub_pub = hub.public();

    let mut link = connect_with_backoff(&mut backoff, Some(5), |_| {
        let url = url.clone();
        let peer = peer.clone();
        let attempts = Arc::clone(&attempts);
        async move {
            attempts.fetch_add(1, Ordering::SeqCst);
            let ws = connect_ws(&url).await?;
            handshake_initiator(ws, &peer, hub_pub, &Hello::new("reconnect-peer"), PINNED_MAX_ENVELOPE).await
        }
    })
    .await
    .unwrap();

    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    assert_eq!(backoff.attempts(), 0, "backoff must reset on successful connect");

    let payload = Envelope::new(Channel::Llm, b"stream-chunk".to_vec());
    link.send(&payload).await.unwrap();
    let echoed = link.recv().await.unwrap();
    assert_eq!(echoed, payload);
    server_task.await.unwrap();
}

#[tokio::test]
async fn ws_loopback_round_trip() {
    let hub = Keypair::generate().unwrap();
    let peer = Keypair::generate().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("ws://{addr}/peer");

    let hub_pub = hub.public();
    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let ws = accept_ws(sock).await.unwrap();
        let (mut link, hello) =
            handshake_responder(ws, &hub, |_| KeyDecision::Accept { max_envelope_bytes: PINNED_MAX_ENVELOPE })
                .await
                .unwrap();
        assert_eq!(hello.peer_id, "lan-peer");
        let env = link.recv().await.unwrap();
        link.send(&env).await.unwrap();
    });

    let ws = connect_ws(&url).await.unwrap();
    let mut client =
        handshake_initiator(ws, &peer, hub_pub, &Hello::new("lan-peer"), PINNED_MAX_ENVELOPE).await.unwrap();

    let big = Envelope::new(Channel::Acp, vec![0x5Au8; 90 * 1024]);
    client.send(&big).await.unwrap();
    assert_eq!(client.recv().await.unwrap(), big);
    server.await.unwrap();
}

#[tokio::test]
async fn wss_loopback_round_trip_with_ephemeral_ca() {
    construct_core::http::ensure_crypto();

    // Generate an ephemeral loopback CA + leaf (short-lived for Apple verifier
    // compliance, matching crates/construct-dev/tests/tls.rs).
    let now = OffsetDateTime::now_utc();
    let mut ca_params = CertificateParams::new(Vec::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.distinguished_name.push(DnType::CommonName, "construct-peer-test-ca");
    ca_params.not_before = now - TimeDuration::hours(1);
    ca_params.not_after = now + TimeDuration::days(7);
    let ca_key = RcKeyPair::generate().unwrap();
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();

    let mut leaf_params = CertificateParams::new(vec!["localhost".into()]).unwrap();
    leaf_params.subject_alt_names =
        vec![SanType::DnsName("localhost".try_into().unwrap()), SanType::IpAddress(IpAddr::from([127, 0, 0, 1]))];
    leaf_params.not_before = now - TimeDuration::hours(1);
    leaf_params.not_after = now + TimeDuration::days(7);
    let leaf_key = RcKeyPair::generate().unwrap();
    let leaf_cert = leaf_params.signed_by(&leaf_key, &ca_cert, &ca_key).unwrap();

    let server_cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(leaf_cert.der().to_vec())],
            PrivateKeyDer::try_from(leaf_key.serialize_der()).unwrap(),
        )
        .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(server_cfg));

    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from(ca_cert.der().to_vec())).unwrap();
    let client_cfg = Arc::new(rustls::ClientConfig::builder().with_root_certificates(roots).with_no_client_auth());

    let hub = Keypair::generate().unwrap();
    let peer = Keypair::generate().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("wss://localhost:{}/peer", addr.port());

    let hub_pub = hub.public();
    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let tls = acceptor.accept(sock).await.unwrap();
        let ws = accept_ws(tls).await.unwrap();
        let (mut link, hello) =
            handshake_responder(ws, &hub, |_| KeyDecision::Accept { max_envelope_bytes: PINNED_MAX_ENVELOPE })
                .await
                .unwrap();
        assert_eq!(hello.peer_id, "tls-peer");
        let env = link.recv().await.unwrap();
        link.send(&env).await.unwrap();
    });

    let ws = connect_ws_with_tls(&url, Some(client_cfg)).await.unwrap();
    let mut client =
        handshake_initiator(ws, &peer, hub_pub, &Hello::new("tls-peer"), PINNED_MAX_ENVELOPE).await.unwrap();

    let big = Envelope::new(Channel::Llm, vec![0xA5u8; 80 * 1024]);
    client.send(&big).await.unwrap();
    assert_eq!(client.recv().await.unwrap(), big);
    server.await.unwrap();
}
