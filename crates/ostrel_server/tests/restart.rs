//! Groundwork for AC-10: data of the app survives a restart of the server on a SQLite file.
//!
//! A server is started on a database file, data is committed through its connection and a
//! request runs; then the server is dropped and a new one is started on the same file. The
//! schema, the rows, a tombstone and the op log must all be there again.
//!
//! The full acceptance of AC-10 (data written through the generated `notes` client, the
//! server process stopped and started again, case N-12 of `examples/v0_2/notes/EXPECTED.md`)
//! needs the op path and the WebSocket transport. These tests check the server side of it
//! with the entries that exist today: the writes of case N-11, done on the server's own
//! connection, read back the same after the restart, and the stored versions and collection
//! entries carry on from where they were.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::task::{Context, Poll, Waker};

use ostrel_core::FileId;
use ostrel_db::api::{
    CollectionChange, CollectionState, Connection, DbError, Driver, Hlc, MapEntry, MigrationPlan,
    NewOp, OpId, Query, ReplicaId, RowId, SchemaHash, ServerSeq, SetChange, SetTag, Value, Write,
};
use ostrel_db_sqlite::SqliteDriver;
use ostrel_server::audit::MemoryAuditLog;
use ostrel_server::host::SystemClock;
use ostrel_server::identity::{Credentials, NoTokens};
use ostrel_server::task::{Outcome, Server};
use ostrel_vm::{Image, Limits};

/// Runs a future that never waits. Every future of the SQLite driver is ready on first poll.
fn block_on<T>(f: impl Future<Output = T>) -> T {
    let mut f = std::pin::pin!(f);
    let mut cx = Context::from_waker(Waker::noop());
    match f.as_mut().poll(&mut cx) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("sqlite future was not ready"),
    }
}

/// A path for the database file of the test `name`, with no file there yet.
fn fresh_file(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("ostrel_server_{name}.db"));
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut p = path.clone().into_os_string();
        p.push(suffix);
        let _ = std::fs::remove_file(p);
    }
    path
}

fn image(src: &str) -> Image {
    let file = FileId::from_raw(0);
    let (tokens, lexed) = ostrel_syntax::lex::lex(src, file);
    assert!(lexed.is_empty(), "{lexed:?}");
    let (module, parsed) = ostrel_syntax::parse::parse(src, file, &tokens, &lexed);
    assert!(parsed.is_empty(), "{parsed:?}");
    let (checked, diags) = ostrel_sema::check(&module, file, src);
    assert!(diags.is_empty(), "{diags:?}");
    let program = ostrel_ir::lower(&module, &checked, file).unwrap();
    ostrel_vm::compile(&program).unwrap()
}

const PROGRAM: &str = "fn main()\n  print(\"up\")\n";

/// Starts a server for `PROGRAM` on the SQLite file at `path`.
fn start(path: &Path) -> Server {
    let url = format!("sqlite:{}", path.display());
    let db = block_on(SqliteDriver::new().connect(&url)).unwrap();
    Server::new(
        image(PROGRAM),
        Limits::default(),
        db,
        Box::new(MemoryAuditLog::new()),
        Box::new(SystemClock),
        Box::new(NoTokens),
    )
}

fn plan() -> MigrationPlan {
    MigrationPlan {
        from: None,
        to: SchemaHash([7; 32]),
        schema: "{\"v\":7}".to_owned(),
        enums: vec![],
        steps: vec![],
    }
}

fn rid(n: u128) -> RowId {
    RowId::from_hex(&format!("{n:032x}")).unwrap()
}

fn insert(row: u128, title: &str) -> Write {
    Write::Insert {
        model: 1,
        row: rid(row),
        fields: vec![(0, Value::Text(title.to_owned()))],
        collections: vec![],
    }
}

fn serves_requests(server: &mut Server) {
    assert_eq!(
        server.run(&Credentials::anonymous()),
        Ok(Outcome {
            principal: None,
            output: vec!["up".to_owned()],
        })
    );
}

