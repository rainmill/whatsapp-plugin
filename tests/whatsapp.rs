//! The plugin end to end against a stand-in for Meta: the mirror, what a send
//! refuses, a message through the queue, and what a delivery report changes.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use laterite_core::parameters::Parameters;
use laterite_core::query::{bind_values, build};
use laterite_core::queue::{JobReg, Queue, Services};
use laterite_core::sea_query::{Alias, Expr, Query};
use laterite_core::serde_json::{json, Value};
use laterite_core::sqlx;
use laterite_core::strata::async_trait;
use laterite_core::testing::connect_test;
use laterite_core::{AnyRowExt, Cache, Crypt, Db, Events, Module, Registry};
use rainmill_vault::{store, ItemInput};
use rainmill_whatsapp::graph::{Graph, GraphError, Method, Request};
use rainmill_whatsapp::{send, templates, Message, WhatsAppError, WhatsAppModule, KIND, SLOT};

/// Answers as Meta would and keeps what it was asked.
#[derive(Default)]
struct FakeMeta {
    asked: Mutex<Vec<Request>>,
    /// The status Meta answers a send with; 200 unless set.
    send_status: Mutex<u16>,
}

#[async_trait]
impl Graph for FakeMeta {
    async fn call(&self, request: Request) -> Result<Value, GraphError> {
        self.asked.lock().unwrap().push(request.clone());
        if request.path.starts_with("debug_token") {
            // An app's own token is its id and its secret.
            if request.token.contains('|') && request.token != "42|shh" {
                return Err(GraphError {
                    status: Some(400),
                    code: Some(190),
                    message: "Invalid OAuth access token signature.".to_string(),
                });
            }
            return Ok(
                json!({ "data": { "app_id": "42", "is_valid": true, "expires_at": 0,
                "scopes": ["whatsapp_business_messaging", "whatsapp_business_management"] } }),
            );
        }
        if request.path.contains("phone_numbers") {
            return Ok(json!({ "data": [
                { "id": "555", "display_phone_number": "+1 555 0100", "verified_name": "Acme" }
            ] }));
        }
        if request.path.contains("subscribed_apps") {
            if request.method == Method::Post {
                return Ok(json!({ "success": true }));
            }
            return Ok(json!({ "data": [] }));
        }
        if request.path.contains("message_templates") && request.method == Method::Post {
            return Ok(json!({ "id": "13", "status": "PENDING", "category": "UTILITY" }));
        }
        if request.path.contains("message_templates") && request.method == Method::Delete {
            return Ok(json!({ "success": true }));
        }
        if request.path.contains("message_templates") {
            return Ok(json!({ "data": [
                { "id": "11", "name": "order_ready", "language": "en", "status": "APPROVED", "category": "UTILITY",
                  "components": [
                    { "type": "BODY", "text": "Hi {{1}}, order {{2}} is ready.", "example": { "body_text": [["Asha", "A-100"]] } },
                    { "type": "BUTTONS", "buttons": [{ "type": "URL", "text": "Track", "url": "https://acme.test/o/{{1}}", "example": ["A-100"] }] }
                  ] },
                { "id": "12", "name": "big_sale", "language": "en", "status": "PENDING", "category": "MARKETING",
                  "components": [{ "type": "BODY", "text": "Sale today." }] }
            ], "paging": { "cursors": { "after": "x" } } }));
        }
        match *self.send_status.lock().unwrap() {
            0 | 200 => Ok(json!({ "messages": [{ "id": "wamid.ONE" }] })),
            status => Err(GraphError {
                status: Some(status),
                code: Some(131026),
                message: "Message undeliverable".to_string(),
            }),
        }
    }
}

async fn setup() -> (
    Db,
    laterite_core::testing::TestGuard,
    Arc<Crypt>,
    Queue,
    Arc<FakeMeta>,
) {
    let (db, guard) = connect_test(&[
        rainmill_vault::migrations::migrations(),
        rainmill_whatsapp::migrations::migrations(),
    ])
    .await;
    let crypt = Arc::new(Crypt::ephemeral());
    let id = store::create(
        &db,
        &crypt,
        KIND,
        &ItemInput {
            label: "Acme WhatsApp".to_string(),
            fields: BTreeMap::from([
                ("access_token".to_string(), "token-one".to_string()),
                ("phone_number_id".to_string(), "555".to_string()),
                ("business_account_id".to_string(), "777".to_string()),
            ]),
            allowance: None,
            period_day: 1,
            expires_at: None,
        },
    )
    .await
    .unwrap();
    store::bind(&db, SLOT, id, false).await.unwrap();

    let mut registry = Registry::new();
    registry.set_owner(WhatsAppModule.id());
    WhatsAppModule.register(&mut registry);
    let queue = Queue::inline(
        registry.take::<JobReg>(),
        Services {
            db: db.clone(),
            events: Events::new(db.clone()),
            cache: Cache::local(),
            parameters: Parameters::new(db.clone()),
        },
    );
    queue.set_crypt(crypt.clone());
    let meta = Arc::new(FakeMeta::default());
    rainmill_whatsapp::use_graph(meta.clone());
    (db, guard, crypt, queue, meta)
}

