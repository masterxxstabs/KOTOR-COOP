//! Real TCP end-to-end: start a dedicated server, join with blocking clients,
//! restart the server and check the world persisted.

use kotor_protocol::client::{connect, ClientError, Receiver};
use kotor_protocol::*;
use kotor_server::config::Config;
use kotor_server::Server;
use std::time::Duration;

fn hello(name: &str, token: &str) -> Hello {
    Hello {
        protocol_version: PROTOCOL_VERSION,
        mod_version: MOD_VERSION.into(),
        player_name: name.into(),
        player_token: token.into(),
        password: None,
        game_build: None,
    }
}

/// Reads until `pred` matches (other messages are skipped).
fn wait_for(rx: &mut Receiver, pred: impl Fn(&ServerMessage) -> bool) -> ServerMessage {
    loop {
        let m = rx.recv().expect("recv");
        if pred(&m) {
            return m;
        }
    }
}

fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::spawn(f).join().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn coop_session_persists_across_restart() {
    let data_dir = std::env::temp_dir().join(format!("kotor-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    let cfg = Config {
        bind: "127.0.0.1".into(),
        port: 0,
        data_dir: data_dir.to_string_lossy().into_owned(),
        ..Default::default()
    };

    let server = Server::start(cfg.clone()).await.unwrap();
    let addr = server.local_addr.to_string();

    let a2 = addr.clone();
    blocking(move || {
        let (w1, tx1, mut rx1) = connect(&a2, hello("Revan", "revan-secret-token-1")).unwrap();
        assert_eq!(w1.leader, Some(w1.your_id));
        let (w2, tx2, mut rx2) = connect(&a2, hello("Carth", "carth-secret-token-2")).unwrap();
        assert_eq!(w2.players.len(), 2);

        // Positions relay between players in the same module.
        for tx in [&tx1, &tx2] {
            tx.send(&ClientMessage::PlayerState(PlayerState {
                module: "end_m01aa".into(),
                position: Vec3::new(1.0, 2.0, 3.0),
                ..Default::default()
            }))
            .unwrap();
        }
        let got = wait_for(&mut rx2, |m| matches!(m, ServerMessage::PlayerStates(_)));
        let ServerMessage::PlayerStates(v) = got else {
            unreachable!()
        };
        assert_eq!(v[0].0, w1.your_id);
        assert_eq!(v[0].1.position, Vec3::new(1.0, 2.0, 3.0));

        // Plot global + world event from one player reach the other.
        tx1.send(&ClientMessage::SetGlobal {
            name: "END_TRASK_DLG".into(),
            value: GlobalValue::Number(2),
        })
        .unwrap();
        tx1.send(&ClientMessage::WorldEvent(WorldEvent::CreatureDied {
            module: "end_m01aa".into(),
            tag: "end_sith01".into(),
        }))
        .unwrap();
        tx1.send(&ClientMessage::RequestTransition {
            module: "end_m01ab".into(),
            waypoint: "wp_bridge".into(),
        })
        .unwrap();
        wait_for(&mut rx2, |m| {
            matches!(m, ServerMessage::GlobalChanged { .. })
        });
        wait_for(&mut rx2, |m| matches!(m, ServerMessage::WorldEvent { .. }));
        wait_for(
            &mut rx2,
            |m| matches!(m, ServerMessage::Transition { module, .. } if module == "end_m01ab"),
        );

        tx1.send(&ClientMessage::SaveCharacter {
            data: b"revan-sheet".to_vec(),
        })
        .unwrap();
        tx1.send(&ClientMessage::Ping { nonce: 42 }).unwrap();
        wait_for(&mut rx1, |m| *m == ServerMessage::Pong { nonce: 42 });

        // Leader leaves -> Carth leads.
        tx1.close();
        let lead = wait_for(&mut rx2, |m| {
            matches!(m, ServerMessage::LeaderChanged { .. })
        });
        assert_eq!(lead, ServerMessage::LeaderChanged { id: w2.your_id });
        tx2.close();
    });

    server.shutdown("test restart").await.unwrap();

    // Same data dir, new process lifetime: everything is still there.
    let server = Server::start(cfg).await.unwrap();
    let addr = server.local_addr.to_string();
    blocking(move || {
        let (w, _tx, mut rx) = connect(&addr, hello("Revan", "revan-secret-token-1")).unwrap();
        assert_eq!(w.world.current_module.as_deref(), Some("end_m01ab"));
        assert_eq!(w.world.entry_waypoint, "wp_bridge");
        assert_eq!(w.world.globals["END_TRASK_DLG"], GlobalValue::Number(2));
        assert_eq!(
            w.world.modules["end_m01aa"].dead_creatures,
            vec!["end_sith01".to_string()]
        );
        let ch = wait_for(&mut rx, |m| {
            matches!(m, ServerMessage::CharacterData { .. })
        });
        assert_eq!(
            ch,
            ServerMessage::CharacterData {
                data: b"revan-sheet".to_vec()
            }
        );

        // Someone else cannot take the name.
        match connect(&addr, hello("revan", "impostor-token-xxxx")) {
            Err(ClientError::Rejected { reason, .. }) => {
                assert_eq!(reason, RejectReason::NameTaken)
            }
            other => panic!("expected rejection, got {:?}", other.map(|r| r.0)),
        }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    server.shutdown("done").await.unwrap();
    std::fs::remove_dir_all(data_dir).unwrap();
}