#[test]
fn restart_on_the_same_sqlite_file_keeps_schema_rows_and_ops() {
    let path = fresh_file("restart");
    let before = {
        let mut server = start(&path);
        serves_requests(&mut server);
        let db: &mut dyn Connection = server.connection();
        block_on(async {
            db.migrate(&plan()).await.unwrap();
            let mut tx = db.begin().await.unwrap();
            tx.apply(&[insert(1, "first"), insert(2, "second")])
                .await
                .unwrap();
            tx.apply(&[Write::Delete {
                model: 1,
                row: rid(2),
                expect_version: 1,
            }])
            .await
            .unwrap();
            let op = NewOp {
                id: OpId {
                    replica: ReplicaId(5),
                    seq: 1,
                },
                hlc: Hlc::new(11, 0, ReplicaId(5)).unwrap(),
                model: 1,
                row: rid(1),
                body: b"{}".to_vec(),
            };
            assert_eq!(tx.append_ops(&[op]).await, Ok(ServerSeq(1)));
            tx.commit().await.unwrap();
            db.query(&Query::all(1)).await.unwrap()
        })
        // The server and its connection are dropped here: the restart.
    };
    assert_eq!(before.0.len(), 1);

    let mut server = start(&path);
    serves_requests(&mut server);
    let db = server.connection();
    block_on(async {
        let schema = db.applied_schema().await.unwrap().unwrap();
        assert_eq!(schema.hash, SchemaHash([7; 32]));
        assert_eq!(db.query(&Query::all(1)).await, Ok(before));
        let mut tx = db.begin().await.unwrap();
        // The tombstone of row 2 still holds its id (AC-31) and the op log continues.
        assert_eq!(
            tx.apply(&[insert(2, "again")]).await,
            Err(DbError::Conflict)
        );
        tx.rollback().await.unwrap();
        let mut tx = db.begin().await.unwrap();
        assert_eq!(tx.append_ops(&[]).await, Ok(ServerSeq(1)));
        let ops = tx.ops_since(ServerSeq(0), 9).await.unwrap();
        assert_eq!(ops.len(), 1);
        tx.rollback().await.unwrap();
    });
}

/// Fields of `Note` in `examples/v0_2/notes/notes.ostl`, in declaration order.
const NOTE: u32 = 1;
const TITLE: u32 = 0;
const BODY: u32 = 1;
const PINNED: u32 = 2;

fn text(s: &str) -> Value {
    Value::Text(s.to_owned())
}

/// A new note as the runtime stores it: the defaults of `body` and `pinned` are written.
fn new_note(row: u128, title: &str) -> Write {
    Write::Insert {
        model: NOTE,
        row: rid(row),
        fields: vec![
            (TITLE, text(title)),
            (BODY, text("")),
            (PINNED, Value::Bool(false)),
        ],
        collections: vec![],
    }
}

fn edit(row: u128, expect_version: u64, field: u32, value: Value) -> Write {
    Write::Update {
        model: NOTE,
        row: rid(row),
        expect_version,
        fields: vec![(field, value)],
        collections: vec![],
    }
}

#[test]
fn restart_keeps_the_notes_of_case_n11() {
    const ALPHA: u128 = 0xa1;
    const BETA: u128 = 0xb2;
    let path = fresh_file("restart_notes");
    {
        let mut server = start(&path);
        let db = server.connection();
        // N-11: add Alpha and Beta, set the body of Beta to `x`, pin Beta, delete Alpha. Each
        // step is its own transaction, as each is its own action in the client.
        block_on(async {
            db.migrate(&plan()).await.unwrap();
            for w in [
                new_note(ALPHA, "Alpha"),
                new_note(BETA, "Beta"),
                edit(BETA, 1, BODY, text("x")),
                edit(BETA, 2, PINNED, Value::Bool(true)),
                Write::Delete {
                    model: NOTE,
                    row: rid(ALPHA),
                    expect_version: 1,
                },
            ] {
                let mut tx = db.begin().await.unwrap();
                tx.apply(&[w]).await.unwrap();
                tx.commit().await.unwrap();
            }
        });
        serves_requests(&mut server);
        // The server and its connection are dropped here: the restart (N-12).
    }

    let mut server = start(&path);
    serves_requests(&mut server);
    let db = server.connection();
    block_on(async {
        // Exactly one note: title Beta, body x, pinned.
        let rows = db.query(&Query::all(NOTE)).await.unwrap().0;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].id, rid(BETA));
        assert_eq!(rows[0].version, 3);
        assert_eq!(
            rows[0].fields,
            vec![
                (TITLE, text("Beta")),
                (BODY, text("x")),
                (PINNED, Value::Bool(true)),
            ]
        );
        assert!(rows[0].collections.is_empty());

        // Versions carry on: a stale version is refused, the stored one is accepted (N-09).
        let mut tx = db.begin().await.unwrap();
        assert_eq!(
            tx.apply(&[edit(BETA, 2, PINNED, Value::Bool(false))]).await,
            Err(DbError::VersionMismatch)
        );
        tx.rollback().await.unwrap();
        let mut tx = db.begin().await.unwrap();
        tx.apply(&[edit(BETA, 3, PINNED, Value::Bool(false))])
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let rows = db.query(&Query::all(NOTE)).await.unwrap().0;
        assert_eq!(rows[0].version, 4);
        assert_eq!(rows[0].fields[2], (PINNED, Value::Bool(false)));

        // The deleted note stays deleted and its id stays taken (AC-31).
        let mut tx = db.begin().await.unwrap();
        assert_eq!(
            tx.apply(&[edit(ALPHA, 2, TITLE, text("Alpha"))]).await,
            Err(DbError::NotFound)
        );
        tx.rollback().await.unwrap();
        let mut tx = db.begin().await.unwrap();
        assert_eq!(
            tx.apply(&[new_note(ALPHA, "Alpha")]).await,
            Err(DbError::Conflict)
        );
        tx.rollback().await.unwrap();
    });
}