/// One column of a message's row, as text.
async fn column(db: &Db, id: i64, name: &str) -> String {
    let (sql, values) = build(
        db.backend,
        Query::select()
            .expr_as(
                Expr::col(Alias::new(name))
                    .cast_as(Alias::new(laterite_core::query::text_cast(db.backend))),
                Alias::new(name),
            )
            .from(Alias::new("whatsapp_messages"))
            .and_where(Expr::col(Alias::new("id")).eq(id))
            .to_owned(),
    );
    let row = bind_values(sqlx::query(&sql), values)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    row.get_text_opt(name).unwrap().unwrap_or_default()
}

// One test, in order: the stand-in for Meta is shared by the whole process.
#[tokio::test]
async fn templates_are_mirrored_and_a_message_is_sent_and_followed() {
    let (db, _guard, crypt, queue, meta) = setup().await;

    // The mirror holds what Meta reported, with the token and account asked.
    assert_eq!(templates::sync(&db, &crypt).await.unwrap(), 2);
    let list = templates::list(&db).await.unwrap();
    assert_eq!(
        list.iter()
            .map(|t| (t.name.as_str(), t.status.as_str()))
            .collect::<Vec<_>>(),
        vec![("big_sale", "PENDING"), ("order_ready", "APPROVED")]
    );
    {
        let asked = meta.asked.lock().unwrap();
        assert_eq!(asked[0].method, Method::Get);
        assert!(
            asked[0].path.starts_with("777/message_templates?"),
            "{}",
            asked[0].path
        );
        assert_eq!(asked[0].token, "token-one");
    }
    // A second sync changes nothing and adds no row.
    templates::sync(&db, &crypt).await.unwrap();
    assert_eq!(templates::list(&db).await.unwrap().len(), 2);

    // What a send refuses before anything is queued.
    let refused = |message: Message| {
        let (queue, db) = (queue.clone(), db.clone());
        async move {
            match send(&queue, &db, message).await {
                Err(WhatsAppError::Refused(why)) => why,
                other => panic!("expected a refusal, got {other:?}"),
            }
        }
    };
    let ready = || Message::template("order_ready", "en").to("+91 98450 12345");
    assert!(
        refused(Message::template("order_ready", "en").to("98450 12345"))
            .await
            .contains("country code")
    );
    assert!(
        refused(Message::template("missing", "en").to("+919845012345"))
            .await
            .contains("no template")
    );
    assert!(
        refused(Message::template("big_sale", "en").to("+919845012345"))
            .await
            .contains("not approved")
    );
    assert!(refused(ready().param("Asha").button("A-1"))
        .await
        .contains("takes 2 values"));
    assert!(refused(ready().param("Asha").param("A-1"))
        .await
        .contains("needs a button value"));

    // A message goes out with its values, is logged, and is counted as utility.
    let id = send(
        &queue,
        &db,
        ready()
            .param("Asha")
            .param("A-1")
            .button("A-1")
            .about("orders", 42),
    )
    .await
    .unwrap();
    assert_eq!(column(&db, id, "state").await, "sent");
    assert_eq!(column(&db, id, "wa_message_id").await, "wamid.ONE");
    assert_eq!(column(&db, id, "to_number").await, "+919845012345");
    assert_eq!(column(&db, id, "subject_type").await, "orders");
    {
        let asked = meta.asked.lock().unwrap();
        let last = asked.last().unwrap();
        assert_eq!(
            (last.method, last.path.as_str()),
            (Method::Post, "555/messages")
        );
        let body = last.body.as_ref().unwrap();
        assert_eq!(body["template"]["name"], "order_ready");
        assert_eq!(
            body["template"]["components"][0]["parameters"][1]["text"],
            "A-1"
        );
    }
    let today = chrono::Utc::now().date_naive();
    let usage = store::usage_between(&db, Some(SLOT), today, today)
        .await
        .unwrap();
    assert!(
        usage
            .iter()
            .any(|row| row.meter == "utility" && row.units == 1),
        "{usage:?}"
    );

    // Meta's reports move it on, record the charge, and never move it back.
    let report = |status: &str, extra: Value| {
        let mut entry = json!({ "id": "wamid.ONE", "status": status, "timestamp": "1750263773" });
        entry
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        json!({ "entry": [{ "changes": [{ "field": "messages", "value": { "statuses": [entry] } }] }] })
    };
    let pricing = json!({ "pricing": { "billable": true, "pricing_model": "PMP", "type": "regular", "category": "utility" } });
    assert_eq!(
        rainmill_whatsapp::webhook::apply(&db, &report("delivered", pricing))
            .await
            .unwrap(),
        1
    );
    assert_eq!(column(&db, id, "state").await, "delivered");
    assert_eq!(column(&db, id, "billable").await, "1");
    assert_eq!(column(&db, id, "pricing_category").await, "utility");
    assert_eq!(
        column(&db, id, "delivered_at").await,
        "2025-06-18T16:22:53Z"
    );
    rainmill_whatsapp::webhook::apply(&db, &report("read", json!({})))
        .await
        .unwrap();
    assert_eq!(column(&db, id, "state").await, "read");
    // A late report is no news, and a message we did not send is not ours.
    assert_eq!(
        rainmill_whatsapp::webhook::apply(&db, &report("sent", json!({})))
            .await
            .unwrap(),
        0
    );
    assert_eq!(column(&db, id, "state").await, "read");

    // A review result reaches the mirror; Meta writes the language its own way.
    let review = json!({ "entry": [{ "changes": [{ "field": "message_template_status_update", "value": {
        "event": "REJECTED", "message_template_name": "big_sale", "message_template_language": "en",
        "reason": "PROMOTIONAL" } }] }] });
    rainmill_whatsapp::webhook::apply(&db, &review)
        .await
        .unwrap();
    let sale = templates::named(&db, "big_sale", "en")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (sale.status.as_str(), sale.rejected_reason.as_deref()),
        ("REJECTED", Some("PROMOTIONAL"))
    );

    // A draft goes to Meta as components and is mirrored in review; a
    // deleted template leaves the mirror.
    let draft = templates::Draft {
        name: "table_ready".into(),
        category: "utility".into(),
        language: "en".into(),
        body: "Hi {{1}}, your table is ready.".into(),
        samples: vec!["Asha".into()],
        footer: "Acme".into(),
        ..templates::Draft::default()
    };
    templates::submit(&db, &crypt, &draft).await.unwrap();
    {
        let asked = meta.asked.lock().unwrap();
        let last = asked.last().unwrap();
        assert_eq!(
            (last.method, last.path.as_str()),
            (Method::Post, "777/message_templates")
        );
        let body = last.body.as_ref().unwrap();
        assert_eq!(body["category"], "UTILITY");
        assert_eq!(body["components"][0]["example"]["body_text"][0][0], "Asha");
        assert_eq!(body["components"][1]["type"], "FOOTER");
    }
    let mirrored = templates::named(&db, "table_ready", "en")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (mirrored.status.as_str(), mirrored.wa_id.as_str()),
        ("PENDING", "13")
    );
    let bad = templates::Draft {
        name: "Table Ready".into(),
        ..draft
    };
    assert!(matches!(
        templates::submit(&db, &crypt, &bad).await,
        Err(WhatsAppError::Refused(_))
    ));
    templates::delete(&db, &crypt, &mirrored).await.unwrap();
    assert!(templates::named(&db, "table_ready", "en")
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        meta.asked.lock().unwrap().last().unwrap().path,
        "777/message_templates?hsm_id=13&name=table_ready"
    );

    // A message Meta refuses is marked failed with Meta's code and words.
    *meta.send_status.lock().unwrap() = 400;
    let failed = send(
        &queue,
        &db,
        ready().param("Ravi").param("B-2").button("B-2"),
    )
    .await;
    let failed_id = match failed {
        Ok(id) => id,
        // An inline queue reports the job's failure to the caller.
        Err(_) => id + 1,
    };
    assert_eq!(column(&db, failed_id, "state").await, "failed");
    assert_eq!(column(&db, failed_id, "error_code").await, "131026");
    assert_eq!(
        column(&db, failed_id, "error_text").await,
        "Message undeliverable"
    );

    a_connection_test_tries_each_part(&meta).await;
}

