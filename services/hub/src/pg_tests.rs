//! Ticket fields on a real Postgres: the logic of `people::apply` and `/fields` through hub routes and
//! concurrent updates over two connections. End-to-end plan computation of a small scope. The database comes from `tools/test-graph-pg.sh` (the `graph-pg` part of
//! `tools/check.sh`): the `HUB_TEST_DB_URI` variable, an empty database, the test applies migrations and seeds itself. Without a database the tests
//! are marked `ignore`, and `cargo test` skips them.
//!   HUB_TEST_DB_URI=postgres://... cargo test -p hub pg_tests -- --ignored

use std::time::Duration;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tower::ServiceExt;

use crate::{
    app, callers,
    config::{Config, local_date, local_time},
    graph_view, migrations, people, plan, query, tx,
};

const IGNORE: &str = "needs HUB_TEST_DB_URI: sh tools/test-graph-pg.sh";

/// Migrations as the owner (the container superuser, app in the cluster), queries and seeds as role hub:
/// every route test also checks that role hub has enough privileges from 0021_db_roles.sql.
async fn db() -> PgPool {
    let owner = owner().await;
    // Tests call migrations and seeds in parallel: migrations have their own advisory lock, seeds have theirs. The migrator of
    // HUB_MIGRATIONS: the chain, or with `baseline` the baseline (tools/test-graph-pg.sh runs the tests both ways).
    migrations::migrator().unwrap().run(&owner).await.unwrap();
    // Like the hub-db-migrate Job: after the migrations, the instance row (a repeat by parallel tests changes nothing).
    migrations::seed_instance(&owner, &install()).await.unwrap();
    owner.close().await;
    let uri = std::env::var("HUB_TEST_DB_URI").expect(IGNORE);
    let db = PgPoolOptions::new()
        .max_connections(6)
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("set role hub").execute(conn).await?;
                Ok(())
            })
        })
        .connect(&uri)
        .await
        .unwrap();
    // Like the service start: the repository registry, then people and components. The test registry (REPOSITORIES) and
    // the example seed: the same registry for both builds (rows an install's chain may carry are not used).
    crate::documents::seed(&db, Some(REPOSITORIES))
        .await
        .unwrap();
    people::seed(&db, &example_seeds()).await.unwrap();
    db
}

/// The document repository registry of these tests: acme/widgets (as in the example seed) and acme/apps.
const REPOSITORIES: &str = "[[repository]]\nrepo = \"acme/widgets\"\nproject = \"widgets\"\ncaller = \"docs-widgets\"\n\n\
     [[repository]]\nrepo = \"acme/apps\"\nproject = \"apps\"\ncaller = \"docs-apps\"\n";

/// The neutral example seed (examples/seed) through the same directory read as at startup.
fn example_seeds() -> crate::seed::Seeds {
    crate::seed::Seeds::load(Some(&crate::seed::example_dir()), true).unwrap()
}

/// A link seed file for the graph route test: the chain c1 .. c10 of blocks, c1 .. c9 part_of c0, every row from the
/// text of a ticket.
fn chain_links() -> Vec<(String, String)> {
    let mut text = String::from("# test chain\n");
    for i in 1..10 {
        text.push_str(&format!(
            "{{\"src\": \"c{i}\", \"rel\": \"blocks\", \"dst\": \"c{n}\", \"source\": \"c{n}:body\"}}\n",
            n = i + 1
        ));
        text.push_str(&format!(
            "{{\"src\": \"c{i}\", \"rel\": \"part_of\", \"dst\": \"c0\", \"source\": \"c0:body\"}}\n"
        ));
    }
    vec![("0001-chain.jsonl".to_owned(), text)]
}

fn install() -> Config {
    Config::from_lookup(|k| match k {
        "HUB_ACCOUNT" => Some("demo".into()),
        "HUB_TIMEZONE" => Some("Europe/Helsinki".into()),
        _ => None,
    })
    .unwrap()
}