#[test]
fn restart_keeps_set_and_map_entries() {
    const ROW: u128 = 0xc3;
    const TAGS: u32 = 3;
    const META: u32 = 4;
    let tag = |seq| OpId {
        replica: ReplicaId(5),
        seq,
    };
    let hlc = |ms| Hlc::new(ms, 0, ReplicaId(5)).unwrap();
    let entry = |key: &str, value: Option<&str>, ms| MapEntry {
        key: text(key),
        value: value.map(text),
        hlc: hlc(ms),
    };
    let path = fresh_file("restart_collections");
    let before = {
        let mut server = start(&path);
        let db = server.connection();
        block_on(async {
            db.migrate(&plan()).await.unwrap();
            let mut tx = db.begin().await.unwrap();
            tx.apply(&[Write::Insert {
                model: NOTE,
                row: rid(ROW),
                fields: vec![(TITLE, text("tagged"))],
                collections: vec![
                    (
                        TAGS,
                        CollectionChange::Set(vec![
                            SetChange::Add {
                                elem: text("a"),
                                tag: tag(1),
                            },
                            SetChange::Add {
                                elem: text("b"),
                                tag: tag(2),
                            },
                        ]),
                    ),
                    (
                        META,
                        CollectionChange::Map(vec![
                            entry("k", Some("v"), 10),
                            entry("gone", Some("w"), 10),
                        ]),
                    ),
                ],
            }])
            .await
            .unwrap();
            tx.apply(&[Write::Update {
                model: NOTE,
                row: rid(ROW),
                expect_version: 1,
                fields: vec![],
                collections: vec![
                    (
                        TAGS,
                        CollectionChange::Set(vec![SetChange::Remove {
                            elem: text("b"),
                            tag: tag(2),
                        }]),
                    ),
                    (META, CollectionChange::Map(vec![entry("gone", None, 11)])),
                ],
            }])
            .await
            .unwrap();
            tx.commit().await.unwrap();
            db.query(&Query::all(NOTE)).await.unwrap()
        })
        // The server and its connection are dropped here: the restart.
    };
    let row = &before.0[0];
    assert_eq!(row.version, 2);
    assert_eq!(
        row.collections,
        vec![
            (
                TAGS,
                CollectionState::Set(vec![SetTag {
                    elem: text("a"),
                    tag: tag(1),
                }])
            ),
            (
                META,
                CollectionState::Map(vec![entry("gone", None, 11), entry("k", Some("v"), 10)])
            ),
        ]
    );

    let mut server = start(&path);
    serves_requests(&mut server);
    let db = server.connection();
    block_on(async {
        assert_eq!(db.query(&Query::all(NOTE)).await, Ok(before));
        // The stored tags and entries are the base of later changes: a replayed remove of the
        // removed tag changes nothing, a new entry replaces the stored one of its key and the
        // removed key keeps its tombstone.
        let mut tx = db.begin().await.unwrap();
        tx.apply(&[Write::Update {
            model: NOTE,
            row: rid(ROW),
            expect_version: 2,
            fields: vec![],
            collections: vec![
                (
                    TAGS,
                    CollectionChange::Set(vec![SetChange::Remove {
                        elem: text("b"),
                        tag: tag(2),
                    }]),
                ),
                (
                    META,
                    CollectionChange::Map(vec![entry("k", Some("v2"), 12)]),
                ),
            ],
        }])
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let row = &db.query(&Query::all(NOTE)).await.unwrap().0[0];
        assert_eq!(row.version, 3);
        assert_eq!(
            row.collections[0],
            (
                TAGS,
                CollectionState::Set(vec![SetTag {
                    elem: text("a"),
                    tag: tag(1),
                }])
            )
        );
        assert_eq!(
            row.collections[1],
            (
                META,
                CollectionState::Map(vec![entry("gone", None, 11), entry("k", Some("v2"), 12)])
            )
        );
    });
}
