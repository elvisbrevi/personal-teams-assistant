use super::*;

const SECRET: &str = "synthetic-secret-value-7f3a91c2";
const OTHER: &str = "synthetic-secret-value-b84d05e6";

fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn new(name: &str, value: &str) -> NewVariable {
    NewVariable {
        id: id(),
        name: name.into(),
        description: format!("Synthetic variable {name}"),
        value: Secret::new(value),
    }
}
struct Fixture {
    _dir: tempfile::TempDir,
    db: PathBuf,
    keys: PathBuf,
}
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    Fixture {
        db: dir.path().join("variables").join("variables.db"),
        keys: dir.path().join("keys"),
        _dir: dir,
    }
}
fn account(f: &Fixture, name: &str) -> Store {
    Store::open_account(&f.db, &f.keys, name).unwrap()
}
fn raw_files(f: &Fixture) -> Vec<Vec<u8>> {
    let dir = f.db.parent().unwrap();
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| std::fs::read(e.unwrap().path()).unwrap())
        .collect()
}
fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}
fn sql(f: &Fixture, statement: &str) {
    let db = Connection::open(&f.db).unwrap();
    db.execute_batch(statement).unwrap();
}

#[test]
fn names_descriptions_and_values_are_bounded_and_environment_compatible() {
    for good in ["A", "_x", "deploy_endpoint", "Api2", &"A".repeat(MAX_NAME)] {
        assert!(validate_name(good).is_ok(), "{good}");
    }
    for bad in [
        "",
        "1A",
        "A-B",
        "A B",
        "ÁB",
        "A.B",
        &"A".repeat(MAX_NAME + 1),
    ] {
        assert!(validate_name(bad).is_err(), "{bad}");
    }
    // No TOKEN/KEY marker or credential type is required.
    assert!(validate_name("deploy_host").is_ok());
    assert!(validate_description("Where releases are published").is_ok());
    for bad in [
        "",
        "   ",
        "line\nbreak",
        "tab\there",
        "spoof \u{202E}txt.exe",
        &"d".repeat(MAX_DESCRIPTION + 1),
    ] {
        assert!(validate_description(bad).is_err(), "{bad:?}");
    }
    assert!(validate_value("line one\nline two\r\n").is_ok());
    assert!(validate_value("ünïcode ✓").is_ok());
    for bad in ["", "a\0b", &"v".repeat(MAX_VALUE + 1)] {
        assert!(validate_value(bad).is_err());
    }
    assert!(validate_value(&"v".repeat(MAX_VALUE)).is_ok());
}

#[test]
fn creation_lists_only_metadata_and_never_prints_a_value() {
    let f = fixture();
    let store = account(&f, "alice");
    let created = store.create(new("DEPLOY_ENDPOINT", SECRET)).unwrap();
    assert!(!created.replayed);
    assert_eq!(created.variable.version, 1);
    store.create(new("billing_user", "other")).unwrap();

    let all = store.list(None).unwrap();
    assert_eq!(
        all.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(),
        ["billing_user", "DEPLOY_ENDPOINT"]
    );
    let json = serde_json::to_string(&all).unwrap();
    assert!(!json.contains(SECRET) && !json.contains("ciphertext"));
    assert_eq!(store.list(Some("deploy")).unwrap().len(), 1);
    assert_eq!(
        store
            .list(Some("SYNTHETIC VARIABLE BILLING"))
            .unwrap()
            .len(),
        1
    );
    assert!(store.list(Some("absent")).unwrap().is_empty());
    assert_eq!(format!("{:?}", Secret::new(SECRET)), "Secret(<redacted>)");
    let entry = new("X", SECRET);
    assert!(!format!("{:?}", entry.value).contains(SECRET));
    assert_eq!(store.resolve("deploy_endpoint").unwrap().expose(), SECRET);
}