async fn owner() -> PgPool {
    let uri = std::env::var("HUB_TEST_DB_URI").expect(IGNORE);
    PgPoolOptions::new()
        .max_connections(2)
        .connect(&uri)
        .await
        .unwrap()
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    kind: &str,
    body: Value,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header(tx::ACTOR_KIND_HEADER, kind)
        .header(tx::ACTOR_HEADER, format!("test:{kind}"))
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn new_ticket(app: &Router, summary: &str, fields: Value) -> String {
    let mut body = json!({"project": "widgets", "summary": summary});
    body.as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    let (status, t) = call(app, "POST", "/api/tickets", "agent", body).await;
    assert_eq!(status, StatusCode::CREATED, "{t}");
    t["code"].as_str().unwrap().to_owned()
}

async fn fields(app: &Router, code: &str, kind: &str, body: Value) -> (StatusCode, Value) {
    call(
        app,
        "POST",
        &format!("/api/tickets/{code}/fields"),
        kind,
        body,
    )
    .await
}

/// Field edges of a ticket: (node key, whether live, close reason) in insertion order.
async fn edges(db: &PgPool, code: &str, rel: &str) -> Vec<(String, bool, Option<String>)> {
    sqlx::query_as(
        "select n.key, e.valid_to is null, e.close_reason from graph.edge e \
         join graph.ticket gt on gt.node_id = e.src join tickets t on t.seq = gt.ticket_seq \
         join graph.node n on n.id = e.dst where t.code = $1 and e.rel = $2 order by e.id",
    )
    .bind(code)
    .bind(rel)
    .fetch_all(db)
    .await
    .unwrap()
}

async fn new_person(app: &Router, handle: &str) {
    let (status, p) = call(
        app,
        "POST",
        "/api/people",
        "human",
        json!({"handle": handle, "name": handle}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{p}");
}

async fn new_component(app: &Router, slug: &str) {
    let (status, c) = call(
        app,
        "POST",
        "/api/components",
        "human",
        json!({"slug": slug, "title": slug}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{c}");
}

#[tokio::test]
#[ignore = "needs HUB_TEST_DB_URI: sh tools/test-graph-pg.sh"]
async fn fields_full_set_noop_errors_rollback_and_retired() {
    let db = db().await;
    let app = app(db.clone(), callers::Callers::default());
    new_person(&app, "pgf-alice").await;
    new_person(&app, "pgf-bob").await;
    for c in ["pgf-a", "pgf-b", "pgf-old"] {
        new_component(&app, c).await;
    }

    // Creation with fields; components sets the whole set: extra ones are closed with removed, missing ones are added.
    let code = new_ticket(
        &app,
        "pg fields",
        json!({"assignee": "pgf-alice", "components": ["pgf-a"]}),
    )
    .await;
    let before: (chrono::DateTime<chrono::Utc>,) =
        sqlx::query_as("select updated_at from tickets where code = $1")
            .bind(&code)
            .fetch_one(&db)
            .await
            .unwrap();
    let (status, out) = fields(&app, &code, "agent", json!({"components": ["pgf-b"]})).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["components"], json!(["pgf-b"]));
    assert_eq!(
        edges(&db, &code, "in_component").await,
        [
            ("pgf-a".into(), false, Some("removed".into())),
            ("pgf-b".into(), true, None)
        ]
    );
    let after: (chrono::DateTime<chrono::Utc>,) =
        sqlx::query_as("select updated_at from tickets where code = $1")
            .bind(&code)
            .fetch_one(&db)
            .await
            .unwrap();
    assert!(after.0 > before.0, "/fields moves updated_at like PATCH");

    // Same assignee: neither a close nor a new row.
    let (status, _) = fields(&app, &code, "agent", json!({"assignee": "pgf-alice"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        edges(&db, &code, "assigned_to").await,
        [("pgf-alice".into(), true, None)]
    );

    // Unknown person and component: 404 with the key, the update is not applied.
    let (status, e) = fields(&app, &code, "agent", json!({"assignee": "pg-nobody"})).await;
    assert_eq!(
        (status, &e["error"]),
        (StatusCode::NOT_FOUND, &json!("unknown_person"))
    );
    let (status, e) = fields(&app, &code, "agent", json!({"components": ["pgf-nope"]})).await;
    assert_eq!(
        (status, &e["error"]),
        (StatusCode::NOT_FOUND, &json!("unknown_component"))
    );
    assert_eq!(edges(&db, &code, "in_component").await.len(), 2);

    // An agent does not remove an assignee and a component set by a human: 403 close_requires_human (GRHCL from the database).
    let (status, _) = fields(
        &app,
        &code,
        "human",
        json!({"assignee": "pgf-bob", "components": ["pgf-a"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, e) = fields(&app, &code, "agent", json!({"assignee": "pgf-alice"})).await;
    assert_eq!(
        (status, &e["error"]),
        (StatusCode::FORBIDDEN, &json!("close_requires_human"))
    );
    let (status, e) = fields(&app, &code, "agent", json!({"components": []})).await;
    assert_eq!(
        (status, &e["error"]),
        (StatusCode::FORBIDDEN, &json!("close_requires_human"))
    );
    let (_, t) = call(
        &app,
        "GET",
        &format!("/api/tickets/{code}"),
        "agent",
        Value::Null,
    )
    .await;
    assert_eq!(
        (&t["assignee"], &t["components"]),
        (&json!("pgf-bob"), &json!(["pgf-a"]))
    );

    // An unknown assignee on creation rolls back the ticket itself.
    let (status, _) = call(
        &app,
        "POST",
        "/api/tickets",
        "agent",
        json!({"project": "widgets", "summary": "pg rollback probe", "assignee": "pg-nobody"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (n,): (i64,) =
        sqlx::query_as("select count(*) from tickets where summary = 'pg rollback probe'")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(n, 0);

    // A retired person and component: those already set stay when the form is saved and can be removed,
    // they are not set again (409 retired).
    new_person(&app, "pg-old").await;
    let code = new_ticket(&app, "pg retired", json!({})).await;
    let (status, _) = fields(
        &app,
        &code,
        "human",
        json!({"assignee": "pg-old", "components": ["pgf-old", "pgf-a"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let mut t = tx::begin(
        &db,
        &tx::Actor {
            id: "test:human".into(),
            kind: "human".into(),
        },
    )
    .await
    .unwrap();
    sqlx::query(
        "update graph.node set retired_at = now(), retire_reason = 'test' \
         where (kind, key) in (('person', 'pg-old'), ('component', 'pgf-old'))",
    )
    .execute(&mut *t)
    .await
    .unwrap();
    t.commit().await.unwrap();
    let form = json!({"assignee": "pg-old", "components": ["pgf-old", "pgf-a", "pgf-b"]});
    let (status, out) = fields(&app, &code, "human", form).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["components"], json!(["pgf-a", "pgf-b", "pgf-old"]));
    let (status, out) = fields(
        &app,
        &code,
        "human",
        json!({"assignee": null, "components": ["pgf-a"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    for body in [
        json!({"assignee": "pg-old"}),
        json!({"components": ["pgf-old"]}),
    ] {
        let (status, e) = fields(&app, &code, "human", body).await;
        assert_eq!(
            (status, &e["error"]),
            (StatusCode::CONFLICT, &json!("retired"))
        );
    }

    // Only a human adds people and components; handle none is taken; an address in a different case is taken.
    for (uri, body) in [
        ("/api/people", json!({"handle": "pg-agent", "name": "A"})),
        ("/api/components", json!({"slug": "pg-agent", "title": "A"})),
    ] {
        let (status, e) = call(&app, "POST", uri, "agent", body).await;
        assert_eq!(
            (status, &e["error"]),
            (StatusCode::FORBIDDEN, &json!("requires_human"))
        );
    }
    let (status, _) = call(
        &app,
        "POST",
        "/api/people",
        "human",
        json!({"handle": "none", "name": "N"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(
        &app,
        "POST",
        "/api/people",
        "human",
        json!({"handle": "pg-mail", "name": "M", "email": "Pg@x.test"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, e) = call(
        &app,
        "POST",
        "/api/people",
        "human",
        json!({"handle": "pg-mail2", "name": "M", "email": "pg@X.test"}),
    )
    .await;
    assert_eq!(
        (status, &e["error"]),
        (StatusCode::CONFLICT, &json!("email_taken"))
    );
    let (status, e) = call(
        &app,
        "POST",
        "/api/people",
        "human",
        json!({"handle": "pg-mail", "name": "M"}),
    )
    .await;
    assert_eq!(
        (status, &e["error"]),
        (StatusCode::CONFLICT, &json!("exists"))
    );
}

/// The first update holds a transaction (`people::apply` on its own connection), the second goes through `/fields` and waits for
/// the ticket row lock; after the first commits, the second computes the difference from its set.
async fn race(
    db: &PgPool,
    app: &Router,
    code: &str,
    first: people::Fields,
    second: Value,
) -> (StatusCode, Value) {
    let (seq,): (i64,) = sqlx::query_as("select seq from tickets where code = $1")
        .bind(code)
        .fetch_one(db)
        .await
        .unwrap();
    let mut t1 = tx::begin(
        db,
        &tx::Actor {
            id: "test:agent".into(),
            kind: "agent".into(),
        },
    )
    .await
    .unwrap();
    people::apply(&mut t1, seq, &first).await.unwrap();
    let (app2, code2) = (app.clone(), code.to_owned());
    let t2 = tokio::spawn(async move { fields(&app2, &code2, "agent", second).await });
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !t2.is_finished(),
        "the second write waits for the ticket row lock"
    );
    t1.commit().await.unwrap();
    t2.await.unwrap()
}

#[tokio::test]
#[ignore = "needs HUB_TEST_DB_URI: sh tools/test-graph-pg.sh"]
async fn parallel_field_writes_queue_on_the_ticket_row() {
    let db = db().await;
    let app = app(db.clone(), callers::Callers::default());
    for h in ["pgr-alice", "pgr-bob", "pgr-carol"] {
        new_person(&app, h).await;
    }
    for c in ["pgr-a", "pgr-b", "pgr-c", "pgr-d"] {
        new_component(&app, c).await;
    }

    // Two assignee changes: both pass in turn, one is live (the last), the previous ones are closed as superseded.
    let code = new_ticket(&app, "pg race assignee", json!({"assignee": "pgr-alice"})).await;
    let first = people::Fields {
        assignee: Some(Some("pgr-bob".into())),
        components: None,
    };
    let (status, out) = race(&db, &app, &code, first, json!({"assignee": "pgr-carol"})).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(
        edges(&db, &code, "assigned_to").await,
        [
            ("pgr-alice".into(), false, Some("superseded".into())),
            ("pgr-bob".into(), false, Some("superseded".into())),
            ("pgr-carol".into(), true, None)
        ]
    );

    // Two full sets from empty: the result is the set of the second update, not the union.
    let code = new_ticket(&app, "pg race components", json!({})).await;
    let first = people::Fields {
        assignee: None,
        components: Some(vec!["pgr-a".into()]),
    };
    let (status, out) = race(&db, &app, &code, first, json!({"components": ["pgr-b"]})).await;
    assert_eq!(
        (status, &out["components"]),
        (StatusCode::OK, &json!(["pgr-b"]))
    );

    // An empty set against {c, d} with c live: the result is {c, d}, not {d}.
    let code = new_ticket(
        &app,
        "pg race components 2",
        json!({"components": ["pgr-c"]}),
    )
    .await;
    let first = people::Fields {
        assignee: None,
        components: Some(vec![]),
    };
    let (status, out) = race(
        &db,
        &app,
        &code,
        first,
        json!({"components": ["pgr-c", "pgr-d"]}),
    )
    .await;
    assert_eq!(
        (status, &out["components"]),
        (StatusCode::OK, &json!(["pgr-c", "pgr-d"]))
    );
    let live: Vec<String> = edges(&db, &code, "in_component")
        .await
        .into_iter()
        .filter(|e| e.1)
        .map(|e| e.0)
        .collect();
    assert_eq!(live, ["pgr-c", "pgr-d"]);
}

/// The link seed on a real Postgres (a repeated import changes nothing) and the /api/graph layout
/// through a hub route: the layers of a test chain from left to right, the response is the same from call to call.
#[tokio::test]
#[ignore = "needs HUB_TEST_DB_URI: sh tools/test-graph-pg.sh"]
async fn seed_links_and_graph_layout() {
    let db = db().await;
    let app = app(db.clone(), callers::Callers::default());
    let files = chain_links();
    let links = graph_view::parse_link_files(&files).unwrap();
    let mut codes: Vec<&str> = links
        .iter()
        .flat_map(|l| [l.src.as_str(), l.dst.as_str()])
        .collect();
    codes.sort();
    codes.dedup();
    // Tickets are written only with an actor (0018_ticket_actor_strict).
    let mut t = tx::begin(&db, &tx::Actor::seed("seed:test")).await.unwrap();
    for c in &codes {
        sqlx::query("insert into tickets(project, code, summary) values ('gadgets', $1, $1) on conflict do nothing")
            .bind(c)
            .execute(&mut *t)
            .await
            .unwrap();
    }
    t.commit().await.unwrap();

    graph_view::seed(&db, &files).await.unwrap();
    let count = || async {
        let (n,): (i64,) =
            sqlx::query_as("select count(*) from graph.edge where created_by = 'seed:links'")
                .fetch_one(&db)
                .await
                .unwrap();
        n
    };
    assert_eq!(count().await, links.len() as i64);
    graph_view::seed(&db, &files).await.unwrap();
    assert_eq!(
        count().await,
        links.len() as i64,
        "second import writes nothing"
    );

    let (status, g) = call(
        &app,
        "GET",
        "/api/graph?root=c5&depth=10",
        "human",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{g}");
    let layer = |code: &str| {
        g["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["code"] == code)
            .unwrap()["layer"]
            .as_u64()
            .unwrap()
    };
    let chain = ["c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8", "c9", "c10"];
    for w in chain.windows(2) {
        assert!(layer(w[0]) < layer(w[1]), "{} before {}", w[0], w[1]);
    }
    let (_, again) = call(
        &app,
        "GET",
        "/api/graph?root=c5&depth=10",
        "human",
        Value::Null,
    )
    .await;
    assert_eq!(g, again, "layout is stable");
    let (status, p) = call(
        &app,
        "GET",
        "/api/graph?project=gadgets",
        "human",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(p["nodes"].as_array().unwrap().len() >= chain.len());
    let (status, _) = call(&app, "GET", "/api/graph?root=nosuch", "human", Value::Null).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Step 2.6: repository registry from the seed on a real Postgres as role hub. The registry of db() is there already (nothing
/// is written), another seed adds acme/gadgets once, a repeat changes nothing, nothing is deleted.
#[tokio::test]
#[ignore = "needs HUB_TEST_DB_URI: sh tools/test-graph-pg.sh"]
async fn seed_repositories_add_and_never_delete() {
    let db = db().await;
    let report = |created: i64, updated: i64, unchanged: i64| {
        Some(json!({"created": created, "updated": updated, "unchanged": unchanged}))
    };
    let base = Some(REPOSITORIES);
    let other = Some(
        "[[repository]]\nrepo = \"acme/gadgets\"\nproject = \"gadgets\"\ncaller = \"docs-gadgets\"\n",
    );
    let seed = crate::documents::seed;
    assert_eq!(seed(&db, base).await.unwrap(), report(0, 0, 2));
    assert_eq!(seed(&db, other).await.unwrap(), report(1, 0, 0));
    assert_eq!(seed(&db, other).await.unwrap(), report(0, 0, 1));
    // The other seed does not carry the base repositories: they stay in the registry, a repeated base seed does not touch them.
    assert_eq!(seed(&db, base).await.unwrap(), report(0, 0, 2));
    assert_eq!(seed(&db, None).await.unwrap(), None);
}

/// Ticket code from the create response; assignee fields and tags are set right here.
/// The scope seed (scope w1 of the example) on a real Postgres gives the scope a plan (not 404 `no_scope`) without a baseline; a repeated
/// seed changes nothing.
#[tokio::test]
#[ignore = "needs HUB_TEST_DB_URI: sh tools/test-graph-pg.sh"]
async fn seed_scopes_gives_the_scope_a_plan() {
    let db = db().await;
    let app = app(db.clone(), callers::Callers::default());
    let mut t = tx::begin(&db, &tx::Actor::seed("seed:test")).await.unwrap();
    for c in ["w2", "w3"] {
        sqlx::query("insert into tickets(project, code, summary) values ('widgets', $1, $1) on conflict do nothing")
            .bind(c)
            .execute(&mut *t)
            .await
            .unwrap();
    }
    t.commit().await.unwrap();
    crate::plan_seed::seed(&db, example_seeds().scopes.as_deref())
        .await
        .unwrap();
    crate::plan_seed::seed(&db, example_seeds().scopes.as_deref())
        .await
        .unwrap();
    let (scopes, edges): (i64, i64) = sqlx::query_as(
        "select (select count(*) from plan.scope s join tickets t on t.seq = s.root_seq where t.code = 'w1'), \
         (select count(*) from graph.edge where created_by = 'seed:scopes' and rel = 'part_of')",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!((scopes, edges), (1, 2), "second run changes nothing");

    let mut conn = db.acquire().await.unwrap();
    let today = plan::local_now(&mut conn).await.unwrap().date();
    drop(conn);
    plan::recompute_dirty(&db, today).await.unwrap();
    let p = ok(&app, "GET", "/api/plan/w1", "agent", json!(null)).await;
    assert_eq!(p["root"], json!("w1"));
    assert_eq!(p["baseline"], Value::Null, "a human approves the baseline");
    let codes = plan_codes(&p, |_| true);
    for c in ["w2", "w3"] {
        assert!(codes.iter().any(|x| x == c), "{c} in {codes:?}");
    }
}

async fn plan_ticket(app: &Router, summary: &str, fields: Value) -> String {
    new_ticket(app, summary, fields).await
}

async fn ok(app: &Router, method: &str, uri: &str, kind: &str, body: Value) -> Value {
    let (status, out) = call(app, method, uri, kind, body).await;
    assert!(status.is_success(), "{method} {uri}: {status} {out}");
    out
}

/// Plan task codes by flag.
fn plan_codes(plan: &Value, pred: impl Fn(&Value) -> bool) -> Vec<String> {
    plan["plan"]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| pred(t))
        .map(|t| t["code"].as_str().unwrap().to_owned())
        .collect()
}

/// End-to-end plan computation of a small scope through hub routes: scope, estimates and WSJF,
/// an external blocker, queued recomputation with skipping by input_hash, the next task, the baseline approval threshold,
/// fever points and the backlog by WSJF.
#[tokio::test]
#[ignore = "needs HUB_TEST_DB_URI: sh tools/test-graph-pg.sh"]
async fn plan_of_a_small_scope_through_the_router() {
    let db = db().await;
    let app = app(db.clone(), callers::Callers::default());
    new_person(&app, "pg-plan").await;
    new_person(&app, "pg-plan2").await;
    let r = plan_ticket(&app, "pg plan root", json!({})).await;
    let a = plan_ticket(&app, "pg plan a", json!({"assignee": "pg-plan"})).await;
    let b = plan_ticket(&app, "pg plan b", json!({"assignee": "pg-plan"})).await;
    let c = plan_ticket(
        &app,
        "pg plan c",
        json!({"assignee": "pg-plan2", "tags": ["feature"]}),
    )
    .await;
    let x = plan_ticket(&app, "pg plan outside blocker", json!({})).await;
    let edge = |src: &str, rel: &str, dst: &str| json!({"src": {"kind": "ticket", "key": src}, "rel": rel, "dst": {"kind": "ticket", "key": dst}});
    for (s, rel, d) in [
        (&a, "part_of", &r),
        (&b, "part_of", &r),
        (&c, "part_of", &r),
        (&a, "blocks", &b),
        (&b, "blocks", &c),
        (&x, "blocks", &a),
    ] {
        ok(&app, "POST", "/api/edges", "agent", edge(s, rel, d)).await;
    }

    // Scope and estimates; body checks before the database and the actor kind in the version.
    let scope = ok(&app, "POST", "/api/scopes", "agent", json!({"root": r})).await;
    assert_eq!(scope["root"], json!(r));
    assert_eq!(scope["actor_kind"], json!("agent"));
    let (status, e) = call(&app, "POST", "/api/scopes", "agent", json!({"root": r})).await;
    assert_eq!(
        (status, &e["error"]),
        (StatusCode::CONFLICT, &json!("scope_exists"))
    );
    let (status, _) = call(
        &app,
        "PUT",
        &format!("/api/tickets/{a}/estimate"),
        "agent",
        json!({"effort_sp": 4}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    ok(
        &app,
        "PUT",
        &format!("/api/tickets/{a}/estimate"),
        "agent",
        json!({"effort_sp": 3}),
    )
    .await;
    let est = ok(
        &app,
        "PUT",
        &format!("/api/tickets/{a}/estimate"),
        "agent",
        json!({"effort_sp": 3, "aggressive_hd": 4}),
    )
    .await;
    assert_eq!(
        (&est["aggressive_hd"], &est["actor_kind"]),
        (&json!(4), &json!("agent"))
    );
    ok(
        &app,
        "PUT",
        &format!("/api/tickets/{b}/estimate"),
        "agent",
        json!({"effort_sp": 2}),
    )
    .await;
    let w = ok(
        &app,
        "PUT",
        &format!("/api/tickets/{a}/wsjf"),
        "agent",
        json!({"business_value": 5, "time_criticality": 3, "risk_opportunity": 2}),
    )
    .await;
    assert_eq!((&w["cost_of_delay"], &w["wsjf"]), (&json!(10), &json!(333)));
    // Live versions for the ticket page forms: the last estimate and WSJF, B has no WSJF.
    let pf = ok(
        &app,
        "GET",
        &format!("/api/tickets/{a}/plan"),
        "agent",
        json!(null),
    )
    .await;
    assert_eq!(
        (&pf["estimate"]["aggressive_hd"], &pf["wsjf"]["wsjf"]),
        (&json!(4), &json!(333))
    );
    let pf = ok(
        &app,
        "GET",
        &format!("/api/tickets/{b}/plan"),
        "agent",
        json!(null),
    )
    .await;
    assert_eq!(
        (&pf["estimate"]["effort_sp"], &pf["wsjf"]),
        (&json!(2), &json!(null))
    );
    let versions: (i64,) = sqlx::query_as(
        "select count(*) from plan.ticket_estimate e join tickets t on t.seq = e.ticket_seq where t.code = $1",
    )
    .bind(&a)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(versions.0, 2, "two versions of the estimate");

    // Before the first recomputation the plan is computed on the fly.
    let live = ok(&app, "GET", &format!("/api/plan/{r}"), "agent", json!(null)).await;
    assert_eq!(live["source"], json!("live"));
    assert_eq!(live["pending"], json!(true));

    let mut conn = db.acquire().await.unwrap();
    let today = plan::local_now(&mut conn).await.unwrap().date();
    drop(conn);
    let run = || plan::recompute_dirty(&db, today);
    assert!(run().await.unwrap() >= 1);
    // The blocker outside the scope is open and has no projection: a milestone at today, A in the chain but not ready.
    let p = ok(&app, "GET", &format!("/api/plan/{r}"), "agent", json!(null)).await;
    assert_eq!(
        (&p["source"], &p["pending"]),
        (&json!("stored"), &json!(false))
    );
    let flagged =
        |p: &Value, f: &str| plan_codes(p, |t| t["flags"].as_array().unwrap().contains(&json!(f)));
    assert_eq!(flagged(&p, "external_blocker"), [a.as_str()]);
    let on_chain = plan_codes(&p, |t| t["on_chain"] == json!(true));
    assert_eq!(on_chain, [a.as_str(), b.as_str(), c.as_str()]);
    assert!(plan_codes(&p, |t| t["ready"] == json!(true)).is_empty());

    // The blocker is done: the status of the ticket outside the scope marks the scope, chain A -> B -> C.
    ok(
        &app,
        "PATCH",
        &format!("/api/tickets/{x}"),
        "agent",
        json!({"status": "done"}),
    )
    .await;
    assert!(run().await.unwrap() >= 1);
    let p = ok(&app, "GET", &format!("/api/plan/{r}"), "agent", json!(null)).await;
    let chain: Vec<i64> = p["plan"]["chain"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["seq"].as_i64().unwrap())
        .collect();
    let seq_of = |code: &str| {
        p["plan"]["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["code"] == json!(code))
            .unwrap()["seq"]
            .as_i64()
            .unwrap()
    };
    assert_eq!(chain, [seq_of(&a), seq_of(&b), seq_of(&c)]);
    assert_eq!(plan_codes(&p, |t| t["ready"] == json!(true)), [a.as_str()]);
    assert_eq!(flagged(&p, "estimate_defaulted"), [c.as_str()]);
    // C without an estimate: 5 points by the feature tag out of the chain's 11 half-days.
    assert_eq!(p["plan"]["defaulted_pct"], json!(5 * 100 / 11));
    assert_eq!(p["plan"]["wsjf_root"], json!(10 * 100 / 3));

    // A repeat without changes: the same input_hash, plan.current is not touched.
    let at = |db: PgPool, root: String| async move {
        let (t,): (chrono::DateTime<chrono::Utc>,) = sqlx::query_as(
            "select c.computed_at from plan.current c join tickets t on t.seq = c.root_seq where t.code = $1",
        )
        .bind(root)
        .fetch_one(&db)
        .await
        .unwrap();
        t
    };
    let before = at(db.clone(), r.clone()).await;
    sqlx::query("select plan.mark_now('demo', array[(select seq from tickets where code = $1)])")
        .bind(&r)
        .execute(&db)
        .await
        .unwrap();
    run().await.unwrap();
    assert_eq!(at(db.clone(), r.clone()).await, before);
    let p = ok(&app, "GET", &format!("/api/plan/{r}"), "agent", json!(null)).await;
    assert_eq!(p["pending"], json!(false));

    // Next task.
    let n = ok(
        &app,
        "GET",
        "/api/next?person=pg-plan",
        "agent",
        json!(null),
    )
    .await;
    assert_eq!((&n["code"], &n["scope"]), (&json!(a), &json!(r)));
    let (status, e) = call(
        &app,
        "GET",
        "/api/next?person=pg-nobody",
        "agent",
        json!(null),
    )
    .await;
    assert_eq!(
        (status, &e["error"]),
        (StatusCode::NOT_FOUND, &json!("unknown_person"))
    );
    let (status, _) = call(
        &app,
        "GET",
        "/api/next?person=Bad_One",
        "agent",
        json!(null),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Baseline: an agent may not; a human hits the default estimate threshold until accepting it explicitly.
    let uri = format!("/api/plan/{r}/baseline");
    let (status, e) = call(&app, "POST", &uri, "agent", json!({})).await;
    assert_eq!(
        (status, &e["error"]),
        (StatusCode::FORBIDDEN, &json!("approve_requires_human"))
    );
    let (status, e) = call(&app, "POST", &uri, "human", json!({})).await;
    assert_eq!(
        (status, &e["error"], &e["defaulted_pct"]),
        (
            StatusCode::CONFLICT,
            &json!("defaults_over_threshold"),
            &json!(45)
        )
    );
    let b1 = ok(
        &app,
        "POST",
        &uri,
        "human",
        json!({"accept_defaults": true}),
    )
    .await;
    assert_eq!(b1["baseline"]["accepted_defaults"], json!(true));
    assert_eq!(b1["baseline"]["approved_kind"], json!("human"));
    assert_eq!(b1["result"]["chain"].as_array().unwrap().len(), 3);
    assert!(run().await.unwrap() >= 1);
    let p = ok(&app, "GET", &format!("/api/plan/{r}"), "agent", json!(null)).await;
    assert_eq!(p["baseline"]["id"], b1["baseline"]["id"]);
    assert_eq!(p["plan"]["buffers"][0]["zone"], json!("green"));
    let fever: Vec<(String, String)> = sqlx::query_as(
        "select f.buffer_key, f.zone from plan.fever_point f join tickets t on t.seq = f.root_seq \
         where t.code = $1 and f.day = $2 order by 1",
    )
    .bind(&r)
    .bind(today)
    .fetch_all(&db)
    .await
    .unwrap();
    assert!(fever.contains(&("pb".into(), "green".into())), "{fever:?}");
    let f = ok(
        &app,
        "GET",
        &format!("/api/plan/{r}/fever"),
        "agent",
        json!(null),
    )
    .await;
    assert_eq!(f["baseline"]["id"], b1["baseline"]["id"]);
    assert!(
        f["points"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["buffer_key"] == json!("pb")
                && p["zone"] == json!("green")
                && p["baseline_id"] == b1["baseline"]["id"]),
        "{f}"
    );

    // The second baseline replaces the first; one is live.
    let b2 = ok(
        &app,
        "POST",
        &uri,
        "human",
        json!({"accept_defaults": true}),
    )
    .await;
    let live: Vec<(i64,)> = sqlx::query_as(
        "select b.id from plan.baseline_live b join tickets t on t.seq = b.root_seq where t.code = $1",
    )
    .bind(&r)
    .fetch_all(&db)
    .await
    .unwrap();
    assert_eq!(live, [(b2["baseline"]["id"].as_i64().unwrap(),)]);

    // A new scope version; the scope list.
    let s2 = ok(
        &app,
        "PUT",
        &format!("/api/scopes/{r}"),
        "human",
        json!({"max_defaulted_pct": 50}),
    )
    .await;
    assert_eq!(s2["max_defaulted_pct"], json!(50));
    assert_ne!(s2["id"], scope["id"]);
    // Only a human changes thresholds; an agent's PUT without thresholds keeps them.
    let (status, e) = call(
        &app,
        "PUT",
        &format!("/api/scopes/{r}"),
        "agent",
        json!({"max_defaulted_pct": 100}),
    )
    .await;
    assert_eq!(
        (status, &e["error"]),
        (StatusCode::FORBIDDEN, &json!("thresholds_require_human"))
    );
    let s3 = ok(
        &app,
        "PUT",
        &format!("/api/scopes/{r}"),
        "agent",
        json!({"due_date": "2030-01-15"}),
    )
    .await;
    assert_eq!(
        (&s3["due_date"], &s3["max_defaulted_pct"], &s3["actor_kind"]),
        (&json!("2030-01-15"), &json!(50), &json!("agent"))
    );
    let list = ok(&app, "GET", "/api/scopes", "agent", json!(null)).await;
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .any(|s| s["root"] == json!(r) && s["baseline_id"] == b2["baseline"]["id"])
    );

    // Backlog: leaves by WSJF, without WSJF last; umbrella R is not a leaf.
    let bl = ok(
        &app,
        "GET",
        "/api/backlog?project=widgets&limit=500",
        "agent",
        json!(null),
    )
    .await;
    let codes: Vec<&str> = bl["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["code"].as_str().unwrap())
        .collect();
    let pos = |c: &str| codes.iter().position(|x| *x == c);
    assert!(pos(&a).unwrap() < pos(&b).unwrap(), "{codes:?}");
    assert_eq!(pos(&r), None);
    assert!(
        bl["scopes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["root"] == json!(r) && s["wsjf_root"] == json!(333))
    );
}

/// Two transactions touch scopes A and B in different orders: marks accumulate until commit and
/// are written in one command in root order, so no update waits for a plan.dirty row, and the commits do not
/// form a ring (before the fix: `deadlock detected` in plan.ticket_dirty).
#[tokio::test]
#[ignore = "needs HUB_TEST_DB_URI: sh tools/test-graph-pg.sh"]
async fn opposite_scope_writes_do_not_deadlock() {
    let db = db().await;
    let app = app(db.clone(), callers::Callers::default());
    let ra = plan_ticket(&app, "pg dl root a", json!({})).await;
    let rb = plan_ticket(&app, "pg dl root b", json!({})).await;
    let a = plan_ticket(&app, "pg dl a", json!({})).await;
    let a2 = plan_ticket(&app, "pg dl a2", json!({})).await;
    let b = plan_ticket(&app, "pg dl b", json!({})).await;
    let b2 = plan_ticket(&app, "pg dl b2", json!({})).await;
    for (s, d) in [(&a, &ra), (&a2, &ra), (&b, &rb), (&b2, &rb)] {
        ok(
            &app,
            "POST",
            "/api/edges",
            "agent",
            json!({"src": {"kind": "ticket", "key": s}, "rel": "part_of", "dst": {"kind": "ticket", "key": d}}),
        )
        .await;
    }
    ok(&app, "POST", "/api/scopes", "agent", json!({"root": ra})).await;
    ok(&app, "POST", "/api/scopes", "agent", json!({"root": rb})).await;
    let roots = vec![ra.clone(), rb.clone()];
    let marked = |db: PgPool, roots: Vec<String>| async move {
        let rows: Vec<(i64,)> = sqlx::query_as(
            "select d.marked from plan.dirty d join tickets t on t.seq = d.root_seq \
             where t.code = any($1) order by t.code",
        )
        .bind(roots)
        .fetch_all(&db)
        .await
        .unwrap();
        rows.into_iter().map(|r| r.0).collect::<Vec<_>>()
    };
    let before = marked(db.clone(), roots.clone()).await;
    let agent = tx::Actor {
        id: "test:agent".into(),
        kind: "agent".into(),
    };
    async fn set_done(t: &mut sqlx::Transaction<'static, sqlx::Postgres>, code: &str) {
        sqlx::query("update tickets set status = 'done' where code = $1")
            .bind(code)
            .execute(&mut **t)
            .await
            .unwrap();
    }
    let steps = async {
        let mut t1 = tx::begin(&db, &agent).await.unwrap();
        let mut t2 = tx::begin(&db, &agent).await.unwrap();
        set_done(&mut t1, &a).await;
        set_done(&mut t2, &b2).await;
        set_done(&mut t1, &b).await;
        set_done(&mut t2, &a2).await;
        let (c1, c2) = tokio::join!(t1.commit(), t2.commit());
        c1.unwrap();
        c2.unwrap();
    };
    tokio::time::timeout(Duration::from_secs(20), steps)
        .await
        .expect("writes to scopes A and B in opposite order wait for each other");
    let after = marked(db.clone(), roots).await;
    assert_eq!(after.len(), 2);
    assert_eq!(
        after,
        [before[0] + 2, before[1] + 2],
        "{before:?} -> {after:?}"
    );
}

/// Scopes deferred by the write limit are not picked from the queue and do not hold its head.
#[tokio::test]
#[ignore = "needs HUB_TEST_DB_URI: sh tools/test-graph-pg.sh"]
async fn throttled_scopes_do_not_hold_the_dirty_queue() {
    let db = db().await;
    let app = app(db.clone(), callers::Callers::default());
    let ra = plan_ticket(&app, "pg throttle root a", json!({})).await;
    let rb = plan_ticket(&app, "pg throttle root b", json!({})).await;
    // Scope A is marked before B: without skipping, the deferred A would stand at the head of the queue.
    ok(&app, "POST", "/api/scopes", "agent", json!({"root": ra})).await;
    ok(&app, "POST", "/api/scopes", "agent", json!({"root": rb})).await;
    let seq = |code: String| {
        let db = db.clone();
        async move {
            let (s,): (i64,) = sqlx::query_as("select seq from tickets where code = $1")
                .bind(code)
                .fetch_one(&db)
                .await
                .unwrap();
            s
        }
    };
    let (a, b) = (seq(ra).await, seq(rb.clone()).await);
    let pending = |root: i64| {
        let db = db.clone();
        async move {
            let (p,): (bool,) = sqlx::query_as(
                "select marked > done from plan.dirty where account = 'demo' and root_seq = $1",
            )
            .bind(root)
            .fetch_one(&db)
            .await
            .unwrap();
            p
        }
    };
    assert!(pending(a).await && pending(b).await);
    // A and all other dirty scopes earlier than B are deferred (other tests of the same database left them): a pass of one
    // scope must take B.
    let ahead: Vec<(i64,)> = sqlx::query_as(
        "select root_seq from plan.dirty where account = 'demo' and marked > done and root_seq <> $1 \
         and since <= (select since from plan.dirty where account = 'demo' and root_seq = $1)",
    )
    .bind(b)
    .fetch_all(&db)
    .await
    .unwrap();
    assert!(ahead.iter().any(|r| r.0 == a), "{ahead:?}");
    let log = std::sync::Mutex::new(plan::WriteLog::default());
    let now = std::time::Instant::now();
    for (root,) in &ahead {
        for _ in 0..plan::WRITE_LIMIT {
            log.lock().unwrap().record(*root, now);
        }
    }
    let mut conn = db.acquire().await.unwrap();
    let today = plan::local_now(&mut conn).await.unwrap().date();
    drop(conn);
    plan::recompute_dirty_with(&db, today, &log, 1)
        .await
        .unwrap();
    assert!(
        !pending(b).await,
        "B is recomputed although A is ahead of it"
    );
    let p = ok(
        &app,
        "GET",
        &format!("/api/plan/{rb}"),
        "agent",
        json!(null),
    )
    .await;
    assert_eq!(p["source"], json!("stored"));
}

/// Document sync through a hub route with the caller check and an actor in the transaction.
#[tokio::test]
#[ignore = "needs HUB_TEST_DB_URI: sh tools/test-graph-pg.sh"]
async fn document_sync_binds_the_caller_to_its_repository_and_maps_errors() {
    let db = db().await;
    let secret = &"d".repeat(40);
    let app = app(
        db.clone(),
        callers::Callers::from_set(
            callers::CallerSet::new([("docs-widgets", secret), ("hub-web", &"w".repeat(40))])
                .unwrap(),
        ),
    );
    let post = |body: Value| {
        let app = app.clone();
        async move {
            let req = Request::post("/api/documents/sync")
                .header("content-type", "application/json")
                .header(crate::callers::CALLER_HEADER, "docs-widgets")
                .header("authorization", format!("Bearer {secret}"))
                .body(Body::from(body.to_string()))
                .unwrap();
            let res = app.oneshot(req).await.unwrap();
            let status = res.status();
            let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
                .await
                .unwrap();
            (
                status,
                serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null),
            )
        }
    };
    // This test's revisions are unique: the database is shared with other runs of the same container.
    let pid = std::process::id();
    let rev = |n: u32| format!("{:040x}", (u64::from(pid) << 8) | u64::from(n));
    let path = format!("docs/superpowers/specs/pg-{pid}.md");
    let snap = |n: u32, ancestors: Vec<String>| {
        json!({"repo": "acme/widgets", "revision": rev(n), "ancestors": ancestors,
               "documents": [{"path": path, "body": "# PG\n"}]})
    };
    let first = rev(1);
    let prev: Option<(String,)> = sqlx::query_as(
        "select revision from repository_document_sync_state where repo = 'acme/widgets'",
    )
    .fetch_optional(&db)
    .await
    .unwrap();
    let mut ancestors = vec![first.clone()];
    ancestors.extend(prev.map(|p| p.0));
    let (status, out) = post(snap(1, ancestors)).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["repo"], "acme/widgets");
    // Actor in the transaction: the in_project edge was set by the caller, kind service:docs-widgets.
    let (by, kind): (String, String) = sqlx::query_as(
        "select e.created_by, e.confirmed_kind from graph.edge e join graph.document d on d.node_id = e.src \
         where d.repo = 'acme/widgets' and d.path = $1 and e.rel = 'in_project'",
    )
    .bind(&path)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(
        (by.as_str(), kind.as_str()),
        ("docs-widgets", "service:docs-widgets")
    );
    // Another repository: 403 before the database function; a stale revision 409; an invalid snapshot 422.
    let mut other = snap(2, vec![rev(2), first.clone()]);
    other["repo"] = json!("acme/apps");
    assert_eq!(post(other).await.0, StatusCode::FORBIDDEN);
    let (status, out) = post(snap(3, vec![rev(3)])).await;
    assert_eq!(
        (status, out["error"].as_str()),
        (StatusCode::CONFLICT, Some("stale"))
    );
    let (status, out) =
        post(json!({"repo": "acme/widgets", "revision": "bad", "ancestors": [], "documents": []}))
            .await;
    assert_eq!(
        (status, out["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_snapshot"))
    );
}

/// sqlx applies a skipped lower migration that arrived after a higher one (on main 0013 and 0014; a branch with 0015 or 0016 after 0017 is renumbered to 0018+).
#[tokio::test]
#[ignore = "needs HUB_TEST_DB_URI: sh tools/test-graph-pg.sh"]
async fn sqlx_applies_a_lower_migration_merged_later() {
    let uri = std::env::var("HUB_TEST_DB_URI").expect(IGNORE);
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&uri)
        .await
        .unwrap();
    let name = format!("hub_order_{}", std::process::id());
    sqlx::query(&format!("drop database if exists {name}"))
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query(&format!("create database {name}"))
        .execute(&admin)
        .await
        .unwrap();
    let (base, _) = uri.rsplit_once('/').unwrap();
    let db = PgPoolOptions::new()
        .max_connections(1)
        .connect(&format!("{base}/{name}"))
        .await
        .unwrap();
    let dir = std::env::temp_dir().join(format!("hub-migration-order-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("0001_a.sql"), "create table a (x int);").unwrap();
    std::fs::write(dir.join("0017_c.sql"), "create table c (x int);").unwrap();
    sqlx::migrate::Migrator::new(dir.as_path())
        .await
        .unwrap()
        .run(&db)
        .await
        .unwrap();
    std::fs::write(dir.join("0013_b.sql"), "create table b (x int);").unwrap();
    sqlx::migrate::Migrator::new(dir.as_path())
        .await
        .unwrap()
        .run(&db)
        .await
        .unwrap();
    let (n,): (i64,) =
        sqlx::query_as("select count(*) from _sqlx_migrations where version in (1, 13, 17)")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(n, 3);
    db.close().await;
    std::fs::remove_dir_all(&dir).unwrap();
    sqlx::query(&format!("drop database {name}"))
        .execute(&admin)
        .await
        .unwrap();
}

/// The instance row (step 2.3) on a separate database: the service does not start without the row, seeding twice gives the same row,
/// a different account is rejected and writes nothing, a time zone change goes through; the startup check runs as role hub.
#[tokio::test]
#[ignore = "needs HUB_TEST_DB_URI: sh tools/test-graph-pg.sh"]
async fn instance_seed_twice_and_the_service_check() {
    let uri = std::env::var("HUB_TEST_DB_URI").expect(IGNORE);
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&uri)
        .await
        .unwrap();
    let name = format!("hub_instance_{}", std::process::id());
    sqlx::query(&format!("drop database if exists {name}"))
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query(&format!("create database {name}"))
        .execute(&admin)
        .await
        .unwrap();
    let (base, _) = uri.rsplit_once('/').unwrap();
    let owner = PgPoolOptions::new()
        .max_connections(1)
        .connect(&format!("{base}/{name}"))
        .await
        .unwrap();
    migrations::migrator().unwrap().run(&owner).await.unwrap();
    let hub = PgPoolOptions::new()
        .max_connections(1)
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("set role hub").execute(conn).await?;
                Ok(())
            })
        })
        .connect(&format!("{base}/{name}"))
        .await
        .unwrap();
    let with = |account: &str, tz: &str| {
        let (account, tz) = (account.to_owned(), tz.to_owned());
        Config::from_lookup(move |k| match k {
            "HUB_ACCOUNT" => Some(account.clone()),
            "HUB_TIMEZONE" => Some(tz.clone()),
            _ => None,
        })
        .unwrap()
    };
    let row = || async {
        sqlx::query_as::<_, (String, String, Option<i32>, Option<String>, Option<Vec<String>>)>(
            "select account, timezone, mailbox_pending_limit, mailbox_pending_ttl::text, document_roots \
             from public.instance",
        )
        .fetch_all(&owner)
        .await
        .unwrap()
    };

    let err = format!("{:#}", install().check_instance(&hub).await.unwrap_err());
    assert!(err.contains("no row"), "{err}");

    migrations::seed_instance(&owner, &install()).await.unwrap();
    let first = row().await;
    assert_eq!(
        first,
        [(
            "demo".to_owned(),
            "Europe/Helsinki".to_owned(),
            Some(20),
            None,
            Some(vec!["docs/superpowers".to_owned()])
        )]
    );
    migrations::seed_instance(&owner, &install()).await.unwrap();
    assert_eq!(row().await, first, "second seed changes nothing");
    install().check_instance(&hub).await.unwrap();

    let err = format!(
        "{:#}",
        migrations::seed_instance(&owner, &with("acme", "UTC"))
            .await
            .unwrap_err()
    );
    assert!(err.contains("HUB_ACCOUNT differs"), "{err}");
    assert_eq!(row().await, first, "refused seed writes nothing");
    let err = format!(
        "{:#}",
        with("acme", "Europe/Helsinki")
            .check_instance(&hub)
            .await
            .unwrap_err()
    );
    assert!(err.contains("belongs to account \"demo\""), "{err}");
    let err = format!(
        "{:#}",
        with("demo", "Mars/Olympus")
            .check_instance(&hub)
            .await
            .unwrap_err()
    );
    assert!(err.contains("not a time zone Postgres knows"), "{err}");
    let err = format!(
        "{:#}",
        migrations::seed_instance(&owner, &with("demo", "Mars/Olympus"))
            .await
            .unwrap_err()
    );
    assert!(err.contains("not a time zone Postgres knows"), "{err}");
    // Postgres accepts an abbreviation and a POSIX offset in `at time zone`, but they are not in pg_timezone_names.
    for bad in ["MSK", "UTC+3"] {
        let err = format!(
            "{:#}",
            with("demo", bad).check_instance(&hub).await.unwrap_err()
        );
        assert!(
            err.contains("not a time zone Postgres knows"),
            "{bad}: {err}"
        );
        let err = format!(
            "{:#}",
            migrations::seed_instance(&owner, &with("demo", bad))
                .await
                .unwrap_err()
        );
        assert!(
            err.contains("not a time zone Postgres knows"),
            "{bad}: {err}"
        );
    }

    migrations::seed_instance(&owner, &with("demo", "UTC"))
        .await
        .unwrap();
    assert_eq!(row().await[0].1, "UTC");
    // The service with the previous env starts: the Job changes the time zone, the service only warns.
    install().check_instance(&hub).await.unwrap();

    hub.close().await;
    owner.close().await;
    sqlx::query(&format!("drop database {name}"))
        .execute(&admin)
        .await
        .unwrap();
}

/// Installation time zone in the plan and the queue (step 2.5) on a separate database: at a day boundary UTC and Europe/Helsinki give
/// different todays, and with Europe/Helsinki local time and day match an explicit zone literal, including at daylight
/// saving transitions. Queries run as role hub.
#[tokio::test]
#[ignore = "needs HUB_TEST_DB_URI: sh tools/test-graph-pg.sh"]
async fn today_follows_the_instance_timezone() {
    let uri = std::env::var("HUB_TEST_DB_URI").expect(IGNORE);
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&uri)
        .await
        .unwrap();
    let name = format!("hub_timezone_{}", std::process::id());
    sqlx::query(&format!("drop database if exists {name}"))
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query(&format!("create database {name}"))
        .execute(&admin)
        .await
        .unwrap();
    let (base, _) = uri.rsplit_once('/').unwrap();
    let owner = PgPoolOptions::new()
        .max_connections(1)
        .connect(&format!("{base}/{name}"))
        .await
        .unwrap();
    migrations::migrator().unwrap().run(&owner).await.unwrap();
    let hub = PgPoolOptions::new()
        .max_connections(1)
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("set role hub").execute(conn).await?;
                Ok(())
            })
        })
        .connect(&format!("{base}/{name}"))
        .await
        .unwrap();
    let tz = |tz: &str| {
        let tz = tz.to_owned();
        Config::from_lookup(move |k| match k {
            "HUB_ACCOUNT" => Some("demo".into()),
            "HUB_TIMEZONE" => Some(tz.clone()),
            _ => None,
        })
        .unwrap()
    };
    // 22:30 UTC on 8 January: in Helsinki (UTC+2) it is already 00:30 on 9 January, the nightly mark at 00:05 has passed.
    let edge: chrono::DateTime<chrono::Utc> = "2030-01-08T22:30:00Z".parse().unwrap();
    let day = |hub: PgPool, at: chrono::DateTime<chrono::Utc>| async move {
        let (d, t): (chrono::NaiveDate, chrono::NaiveDateTime) = sqlx::query_as(concat!(
            "select ",
            local_date!("$1::timestamptz"),
            ", ",
            local_time!("$1::timestamptz"),
            "::timestamp"
        ))
        .bind(at)
        .fetch_one(&hub)
        .await
        .unwrap();
        (d, t)
    };

    // Without the instance row the query fails (HUBIN) instead of using the server time zone.
    let err = sqlx::query(plan::LOCAL_NOW)
        .execute(&hub)
        .await
        .unwrap_err();
    assert_eq!(
        err.as_database_error().and_then(|d| d.code()).as_deref(),
        Some("HUBIN"),
        "{err}"
    );

    migrations::seed_instance(&owner, &tz("UTC")).await.unwrap();
    let (utc_day, utc_now) = day(hub.clone(), edge).await;
    assert_eq!(
        utc_day,
        chrono::NaiveDate::from_ymd_opt(2030, 1, 8).unwrap()
    );
    assert!(!plan::nightly_due(utc_now, Some(utc_day)));

    migrations::seed_instance(&owner, &tz("Europe/Helsinki"))
        .await
        .unwrap();
    let (local_day, local_now) = day(hub.clone(), edge).await;
    assert_eq!(
        local_day,
        chrono::NaiveDate::from_ymd_opt(2030, 1, 9).unwrap()
    );
    assert_ne!(utc_day, local_day, "the day boundary differs by time zone");
    assert!(plan::nightly_due(local_now, Some(utc_day)));

    // With Europe/Helsinki everything matches an explicit literal: now() is one per transaction, moments around midnight and the
    // daylight saving transitions (31.03.2030 and 27.10.2030) give the same day and time.
    let mut t = hub.begin().await.unwrap();
    let (now,): (chrono::NaiveDateTime,) = sqlx::query_as(plan::LOCAL_NOW)
        .fetch_one(&mut *t)
        .await
        .unwrap();
    let (was,): (chrono::NaiveDateTime,) =
        sqlx::query_as("select (now() at time zone 'Europe/Helsinki')::timestamp")
            .fetch_one(&mut *t)
            .await
            .unwrap();
    t.rollback().await.unwrap();
    assert_eq!(now, was);
    for at in [
        "2030-01-08T21:59:59Z",
        "2030-01-08T22:00:00Z",
        "2030-03-31T00:59:59Z",
        "2030-03-31T01:00:00Z",
        "2030-03-31T21:00:00Z",
        "2030-10-26T21:00:00Z",
        "2030-10-27T00:59:59Z",
        "2030-10-27T01:00:00Z",
    ] {
        let at: chrono::DateTime<chrono::Utc> = at.parse().unwrap();
        let (d, t) = day(hub.clone(), at).await;
        let (was_d, was_t): (chrono::NaiveDate, chrono::NaiveDateTime) = sqlx::query_as(
            "select ($1::timestamptz at time zone 'Europe/Helsinki')::date, \
             ($1::timestamptz at time zone 'Europe/Helsinki')::timestamp",
        )
        .bind(at)
        .fetch_one(&hub)
        .await
        .unwrap();
        assert_eq!((d, t), (was_d, was_t), "{at}");
    }

    // Real plan and queue queries. done_on: tz-log is closed by the journal at the day boundary (updated_at today,
    // so that a rollback of the journal branch would be visible), tz-upd without a journal row is closed by updated_at at the same boundary.
    // READY: start_not_before of tz-ready equals today in UTC+14; in UTC-12 today is always earlier, the ticket is not ready.
    let mut t = tx::begin(&owner, &tx::Actor::seed("seed:test"))
        .await
        .unwrap();
    sqlx::query(
        "insert into tickets(project, code, summary, status, updated_at) values \
         ('apps', 'tz-log', 'tz-log', 'done', now()), ('apps', 'tz-upd', 'tz-upd', 'done', $1), \
         ('apps', 'tz-ready', 'tz-ready', 'open', now())",
    )
    .bind(edge)
    .execute(&mut *t)
    .await
    .unwrap();
    sqlx::query(
        "insert into ticket_change(ticket_seq, field, old, new, actor, actor_kind, at) \
         select seq, 'status', '\"open\"', '\"done\"', 'seed:test', 'seed', $1 from tickets where code = 'tz-log'",
    )
    .bind(edge)
    .execute(&mut *t)
    .await
    .unwrap();
    sqlx::query(
        "insert into plan.ticket_estimate(ticket_seq, effort_sp, start_not_before) \
         select seq, 1, (now() at time zone 'Etc/GMT-14')::date from tickets where code = 'tz-ready'",
    )
    .execute(&mut *t)
    .await
    .unwrap();
    t.commit().await.unwrap();
    let done_on = |hub: PgPool| async move {
        let mut out = Vec::new();
        for code in ["tz-log", "tz-upd"] {
            let rows: Vec<plan::TaskRow> = sqlx::query_as(plan::TASKS_SQL)
                .bind(
                    sqlx::query_scalar::<_, i64>("select seq from tickets where code = $1")
                        .bind(code)
                        .fetch_one(&hub)
                        .await
                        .unwrap(),
                )
                .fetch_all(&hub)
                .await
                .unwrap();
            assert_eq!(rows.len(), 1, "{code}");
            out.push(rows[0].done_on);
        }
        out
    };
    let ready = |hub: PgPool| async move {
        sqlx::query_scalar::<_, i64>(&format!(
            "select count(*) from tickets where code = 'tz-ready'{}",
            query::READY
        ))
        .fetch_one(&hub)
        .await
        .unwrap()
    };
    let d24 = chrono::NaiveDate::from_ymd_opt(2030, 1, 8);
    let d25 = chrono::NaiveDate::from_ymd_opt(2030, 1, 9);
    assert_eq!(
        done_on(hub.clone()).await,
        vec![d25, d25],
        "Europe/Helsinki"
    );
    migrations::seed_instance(&owner, &tz("UTC")).await.unwrap();
    assert_eq!(done_on(hub.clone()).await, vec![d24, d24], "UTC");
    migrations::seed_instance(&owner, &tz("Etc/GMT-14"))
        .await
        .unwrap();
    assert_eq!(
        ready(hub.clone()).await,
        1,
        "UTC+14: start_not_before is today"
    );
    migrations::seed_instance(&owner, &tz("Etc/GMT+12"))
        .await
        .unwrap();
    assert_eq!(
        ready(hub.clone()).await,
        0,
        "UTC-12: start_not_before is later"
    );

    hub.close().await;
    owner.close().await;
    sqlx::query(&format!("drop database {name}"))
        .execute(&admin)
        .await
        .unwrap();
}

/// Schema check at service startup: as role hub it passes on a database with the migrations applied and
/// refuses if the image has a migration missing from the database, or an applied one changed its checksum.
#[tokio::test]
#[ignore = "needs HUB_TEST_DB_URI: sh tools/test-graph-pg.sh"]
async fn service_verifies_the_schema_and_refuses_a_lagging_one() {
    let db = db().await;
    migrations::verify(&db, &migrations::migrator().unwrap())
        .await
        .unwrap();

    let dir = std::env::temp_dir().join(format!("hub-migration-verify-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for x in migrations::migrator().unwrap().iter() {
        let name = format!("{:04}_{}.sql", x.version, x.description.replace(' ', "_"));
        std::fs::write(dir.join(name), x.sql.as_bytes()).unwrap();
    }
    std::fs::write(dir.join("99990001_next.sql"), "create table next (x int);").unwrap();
    let ahead = sqlx::migrate::Migrator::new(dir.as_path()).await.unwrap();
    let err = format!("{:#}", migrations::verify(&db, &ahead).await.unwrap_err());
    assert!(err.contains("missing [99990001]"), "{err}");

    std::fs::remove_file(dir.join("99990001_next.sql")).unwrap();
    // The lowest version: 0001 of the chain, 1000 of the baseline build.
    let low = migrations::migrator()
        .unwrap()
        .iter()
        .map(|x| x.version)
        .min()
        .unwrap();
    let first = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(&format!("{low:04}_"))
        })
        .unwrap();
    let mut sql = std::fs::read_to_string(&first).unwrap();
    sql.push_str("\n-- edited\n");
    std::fs::write(&first, sql).unwrap();
    let edited = sqlx::migrate::Migrator::new(dir.as_path()).await.unwrap();
    let err = format!("{:#}", migrations::verify(&db, &edited).await.unwrap_err());
    assert!(err.contains(&format!("checksum changed [{low}]")), "{err}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `sql` as role `role` on one owner connection: `Ok(rows)` or `Err(SQLSTATE)`.
async fn as_role(c: &mut sqlx::PgConnection, role: &str, sql: &str) -> Result<u64, String> {
    sqlx::query(&format!("set role {role}"))
        .execute(&mut *c)
        .await
        .unwrap();
    let r = sqlx::query(sql).execute(&mut *c).await;
    sqlx::query("reset role").execute(&mut *c).await.unwrap();
    r.map(|r| r.rows_affected()).map_err(|e| {
        e.as_database_error()
            .and_then(|d| d.code())
            .map_or_else(|| e.to_string(), |c| c.into_owned())
    })
}

/// Role boundaries: hub_web neither writes nor reads hub tables, hub does not see the auth schema and does not change the schema;
/// privileges on objects of future migrations arrive by themselves (ALTER DEFAULT PRIVILEGES of the migration role).
#[tokio::test]
#[ignore = "needs HUB_TEST_DB_URI: sh tools/test-graph-pg.sh"]
async fn roles_keep_hub_and_hub_web_apart() {
    db().await.close().await;
    let owner = owner().await;
    let mut c = owner.acquire().await.unwrap();
    let denied = Err("42501".to_owned());
    let id = format!("t15k{}", std::process::id());

    for sql in [
        "insert into public.tickets(project, code, summary) values ('widgets', 'x15k', 'x')"
            .to_owned(),
        "select count(*) from public.tickets".to_owned(),
        "update public.tickets set summary = summary".to_owned(),
        "delete from public.tickets".to_owned(),
        "select count(*) from graph.node".to_owned(),
        "select count(*) from plan.dirty".to_owned(),
        "select count(*) from mailbox.keys".to_owned(),
        "select mailbox.exchange('k', 'alice', 'a', null, now(), false)".to_owned(),
    ] {
        assert_eq!(
            as_role(&mut c, "hub_web", &sql).await,
            denied,
            "hub_web: {sql}"
        );
    }
    // hub_web may do everything Better Auth does with its own tables: insert, read, update, delete.
    for sql in [
        format!(
            r#"insert into auth."user"(id, name, email, "emailVerified") values ('{id}', 'n', '{id}@test', true)"#
        ),
        format!(
            r#"insert into auth.session(id, "expiresAt", token, "updatedAt", "userId")
               values ('{id}', now() + interval '1 day', '{id}', now(), '{id}')"#
        ),
        format!(r#"update auth.session set "updatedAt" = now() where id = '{id}'"#),
        format!(
            r#"insert into auth.jwks(id, "publicKey", "privateKey", "createdAt") values ('{id}', 'p', 'k', now())"#
        ),
    ] {
        assert_eq!(
            as_role(&mut c, "hub_web", &sql).await,
            Ok(1),
            "hub_web: {sql}"
        );
    }

    for sql in [
        "select count(*) from auth.session",
        "select count(*) from auth.jwks",
        r#"select count(*) from auth."user""#,
        r#"insert into auth.verification(id, identifier, value, "expiresAt") values ('v', 'i', 'v', now())"#,
        "create table public.hub_ddl (x int)",
        "drop table public.tickets",
        "alter table public.tickets add column x int",
        "truncate public.ticket_change",
        "delete from public._sqlx_migrations",
    ] {
        assert_eq!(as_role(&mut c, "hub", sql).await, denied, "hub: {sql}");
    }
    assert_eq!(
        as_role(
            &mut c,
            "hub",
            "select 1 from public._sqlx_migrations limit 1"
        )
        .await,
        Ok(1)
    );

    // Objects of the next migration (created by the owner): privileges arrive without new GRANTs.
    for sql in [
        format!("create table auth.{id} (x int)"),
        format!("create table public.{id} (x serial)"),
    ] {
        sqlx::query(&sql).execute(&mut *c).await.unwrap();
    }
    assert_eq!(
        as_role(
            &mut c,
            "hub_web",
            &format!("insert into auth.{id} values (1)")
        )
        .await,
        Ok(1)
    );
    assert_eq!(
        as_role(&mut c, "hub", &format!("insert into auth.{id} values (1)")).await,
        denied
    );
    assert_eq!(
        as_role(
            &mut c,
            "hub",
            &format!("insert into public.{id} default values")
        )
        .await,
        Ok(1)
    );
    assert_eq!(
        as_role(
            &mut c,
            "hub_web",
            &format!("select count(*) from public.{id}")
        )
        .await,
        denied
    );

    for sql in [
        format!("drop table auth.{id}"),
        format!("drop table public.{id}"),
        format!(r#"delete from auth.jwks where id = '{id}'"#),
        format!(r#"delete from auth."user" where id = '{id}'"#),
    ] {
        sqlx::query(&sql).execute(&mut *c).await.unwrap();
    }
    let (n,): (i64,) = sqlx::query_as(&format!(
        "select count(*) from auth.session where id = '{id}'"
    ))
    .fetch_one(&mut *c)
    .await
    .unwrap();
    assert_eq!(n, 0, "session goes with its user (on delete cascade)");
}

/// A request from a human or an agent with the account `user:<user>` and, optionally, a fresh sign-in.
async fn as_user(
    app: &Router,
    method: &str,
    uri: &str,
    kind: &str,
    user: &str,
    fresh: bool,
    body: Value,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header(tx::ACTOR_KIND_HEADER, kind)
        .header(tx::ACTOR_HEADER, format!("user:{user}"));
    if fresh {
        req = req.header(tx::FRESH_HEADER, "1");
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Mailbox MCP exchange from an account (like hub-web for the mailbox tool).
async fn mcp_exchange(app: &Router, user: &str, agent: &str) -> (StatusCode, Value) {
    call(
        app,
        "POST",
        "/api/mailbox/exchange-user",
        "agent",
        json!({"user_id": user, "agent": agent, "msg": "hi"}),
    )
    .await
}

async fn bind(
    app: &Router,
    user: &str,
    kind: &str,
    fresh: bool,
    handle: &str,
) -> (StatusCode, Value) {
    as_user(
        app,
        "POST",
        "/api/people/logins",
        kind,
        user,
        fresh,
        json!({ "handle": handle }),
    )
    .await
}

fn error_of(r: &(StatusCode, Value)) -> (StatusCode, String) {
    (r.0, r.1["error"].as_str().unwrap_or("").to_owned())
}

/// Binding a sign-in to a person (only a human with a fresh sign-in, one live per account and per person),
/// the MCP key owner per binding in transitional mode (three cases) and mailbox key approval only by the owner.
#[tokio::test]
#[ignore = "needs HUB_TEST_DB_URI: sh tools/test-graph-pg.sh"]
async fn logins_own_mailbox_keys() {
    let db = db().await;
    let app = app(db.clone(), callers::Callers::default());
    // A new installation expects no legacy mailbox import, so the mailbox is open without one.
    let expected: Option<String> =
        sqlx::query_scalar("select mailbox_legacy_import from public.instance where id")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(expected, None);
    let pid = std::process::id();
    let (a, b) = (format!("la{pid}"), format!("lb{pid}"));
    let (ua, ub, uc) = (format!("ua{pid}"), format!("ub{pid}"), format!("uc{pid}"));
    new_person(&app, &a).await;
    new_person(&app, &b).await;
    let forbidden = |e: &str| (StatusCode::FORBIDDEN, e.to_owned());

    // Before bindings the account works as before: owner from the prefix.
    assert_eq!(
        mcp_exchange(&app, &ua, &format!("{b}-x")).await.0,
        StatusCode::OK
    );

    // Binding: only a human, only with a fresh sign-in.
    let r = bind(&app, &ua, "human", false, &a).await;
    assert_eq!(error_of(&r), forbidden("fresh_session_required"));
    let r = bind(&app, &ua, "agent", true, &a).await;
    assert_eq!(error_of(&r), forbidden("requires_human"));
    let r = bind(&app, &ua, "human", true, "nobody").await;
    assert_eq!(
        error_of(&r),
        (StatusCode::NOT_FOUND, "unknown_person".into())
    );
    let r = bind(&app, &ua, "human", true, &a).await;
    assert_eq!(r.0, StatusCode::CREATED, "{}", r.1);
    assert_eq!(r.1["user_id"], json!(ua));
    assert_eq!(r.1["handle"], json!(a));
    let r = bind(&app, &ua, "human", true, &b).await;
    assert_eq!(error_of(&r), (StatusCode::CONFLICT, "already_bound".into()));
    let r = bind(&app, &ub, "human", true, &a).await;
    assert_eq!(error_of(&r), (StatusCode::CONFLICT, "handle_taken".into()));
    // The binding took over the account's MCP key created with another prefix.
    let (owner,): (String,) = sqlx::query_as("select owner from mailbox.keys where id = $1")
        .bind(format!("user:{ua}"))
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(owner, a);

    // An account with a binding: the prefix must be its handle.
    let r = mcp_exchange(&app, &ua, &format!("{b}-x")).await;
    assert_eq!(error_of(&r), forbidden("agent_owner_mismatch"));
    assert_eq!(r.1["login"], json!(a));
    assert_eq!(
        mcp_exchange(&app, &ua, &format!("{a}-x")).await.0,
        StatusCode::OK
    );
    // An account without a binding, the prefix is bound to another account.
    let r = mcp_exchange(&app, &uc, &format!("{a}-y")).await;
    assert_eq!(error_of(&r), forbidden("agent_owner_mismatch"));
    // An account without a binding, the prefix is free: as before.
    assert_eq!(
        mcp_exchange(&app, &uc, &format!("{b}-y")).await.0,
        StatusCode::OK
    );

    let logins_of = |u: &str| format!("/api/people/logins?user_id={u}");
    let (s, logins) = as_user(
        &app,
        "GET",
        &logins_of(&ua),
        "human",
        &ua,
        false,
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(logins.as_array().unwrap().len(), 1);
    assert_eq!(logins[0]["handle"], json!(a));

    // Owner b's curl key: only b approves it, by binding with a fresh sign-in.
    let curl_key = |agent: String, key: String| {
        let app = app.clone();
        async move {
            let res = app
                .oneshot(
                    Request::post("/api/mailbox/exchange")
                        .header("content-type", "application/json")
                        .body(Body::from(
                            json!({"agent": agent, "key": key, "msg": "hi"}).to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK);
            crate::mailbox::key_id(&key).unwrap()
        }
    };
    let key = format!("k{pid}{}", "x".repeat(32));
    let id = curl_key(format!("{b}-c"), key).await;
    let path = format!("/api/mailbox/keys/{id}");
    let approve = json!({"status": "approved"});
    let r = as_user(&app, "PATCH", &path, "human", &ua, true, approve.clone()).await;
    assert_eq!(error_of(&r), forbidden("not_key_owner"));
    let r = as_user(&app, "PATCH", &path, "human", &ub, true, approve.clone()).await;
    assert_eq!(error_of(&r), forbidden("no_login"));
    assert_eq!(
        bind(&app, &ub, "human", true, &b).await.0,
        StatusCode::CREATED
    );
    let r = as_user(&app, "PATCH", &path, "human", &ub, false, approve.clone()).await;
    assert_eq!(error_of(&r), forbidden("fresh_session_required"));
    let r = as_user(&app, "PATCH", &path, "agent", &ub, true, approve.clone()).await;
    assert_eq!(error_of(&r), forbidden("requires_human"));
    let r = as_user(&app, "PATCH", &path, "human", &ub, true, approve.clone()).await;
    assert_eq!(r.0, StatusCode::OK, "{}", r.1);
    assert_eq!(r.1["status"], json!("approved"));
    let r = as_user(
        &app,
        "PATCH",
        "/api/mailbox/keys/sha256:00",
        "human",
        &ub,
        true,
        approve,
    )
    .await;
    assert_eq!(error_of(&r), (StatusCode::NOT_FOUND, "no_such_key".into()));
    let (actor,): (String,) = sqlx::query_as(
        "select actor from mailbox.key_review where key_id = $1 order by id desc limit 1",
    )
    .bind(&id)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(actor, format!("user:{ub}"));

    // A pending key of an owner without a binding (outside the graph): it cannot be approved, any bound human with a fresh
    // sign-in may revoke it, with a journal record. Otherwise junk would occupy the shared pending limit forever.
    let stray = curl_key(format!("z{pid}-c"), format!("s{pid}{}", "y".repeat(32))).await;
    let stray_path = format!("/api/mailbox/keys/{stray}");
    let revoke = json!({"status": "revoked"});
    let r = as_user(
        &app,
        "PATCH",
        &stray_path,
        "human",
        &ua,
        true,
        json!({"status": "approved"}),
    )
    .await;
    assert_eq!(error_of(&r), forbidden("not_key_owner"));
    let r = as_user(
        &app,
        "PATCH",
        &stray_path,
        "human",
        &ua,
        false,
        revoke.clone(),
    )
    .await;
    assert_eq!(error_of(&r), forbidden("fresh_session_required"));
    let r = as_user(
        &app,
        "PATCH",
        &stray_path,
        "human",
        &uc,
        true,
        revoke.clone(),
    )
    .await;
    assert_eq!(error_of(&r), forbidden("no_login"));
    let r = as_user(
        &app,
        "PATCH",
        &stray_path,
        "human",
        &ua,
        true,
        revoke.clone(),
    )
    .await;
    assert_eq!(r.0, StatusCode::OK, "{}", r.1);
    assert_eq!(r.1["status"], json!("revoked"));
    assert_eq!(r.1["owner_bound"], json!(false));
    let (actor,): (String,) = sqlx::query_as(
        "select actor from mailbox.key_review where key_id = $1 order by id desc limit 1",
    )
    .bind(&stray)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(actor, format!("user:{ua}"));
    // A revoked key is no longer pending: a non-owner cannot restore it.
    let r = as_user(
        &app,
        "PATCH",
        &stray_path,
        "human",
        &ua,
        true,
        revoke.clone(),
    )
    .await;
    assert_eq!(error_of(&r), forbidden("not_key_owner"));
    // Another person does not revoke a pending key of the bound owner b.
    let own = curl_key(format!("{b}-d"), format!("o{pid}{}", "z".repeat(32))).await;
    let own_path = format!("/api/mailbox/keys/{own}");
    let r = as_user(&app, "PATCH", &own_path, "human", &ua, true, revoke).await;
    assert_eq!(error_of(&r), forbidden("not_key_owner"));
    let (_, keys) = as_user(
        &app,
        "GET",
        "/api/mailbox/keys",
        "human",
        &ua,
        false,
        Value::Null,
    )
    .await;
    let bound_of = |id: &str| {
        keys.as_array()
            .unwrap()
            .iter()
            .find(|k| k["id"] == json!(id))
            .map(|k| k["owner_bound"].clone())
    };
    assert_eq!(bound_of(&own), Some(json!(true)));
    assert_eq!(bound_of(&stray), Some(json!(false)));

    // Retiring a binding: any human with a fresh sign-in; afterwards the account is again without a binding.
    let retire = format!("/api/people/logins/{a}/retire");
    let r = as_user(&app, "POST", &retire, "human", &ub, false, Value::Null).await;
    assert_eq!(error_of(&r), forbidden("fresh_session_required"));
    let r = as_user(&app, "POST", &retire, "human", &ub, true, Value::Null).await;
    assert_eq!(r.0, StatusCode::OK, "{}", r.1);
    assert_eq!(r.1["retired_by"], json!(format!("user:{ub}")));
    let r = as_user(&app, "POST", &retire, "human", &ub, true, Value::Null).await;
    assert_eq!(error_of(&r), (StatusCode::NOT_FOUND, "no_login".into()));
    let (_, logins) = as_user(
        &app,
        "GET",
        &logins_of(&ua),
        "human",
        &ua,
        false,
        Value::Null,
    )
    .await;
    assert_eq!(logins, json!([]));
}

/// Tests that replay the migration chain itself (pg_tests_chain.rs, cfg `chain_tests` from build.rs): they exist only where the
/// chain and its history do.
#[cfg(chain_tests)]
// rustfmt resolves a module file whatever the cfg; without the skip `cargo fmt` fails in a tree without the file.
// tools/check.sh (part rust-lint) checks the file with rustfmt directly where it exists.
#[rustfmt::skip]
#[path = "pg_tests_chain.rs"]
mod chain;

/// `hub migrate --adopt` (steps 2.12c and 2.15, src/adopt.rs, docs/migrations.md): the baseline migrator explicitly, whatever
/// HUB_MIGRATIONS says. Each test has its own database built by the chain (or empty), so they exist where the chain does
/// (cfg `chain` from build.rs).
#[cfg(chain)]
mod adopt_tests {
    use sqlx::{PgPool, postgres::PgPoolOptions};

    use super::IGNORE;
    use crate::{
        adopt::{self, Outcome},
        migrations,
    };

    struct Scratch {
        admin: PgPool,
        name: String,
        db: PgPool,
    }

    impl Scratch {
        async fn new(tag: &str) -> Self {
            let uri = std::env::var("HUB_TEST_DB_URI").expect(IGNORE);
            let admin = PgPoolOptions::new()
                .max_connections(1)
                .connect(&uri)
                .await
                .unwrap();
            let name = format!("hub_adopt_{tag}_{}", std::process::id());
            sqlx::query(&format!("drop database if exists {name}"))
                .execute(&admin)
                .await
                .unwrap();
            sqlx::query(&format!("create database {name}"))
                .execute(&admin)
                .await
                .unwrap();
            let (base, _) = uri.rsplit_once('/').unwrap();
            let db = PgPoolOptions::new()
                .max_connections(2)
                .connect(&format!("{base}/{name}"))
                .await
                .unwrap();
            Self { admin, name, db }
        }

        /// A database built by the migration chain, like production before the baseline.
        async fn chain(tag: &str) -> Self {
            let s = Self::new(tag).await;
            migrations::chain().run(&s.db).await.unwrap();
            s
        }

        async fn adopt(&self, journal: &adopt::Journal) -> anyhow::Result<Outcome> {
            let mut c = self.db.acquire().await.unwrap();
            adopt::adopt(&mut c, journal).await
        }

        async fn catalog_diff(&self) -> Vec<String> {
            let mut c = self.db.acquire().await.unwrap();
            adopt::catalog_diff(&mut c).await.unwrap()
        }

        async fn exec(&self, sql: &str) {
            sqlx::query(sql).execute(&self.db).await.unwrap();
        }

        /// The journal as (version, description, success, checksum, installed_on).
        async fn journal(&self) -> Vec<(i64, String, bool, Vec<u8>, String)> {
            sqlx::query_as(
                "select version, description, success, checksum, installed_on::text \
                 from public._sqlx_migrations order by version",
            )
            .fetch_all(&self.db)
            .await
            .unwrap()
        }

        /// Adopt refuses with `want` in the error, and the journal stays without version 1000.
        async fn refused(&self, journal: &adopt::Journal, want: &str) {
            let err = format!("{:#}", self.adopt(journal).await.unwrap_err());
            assert!(err.contains(want), "{err}");
            assert!(
                self.journal().await.iter().all(|r| r.0 != 1000),
                "refused adopt wrote version 1000"
            );
        }

        async fn drop(self) {
            self.db.close().await;
            sqlx::query(&format!("drop database {}", self.name))
                .execute(&self.admin)
                .await
                .unwrap();
        }
    }

    /// HUB_ADOPT_CHECKSUMS of the chain this repository has (versions 1-14 and 17-29, like the production journal).
    fn expected() -> adopt::Journal {
        adopt::parse_checksums(&adopt::checksums_of(&migrations::chain())).unwrap()
    }

    #[tokio::test]
    #[ignore = "needs HUB_TEST_DB_URI: sh tools/test-graph-pg.sh"]
    async fn adopt_a_chain_database_and_a_repeat_changes_nothing() {
        let s = Scratch::chain("ok").await;
        let j = expected();
        assert!(j.contains_key(&14) && j.contains_key(&17) && !j.contains_key(&15));
        // Plain `hub migrate` of the baseline build does not run the baseline over the chain.
        let err = format!("{:#}", adopt::refuse_unadopted(&s.db).await.unwrap_err());
        assert!(err.contains("hub migrate --adopt"), "{err}");

        assert_eq!(s.adopt(&j).await.unwrap(), Outcome::Adopted);
        let rows = s.journal().await;
        let base = migrations::baseline();
        let base = base.iter().next().unwrap();
        let last = rows.last().unwrap();
        assert_eq!(
            (last.0, last.1.as_str(), last.2, last.3.as_slice()),
            (1000, "baseline", true, &*base.checksum)
        );
        assert_eq!(rows.len(), j.len() + 1);
        // After adopt the baseline migrator has nothing to do, and the service check passes.
        adopt::refuse_unadopted(&s.db).await.unwrap();
        migrations::baseline().run(&s.db).await.unwrap();
        migrations::verify(&s.db, &migrations::baseline())
            .await
            .unwrap();
        assert_eq!(s.journal().await, rows);

        assert_eq!(s.adopt(&j).await.unwrap(), Outcome::AlreadyAdopted);
        assert_eq!(s.journal().await, rows);
        s.drop().await;
    }

    #[tokio::test]
    #[ignore = "needs HUB_TEST_DB_URI: sh tools/test-graph-pg.sh"]
    async fn adopt_refuses_a_foreign_checksum() {
        let s = Scratch::chain("sum").await;
        let mut j = expected();
        j.get_mut(&7).unwrap()[0] ^= 1;
        s.refused(&j, "versions with a different checksum [7]")
            .await;
        s.drop().await;
    }

    #[tokio::test]
    #[ignore = "needs HUB_TEST_DB_URI: sh tools/test-graph-pg.sh"]
    async fn adopt_refuses_an_extra_version() {
        let s = Scratch::chain("extra").await;
        s.exec(
            "insert into public._sqlx_migrations (version, description, success, checksum, execution_time) \
             values (999, 'extra', true, '\\x00', 0)",
        )
        .await;
        s.refused(&expected(), "versions not in HUB_ADOPT_CHECKSUMS [999]")
            .await;
        s.drop().await;
    }

    #[tokio::test]
    #[ignore = "needs HUB_TEST_DB_URI: sh tools/test-graph-pg.sh"]
    async fn adopt_refuses_a_missing_version() {
        let s = Scratch::chain("missing").await;
        s.exec("delete from public._sqlx_migrations where version = 5")
            .await;
        s.refused(&expected(), "versions missing from the journal [5]")
            .await;
        s.drop().await;
    }

    #[tokio::test]
    #[ignore = "needs HUB_TEST_DB_URI: sh tools/test-graph-pg.sh"]
    async fn adopt_refuses_a_failed_version() {
        let s = Scratch::chain("failed").await;
        s.exec("update public._sqlx_migrations set success = false where version = 3")
            .await;
        s.refused(&expected(), "versions with success = false [3]")
            .await;
        s.drop().await;
    }

    #[tokio::test]
    #[ignore = "needs HUB_TEST_DB_URI: sh tools/test-graph-pg.sh"]
    async fn adopt_refuses_an_extra_column() {
        let s = Scratch::chain("column").await;
        s.exec("alter table public.tickets add column adopt_probe int")
            .await;
        let diff = s.catalog_diff().await;
        assert!(
            diff.iter()
                .any(|l| l.starts_with("+ column public.tickets ") && l.contains(" adopt_probe ")),
            "{diff:#?}"
        );
        s.refused(&expected(), "catalog: ").await;
        s.drop().await;
    }

    #[tokio::test]
    #[ignore = "needs HUB_TEST_DB_URI: sh tools/test-graph-pg.sh"]
    async fn adopt_refuses_a_different_function_comment() {
        // The baseline comments no function, so any comment on one differs from it.
        let s = Scratch::chain("comment").await;
        let (sig,): (String,) = sqlx::query_as(
            "select p.oid::regprocedure::text from pg_proc p join pg_namespace n on n.oid = p.pronamespace \
             where n.nspname = 'graph' order by 1 limit 1",
        )
        .fetch_one(&s.db)
        .await
        .unwrap();
        s.exec(&format!(
            "comment on function {sig} is 'a different comment'"
        ))
        .await;
        let diff = s.catalog_diff().await;
        assert_eq!(diff.len(), 1, "{diff:#?}");
        assert!(
            diff[0].starts_with("+ comment function graph.")
                && diff[0].ends_with(" = a different comment"),
            "{diff:#?}"
        );
        s.refused(&expected(), "catalog: 1 lines differ").await;
        s.drop().await;
    }

    /// A new installation of the baseline build: plain `hub migrate` applies the baseline as version 1000, whose catalog is the
    /// embedded one, and adopt has nothing to do.
    #[tokio::test]
    #[ignore = "needs HUB_TEST_DB_URI: sh tools/test-graph-pg.sh"]
    async fn a_new_database_gets_the_baseline_the_normal_way() {
        let s = Scratch::new("fresh").await;
        adopt::refuse_unadopted(&s.db).await.unwrap();
        migrations::baseline().run(&s.db).await.unwrap();
        let rows = s.journal().await;
        assert_eq!(
            rows.iter().map(|r| (r.0, r.2)).collect::<Vec<_>>(),
            vec![(1000, true)]
        );
        assert_eq!(s.catalog_diff().await, Vec::<String>::new());
        assert_eq!(s.adopt(&expected()).await.unwrap(), Outcome::AlreadyAdopted);
        assert_eq!(s.journal().await, rows);
        s.drop().await;
    }
}
