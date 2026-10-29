mod common;

use common::{app, platform, signup, Recorder};
use serde_json::{json, Value};
use std::sync::Arc;
use wxlike_client::{execute::Executor, Http};
use wxlike_cmd::policy::Policy;

fn executor(base: &str, token: Option<String>, write: bool) -> Executor {
    let mut http = Http::new(base);
    http.token = token;
    Executor::new(http, Policy::new(Vec::<String>::new(), write, false))
}

fn run(ex: &Executor, id: &str, params: Value) -> Value {
    ex.run(id, &params).unwrap_or_else(|e| panic!("{id}: {e}"))
}

fn rejects(ex: &Executor, id: &str, params: Value, status: &str) {
    let error = ex.run(id, &params).expect_err("request must fail");
    assert!(error.contains(status), "{id}: {error}");
}

fn accept_lifecycle(a: &Executor, b: &Executor, c: &Executor, alice: &str, bob: &str) {
    let created = run(
        a,
        "friend.request.create",
        json!({"to_user_id": bob, "message": "hello"}),
    );
    assert_eq!(created["from_uid"], alice);
    assert_eq!(created["to_uid"], bob);
    assert_eq!(created["status"], "pending");
    let args = json!({"req_id": created["id"]});
    let incoming = run(b, "friend.requests.incoming", json!({}));
    assert_eq!(incoming.as_array().unwrap().len(), 1);
    assert_eq!(incoming[0]["id"], created["id"]);
    assert_eq!(incoming[0]["message"], "hello");
    assert_eq!(run(a, "friend.requests.outgoing", json!({})), incoming);
    assert_eq!(run(a, "friend.requests.incoming", json!({})), json!([]));
    assert_eq!(run(b, "friend.requests.outgoing", json!({})), json!([]));
    for id in [
        "friend.list",
        "friend.requests.incoming",
        "friend.requests.outgoing",
    ] {
        assert_eq!(run(c, id, json!({})), json!([]));
    }
    for id in ["friend.request.accept", "friend.request.decline"] {
        rejects(c, id, args.clone(), "403");
        rejects(a, id, args.clone(), "403");
    }
    rejects(
        a,
        "friend.request.create",
        json!({"to_user_id": bob}),
        "409",
    );
    run(b, "friend.request.accept", args.clone());
    rejects(b, "friend.request.accept", args, "409");
    assert_eq!(run(b, "friend.requests.incoming", json!({})), json!([]));
    assert_eq!(
        run(a, "friend.requests.outgoing", json!({}))[0]["status"],
        "accepted"
    );
    for (ex, peer) in [(a, bob), (b, alice)] {
        let friends = run(ex, "friend.list", json!({}));
        assert_eq!(friends.as_array().unwrap().len(), 1);
        assert_eq!(friends[0]["friend_id"], peer);
    }
    assert_eq!(run(c, "friend.list", json!({})), json!([]));
}

fn decline_lifecycle(a: &Executor, c: &Executor, carol: &str) {
    let created = run(a, "friend.request.create", json!({"to_user_id": carol}));
    assert_eq!(created["message"], "");
    let args = json!({"req_id": created["id"]});
    run(c, "friend.request.decline", args.clone());
    rejects(c, "friend.request.decline", args, "409");
    assert_eq!(run(c, "friend.requests.incoming", json!({})), json!([]));
    assert_eq!(run(c, "friend.list", json!({})), json!([]));
    let outgoing = run(a, "friend.requests.outgoing", json!({}));
    let declined = outgoing
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == created["id"])
        .expect("declined request retained");
    assert_eq!(declined["status"], "declined");
}

fn invalid_requests(base: &str, token: String, bob: &str) {
    let authed = executor(base, Some(token.clone()), true);
    let read_only = executor(base, Some(token), false);
    let anonymous = executor(base, None, true);
    let calls = [
        ("friend.list", json!({})),
        ("friend.requests.incoming", json!({})),
        ("friend.requests.outgoing", json!({})),
        ("friend.request.create", json!({"to_user_id": bob})),
        (
            "friend.request.accept",
            json!({"req_id": "ffffffffffffffff"}),
        ),
        (
            "friend.request.decline",
            json!({"req_id": "ffffffffffffffff"}),
        ),
    ];
    for (id, params) in calls {
        rejects(&anonymous, id, params.clone(), "401");
        if id.starts_with("friend.request.") {
            rejects(&read_only, id, params, "write");
        }
    }
    for id in ["friend.request.accept", "friend.request.decline"] {
        rejects(&authed, id, json!({"req_id": "ffffffffffffffff"}), "404");
        assert!(authed.run(id, &json!({})).is_err());
        assert!(authed.run(id, &json!({"req_id": "../incoming"})).is_err());
    }
    assert!(authed.run("friend.request.create", &json!({})).is_err());
    rejects(
        &authed,
        "friend.request.create",
        json!({"to_user_id": "ffffffffffffffff"}),
        "404",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn executor_drives_authenticated_friend_routes() {
    let state = platform();
    let recorder = Recorder::new();
    state.set_event_sink(Arc::new(recorder.clone()));
    let router = app(state);
    let (alice, at) = signup(&router, "friend-cmd-alice").await;
    let (bob, bt) = signup(&router, "friend-cmd-bob").await;
    let (carol, ct) = signup(&router, "friend-cmd-carol").await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let serving = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    let result = tokio::task::spawn_blocking(move || {
        let a = executor(&base, Some(at.clone()), true);
        let b = executor(&base, Some(bt), true);
        let c = executor(&base, Some(ct), true);
        accept_lifecycle(&a, &b, &c, &alice, &bob);
        assert_eq!(recorder.take().len(), 2, "request and acceptance only");
        decline_lifecycle(&a, &c, &carol);
        assert_eq!(recorder.take().len(), 2, "request and decline only");
        invalid_requests(&base, at, &bob);
        assert_eq!(recorder.count(), 0, "rejected commands emit nothing");
    })
    .await;
    stop.send(()).unwrap();
    serving.await.unwrap();
    result.unwrap();
}
