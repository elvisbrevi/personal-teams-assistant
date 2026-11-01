use super::*;

const VALUE: &str = "synthetic-delivered-value-5be1";
const TOKEN: &str = "synthetic-destination-token-77c0";
const ENTRY: &str = "aaaaaaaa-0000-4000-8000-000000000001";
const AUTH: &str = "aaaaaaaa-0000-4000-8000-000000000002";

fn http(body: Value) -> Destination {
    Destination {
        id: "blog-editor-openai".into(),
        description: "Blog editor: OpenAI key in Settings".into(),
        target: Target::Http {
            url: "https://editor.example.com/api/import/secrets/OPENAI_API_KEY".into(),
            method: "PUT".into(),
            headers: BTreeMap::from([("X-Blog-Editor".into(), "1".into())]),
            body,
            auth: Some(Auth {
                header: "Authorization".into(),
                prefix: "Bearer ".into(),
                variable: AUTH.into(),
            }),
            confirm_field: Some("/last4".into()),
        },
        variables: vec![ENTRY.into()],
        created_at: 1,
        approved_at: None,
        approved_digest: None,
    }
}
fn cloudflare() -> Destination {
    Destination {
        id: "blog-worker-github".into(),
        description: "Blog editor Worker: GitHub token".into(),
        target: Target::CloudflareWorkerSecret {
            account_id: "0123456789abcdef0123456789abcdef".into(),
            script: "blog-editor".into(),
            secret_name: "GITHUB_TOKEN".into(),
            token: AUTH.into(),
        },
        variables: vec![ENTRY.into()],
        created_at: 1,
        approved_at: None,
        approved_digest: None,
    }
}
fn with(mut destination: Destination, change: impl FnOnce(&mut Target)) -> Destination {
    change(&mut destination.target);
    destination
}

#[test]
fn definitions_fix_an_https_request_with_the_value_in_one_place() {
    assert!(validate(&http(json!({"value": PLACEHOLDER}))).is_ok());
    assert!(
        validate(&http(
            json!({"secret": {"text": PLACEHOLDER, "type": "plain"}})
        ))
        .is_ok()
    );
    assert!(validate(&cloudflare()).is_ok());
    let refused = |destination: Destination| {
        assert!(
            matches!(validate(&destination), Err(Error::Input(_))),
            "{destination:?}"
        );
    };
    // The body holds the value exactly once, as a whole string, in an object.
    refused(http(json!({"value": "x"})));
    refused(http(json!({"a": PLACEHOLDER, "b": PLACEHOLDER})));
    refused(http(json!({"value": format!("Bearer {PLACEHOLDER}")})));
    refused(http(json!([PLACEHOLDER])));
    // HTTPS on the default port, without credentials, a fragment or a placeholder.
    for url in [
        "http://editor.example.com/x",
        "https://editor.example.com:8443/x",
        "https://user:pass@editor.example.com/x",
        "https://editor.example.com/x#f",
        "https://editor.example.com/{{value}}",
        "ftp://editor.example.com/x",
    ] {
        refused(with(http(json!({"v": PLACEHOLDER})), |t| {
            if let Target::Http { url: u, .. } = t {
                *u = url.into();
            }
        }));
    }
    // No method other than PUT, POST, PATCH; no fixed credential or host-owned header.
    refused(with(http(json!({"v": PLACEHOLDER})), |t| {
        if let Target::Http { method, .. } = t {
            *method = "GET".into();
        }
    }));
    for (name, value) in [
        ("Authorization", "Bearer fixed"),
        ("Host", "other.example.com"),
        ("Cookie", "session=1"),
        ("X-Line", "one\r\ntwo"),
        ("X-Value", PLACEHOLDER),
    ] {
        refused(with(http(json!({"v": PLACEHOLDER})), |t| {
            if let Target::Http { headers, .. } = t {
                headers.insert(name.into(), value.into());
            }
        }));
    }
    // Variables are named by entry ID, once each, at most eight.
    let mut many = http(json!({"v": PLACEHOLDER}));
    many.variables = (0..9)
        .map(|i| format!("aaaaaaaa-0000-4000-8000-00000000001{i}"))
        .collect();
    refused(many);
    let mut twice = http(json!({"v": PLACEHOLDER}));
    twice.variables = vec![ENTRY.into(), ENTRY.into()];
    refused(twice);
    let mut named = http(json!({"v": PLACEHOLDER}));
    named.variables = vec!["OPENAI_API_KEY".into()];
    refused(named);
    refused(with(cloudflare(), |t| {
        if let Target::CloudflareWorkerSecret { account_id, .. } = t {
            *account_id = "not-an-account".into();
        }
    }));
}