#[test]
fn values_are_encrypted_at_rest_and_keys_stay_outside_the_database() {
    let f = fixture();
    let store = account(&f, "alice");
    store.create(new("TOKEN_ONE", SECRET)).unwrap();
    let master = std::fs::read_to_string(f.keys.join("master-1.key")).unwrap();
    for bytes in raw_files(&f) {
        assert!(!contains(&bytes, SECRET), "plaintext value in a store file");
        assert!(
            !contains(&bytes, master.trim()),
            "master key in a store file"
        );
    }
    // Descriptions are metadata and readable by design.
    assert!(
        raw_files(&f)
            .iter()
            .any(|b| contains(b, "Synthetic variable TOKEN_ONE"))
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(f.keys.join("master-1.key"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // A reopened store reads the same value.
    drop(store);
    assert_eq!(
        account(&f, "alice").resolve("TOKEN_ONE").unwrap().expose(),
        SECRET
    );
}

#[test]
fn creation_is_idempotent_and_never_overwrites_an_unrelated_entry() {
    let f = fixture();
    let store = account(&f, "alice");
    let first = new("DEPLOY_ENDPOINT", SECRET);
    let (entry, description) = (first.id.clone(), first.description.clone());
    store.create(first).unwrap();

    // A retry after a timeout reports the existing entry and writes nothing.
    let retry = store
        .create(NewVariable {
            id: entry.clone(),
            name: "DEPLOY_ENDPOINT".into(),
            description: description.clone(),
            value: Secret::new(SECRET),
        })
        .unwrap();
    assert!(retry.replayed);
    assert_eq!(retry.variable.version, 1);
    assert_eq!(store.list(None).unwrap().len(), 1);

    // The same entry ID with other content, or another entry with the same name in any case,
    // is a conflict and the original stays intact.
    for (entry_id, name, value) in [
        (entry.clone(), "DEPLOY_ENDPOINT", OTHER),
        (entry.clone(), "RENAMED", SECRET),
        (id(), "DEPLOY_ENDPOINT", OTHER),
        (id(), "deploy_endpoint", OTHER),
    ] {
        let error = store
            .create(NewVariable {
                id: entry_id,
                name: name.into(),
                description: description.clone(),
                value: Secret::new(value),
            })
            .unwrap_err();
        assert!(matches!(error, Error::Conflict(_)), "{error}");
        assert!(!error.to_string().contains(SECRET) && !error.to_string().contains(OTHER));
    }
    assert_eq!(store.resolve("DEPLOY_ENDPOINT").unwrap().expose(), SECRET);
    assert_eq!(store.list(None).unwrap().len(), 1);
    assert!(
        store
            .create(NewVariable {
                id: "not-a-uuid".into(),
                ..new("A", "v")
            })
            .is_err()
    );
}

#[test]
fn account_count_is_bounded() {
    let f = fixture();
    let store = account(&f, "alice");
    for n in 0..MAX_VARIABLES {
        store.create(new(&format!("V{n}"), "v")).unwrap();
    }
    assert!(matches!(
        store.create(new("ONE_MORE", "v")),
        Err(Error::Input(_))
    ));
}

#[test]
fn accounts_are_isolated_and_swapped_ciphertext_fails_closed() {
    let f = fixture();
    let alice = account(&f, "alice");
    let bob = account(&f, "bob");
    alice.create(new("SHARED_NAME", SECRET)).unwrap();
    alice.create(new("SECOND", "second-value")).unwrap();
    bob.create(new("SHARED_NAME", OTHER)).unwrap();
    assert_eq!(alice.list(None).unwrap().len(), 2);
    assert_eq!(bob.list(None).unwrap().len(), 1);
    assert_eq!(alice.resolve("shared_name").unwrap().expose(), SECRET);
    assert_eq!(bob.resolve("SHARED_NAME").unwrap().expose(), OTHER);
    assert!(bob.resolve("SECOND").is_err());

    // Separate data keys.
    let db = Connection::open(&f.db).unwrap();
    let keys: i64 = db
        .query_row(
            "SELECT COUNT(DISTINCT wrapped) FROM account_keys",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(keys, 2);

    // Bob's ciphertext moved into Alice's entry, and Alice's into another of her entries.
    sql(
        &f,
        "UPDATE variables SET ciphertext=(SELECT ciphertext FROM variables WHERE account='bob') WHERE account='alice' AND name='SHARED_NAME'",
    );
    assert!(matches!(
        alice.resolve("SHARED_NAME"),
        Err(Error::Integrity(_))
    ));
    sql(
        &f,
        "UPDATE variables SET ciphertext=(SELECT ciphertext FROM variables WHERE account='alice' AND name='SHARED_NAME') WHERE account='alice' AND name='SECOND'",
    );
    assert!(matches!(alice.resolve("SECOND"), Err(Error::Integrity(_))));
    let report = alice.verify().unwrap();
    assert_eq!(report.checked, 2);
    assert_eq!(report.failed.len(), 2);
    assert_eq!(bob.resolve("SHARED_NAME").unwrap().expose(), OTHER);
    assert!(bob.verify().unwrap().failed.is_empty());
}

#[test]
fn every_bound_field_is_authenticated() {
    for tamper in [
        "UPDATE variables SET name='RENAMED', name_key='RENAMED'",
        "UPDATE variables SET revision=2",
        "UPDATE variables SET id='00000000-0000-4000-8000-000000000000'",
        "UPDATE variables SET ciphertext=zeroblob(length(ciphertext))",
        "FLIP",
        "UPDATE variables SET ciphertext=x'00'",
    ] {
        let f = fixture();
        let store = account(&f, "alice");
        store.create(new("ORIGINAL", SECRET)).unwrap();
        if tamper == "FLIP" {
            // One bit of the authenticated ciphertext.
            let db = Connection::open(&f.db).unwrap();
            let mut sealed: Vec<u8> = db
                .query_row("SELECT ciphertext FROM variables", [], |r| r.get(0))
                .unwrap();
            let last = sealed.len() - 1;
            sealed[last] ^= 1;
            db.execute("UPDATE variables SET ciphertext=?1", [sealed])
                .unwrap();
        } else {
            sql(&f, tamper);
        }
        let name = store.list(None).unwrap()[0].name.clone();
        assert!(
            matches!(store.resolve(&name), Err(Error::Integrity(_))),
            "{tamper}"
        );
        assert!(store.verify().unwrap().failed.contains(&name), "{tamper}");
    }
}

#[test]
fn a_missing_or_wrong_key_fails_closed_without_generating_one() {
    let f = fixture();
    let store = account(&f, "alice");
    store.create(new("KEEP_ME", SECRET)).unwrap();
    let backup = store.export_keys().unwrap();
    std::fs::remove_file(f.keys.join("master-1.key")).unwrap();

    assert!(matches!(store.resolve("KEEP_ME"), Err(Error::Key(_))));
    assert!(matches!(
        store.create(new("ANOTHER", "v")),
        Err(Error::Key(_))
    ));
    assert!(matches!(store.rotate(), Err(Error::Key(_))));
    assert!(
        store.keys.versions().unwrap().is_empty(),
        "no key was regenerated"
    );
    assert_eq!(store.verify().unwrap().failed, ["KEEP_ME"]);

    // A different key under the same name authenticates nothing.
    store.keys.create(1, &random_key()).unwrap();
    assert!(matches!(store.resolve("KEEP_ME"), Err(Error::Key(_))));
    std::fs::remove_file(f.keys.join("master-1.key")).unwrap();

    // Restoring the operator's backup makes the data readable again.
    assert_eq!(store.import_keys(&backup).unwrap(), [1]);
    assert_eq!(store.resolve("KEEP_ME").unwrap().expose(), SECRET);
    assert!(store.verify().unwrap().failed.is_empty());

    // Lost account key rows with existing data never produce a fresh key either.
    sql(&f, "DELETE FROM account_keys");
    assert!(matches!(
        store.create(new("LATER", "v")),
        Err(Error::Key(_))
    ));
    assert_eq!(
        account(&f, "alice").list(None).unwrap().len(),
        1,
        "nothing was changed"
    );
}

#[test]
fn key_backup_restores_a_copied_database_and_never_replaces_a_different_key() {
    let f = fixture();
    let store = account(&f, "alice");
    store.create(new("RESTORE_ME", SECRET)).unwrap();
    let backup = store.export_keys().unwrap();
    // The backup holds keys only: no ciphertext, names or values.
    let text = serde_json::to_string(&backup).unwrap();
    assert!(!text.contains("RESTORE_ME") && !text.contains(SECRET));
    assert_eq!(format!("{backup:?}"), "KeyBackup(1 keys)");

    let elsewhere = fixture();
    std::fs::create_dir_all(elsewhere.db.parent().unwrap()).unwrap();
    // Checkpoint, then copy the database as a routine backup would.
    Connection::open(&f.db)
        .unwrap()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    std::fs::copy(&f.db, &elsewhere.db).unwrap();
    let restored = account(&elsewhere, "alice");
    assert!(matches!(restored.resolve("RESTORE_ME"), Err(Error::Key(_))));
    restored.import_keys(&backup).unwrap();
    assert_eq!(restored.resolve("RESTORE_ME").unwrap().expose(), SECRET);

    let mut different = KeyBackup {
        format: backup.format.clone(),
        masters: backup.masters.clone(),
    };
    different.masters.insert(1, STANDARD.encode(random_key()));
    assert!(matches!(
        restored.import_keys(&different),
        Err(Error::Conflict(_))
    ));
    assert_eq!(restored.resolve("RESTORE_ME").unwrap().expose(), SECRET);
    for broken in [
        KeyBackup {
            format: "other".into(),
            masters: backup.masters.clone(),
        },
        KeyBackup {
            format: backup.format.clone(),
            masters: BTreeMap::new(),
        },
        KeyBackup {
            format: backup.format.clone(),
            masters: BTreeMap::from([(1, "!!".into())]),
        },
    ] {
        assert!(restored.import_keys(&broken).is_err());
    }
}

#[test]
fn rotation_rewraps_and_reencrypts_every_account_and_failures_change_nothing() {
    let f = fixture();
    let alice = account(&f, "alice");
    let bob = account(&f, "bob");
    alice.create(new("ONE", SECRET)).unwrap();
    alice.create(new("TWO", "second\nline")).unwrap();
    bob.create(new("BOBS", OTHER)).unwrap();
    let before: Vec<u8> = Connection::open(&f.db)
        .unwrap()
        .query_row(
            "SELECT ciphertext FROM variables WHERE name='ONE'",
            [],
            |r| r.get(0),
        )
        .unwrap();

    let rotation = alice.rotate().unwrap();
    assert_eq!(rotation.master_version, 2);
    assert_eq!(rotation.data_key_version, 2);
    assert_eq!(rotation.variables, 2);
    assert_eq!(rotation.retained_master_versions, [1, 2]);
    let after: Vec<u8> = Connection::open(&f.db)
        .unwrap()
        .query_row(
            "SELECT ciphertext FROM variables WHERE name='ONE'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_ne!(before, after);
    assert_eq!(alice.resolve("ONE").unwrap().expose(), SECRET);
    assert_eq!(alice.resolve("TWO").unwrap().expose(), "second\nline");
    // Another account sharing the store moved to the new master key as well.
    std::fs::remove_file(f.keys.join("master-1.key")).unwrap();
    assert_eq!(bob.resolve("BOBS").unwrap().expose(), OTHER);
    assert_eq!(alice.list(None).unwrap()[0].version, 1);
    alice.create(new("THREE", "third")).unwrap();
    assert!(alice.verify().unwrap().failed.is_empty());

    // An entry that fails authentication aborts the rotation before any key is written.
    sql(&f, "UPDATE variables SET ciphertext=x'00' WHERE name='TWO'");
    let versions = alice.keys.versions().unwrap();
    assert!(matches!(alice.rotate(), Err(Error::Integrity(_))));
    assert_eq!(alice.keys.versions().unwrap(), versions);
    assert_eq!(alice.resolve("ONE").unwrap().expose(), SECRET);
}

#[test]
fn a_profile_store_keeps_a_stable_account_and_refuses_another_portal_account() {
    let dir = tempfile::tempdir().unwrap();
    let first = Store::open_profile(dir.path(), None).unwrap();
    first.create(new("LOCAL_ONE", SECRET)).unwrap();
    let account = first.account.clone();
    drop(first);

    // Binding the profile to a portal account later keeps its variables readable.
    let bound =
        Store::open_profile(dir.path(), Some("11111111-1111-4111-8111-111111111111")).unwrap();
    assert_eq!(bound.account, account);
    assert_eq!(bound.resolve("LOCAL_ONE").unwrap().expose(), SECRET);
    drop(bound);
    assert!(Store::open_profile(dir.path(), Some("11111111-1111-4111-8111-111111111111")).is_ok());
    assert!(matches!(
        Store::open_profile(dir.path(), Some("22222222-2222-4222-8222-222222222222")),
        Err(Error::Conflict(_))
    ));
    // Variables are never in the profile's other data and keys are kept apart.
    assert!(dir.path().join("keys").join("master-1.key").exists());
    assert!(dir.path().join("variables").join("variables.db").exists());
}

#[test]
fn the_published_package_has_no_variable_stores_or_keys() {
    let manifest = include_str!("../../Cargo.toml");
    let include = manifest
        .split("include = [")
        .nth(1)
        .and_then(|rest| rest.split(']').next())
        .unwrap();
    for entry in include
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('"'))
    {
        assert!(
            !entry.contains("variables") && !entry.contains("keys") && !entry.contains(".db"),
            "{entry}"
        );
    }
}