/// The connection test tries each part and sends nothing.
async fn a_connection_test_tries_each_part(meta: &FakeMeta) {
    use rainmill_whatsapp::check::{run, subscribe, Outcome};
    let field = |name: &str| {
        match name {
            "access_token" => "token-one",
            "phone_number_id" => "555",
            "business_account_id" => "777",
            _ => "",
        }
        .to_string()
    };
    let before = meta.asked.lock().unwrap().len();
    let (checks, reached) = run(&field).await;
    assert!(reached, "Meta took the token for the account");
    let of = |label: &str| {
        checks
            .iter()
            .find(|c| c.label.source() == label)
            .map(|c| c.outcome.clone())
            .unwrap_or_else(|| panic!("no check named {label}"))
    };
    assert!(matches!(of("Access token"), Outcome::Passed(_)));
    assert_eq!(
        of("Phone number"),
        Outcome::Passed("+1 555 0100 (Acme).".to_string())
    );
    // Nothing stored for these two, and no app subscribed: each says so.
    assert!(matches!(of("App secret"), Outcome::Failed(_)));
    assert!(matches!(of("Webhook"), Outcome::Failed(_)));
    assert!(matches!(of("Delivery reports"), Outcome::Failed(_)));
    // Reads alone.
    {
        let asked = meta.asked.lock().unwrap();
        assert!(asked.len() > before);
        assert!(asked[before..].iter().all(|r| r.method == Method::Get));
    }
    // The one write the test offers: subscribing the app to the account.
    assert!(checks.iter().any(|c| c.subscribable));
    subscribe(&field).await.unwrap();
    let asked = meta.asked.lock().unwrap();
    let last = asked.last().unwrap();
    assert_eq!(last.method, Method::Post);
    assert_eq!(last.path, "777/subscribed_apps");
}