#[test]
fn an_approval_names_exactly_what_was_reviewed() {
    let dir = tempfile::tempdir().unwrap();
    let list = Destinations::new(dir.path());
    let proposed = http(json!({"value": PLACEHOLDER}));
    list.add(Destination {
        approved_at: Some(5),
        approved_digest: Some(proposed.digest()),
        ..proposed.clone()
    })
    .unwrap();
    // A proposal never arrives approved, whatever it claims.
    assert!(!list.get(&proposed.id).unwrap().approved());
    assert!(matches!(
        list.approve(&proposed.id, "0000", 9),
        Err(Error::Conflict(_))
    ));
    let approved = list.approve(&proposed.id, &proposed.digest(), 9).unwrap();
    assert!(approved.approved());
    // Any change to what was reviewed voids the approval: URL, header, body or variables.
    for changed in [
        with(approved.clone(), |t| {
            if let Target::Http { url, .. } = t {
                *url = "https://attacker.example.com/collect".into();
            }
        }),
        with(approved.clone(), |t| {
            if let Target::Http { headers, .. } = t {
                headers.insert("X-Extra".into(), "1".into());
            }
        }),
        Destination {
            variables: vec![AUTH.into()],
            ..approved.clone()
        },
    ] {
        assert!(!changed.approved());
    }
    // A name is never redefined in place, and another account's list is not reused.
    assert!(matches!(
        list.add(proposed.clone()),
        Err(Error::Conflict(_))
    ));
    assert_eq!(
        Destinations::account(dir.path(), "other").list().unwrap(),
        []
    );
    assert!(list.remove(&proposed.id).unwrap());
    assert!(!list.remove(&proposed.id).unwrap());
    // The stored file holds the definition only: no value can be in it.
    assert!(
        !std::fs::read_to_string(dir.path().join("deliveries.json"))
            .unwrap_or_default()
            .contains(VALUE)
    );
}

#[test]
fn requests_carry_the_value_only_in_the_body_and_the_credential_only_in_its_header() {
    let value = Secret::new(VALUE);
    let token = Secret::new(TOKEN);
    let built = request(
        &http(json!({"value": PLACEHOLDER, "kind": "openai"})),
        &value,
        Some(&token),
    )
    .unwrap();
    assert_eq!(
        built.url,
        "https://editor.example.com/api/import/secrets/OPENAI_API_KEY"
    );
    assert_eq!(built.method, reqwest::Method::PUT);
    assert_eq!(
        serde_json::from_slice::<Value>(&built.body).unwrap(),
        json!({"value": VALUE, "kind": "openai"})
    );
    assert!(
        built
            .headers
            .contains(&("Authorization".into(), format!("Bearer {TOKEN}")))
    );
    assert!(built.headers.iter().all(|(_, v)| !v.contains(VALUE)));
    assert!(matches!(
        request(&http(json!({"value": PLACEHOLDER})), &value, None),
        Err(Error::Input(_))
    ));
    let worker = request(&cloudflare(), &value, Some(&token)).unwrap();
    assert_eq!(
        worker.url,
        "https://api.cloudflare.com/client/v4/accounts/0123456789abcdef0123456789abcdef/workers/scripts/blog-editor/secrets"
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&worker.body).unwrap(),
        json!({"name": "GITHUB_TOKEN", "text": VALUE, "type": "secret_text"})
    );
    // A credential that would break the header line is refused, not sent.
    assert!(request(&cloudflare(), &value, Some(&Secret::new("a\nb"))).is_err());
}

#[test]
fn confirmation_is_computed_here_and_never_echoes_the_reply() {
    let value = Secret::new(VALUE);
    let destination = http(json!({"value": PLACEHOLDER}));
    assert!(confirmed(
        &destination,
        br#"{"ok":true,"last4":"5be1"}"#,
        &value
    ));
    assert!(!confirmed(
        &destination,
        br#"{"ok":true,"last4":"0000"}"#,
        &value
    ));
    assert!(!confirmed(&destination, b"not json", &value));
    assert!(!confirmed(
        &destination,
        br#"{"ok":true,"last4":"e1"}"#,
        &Secret::new("short")
    ));
    assert!(confirmed(
        &cloudflare(),
        br#"{"success":true,"result":{"name":"GITHUB_TOKEN","type":"secret_text"}}"#,
        &value
    ));
    assert!(!confirmed(&cloudflare(), br#"{"success":false}"#, &value));
}
