use session_bridge::transport;

/* REQ-006: A sender may close after writing; authenticate its retained Unix UID before reading. */
#[tokio::test]
async fn authenticates_peer_after_sender_closes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("peer.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let mut client = tokio::net::UnixStream::connect(&path).await.unwrap();
    transport::write_frame(&mut client, &serde_json::json!({"probe":true}))
        .await
        .unwrap();
    drop(client);
    let (mut server, _) = listener.accept().await.unwrap();
    transport::verify_peer(&server).unwrap();
    let frame: serde_json::Value = transport::read_frame(&mut server).await.unwrap();
    assert_eq!(frame["probe"], true);
}
