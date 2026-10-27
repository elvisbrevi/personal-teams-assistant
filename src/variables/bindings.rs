//! Use-time bindings: which stored variable a consumer slot resolves to.
//!
//! The variable itself stays generic (name, description, value). A binding only records, per
//! account, that a consumer slot refers to one entry ID; the consumer's own rules are checked
//! when the value is used. A binding to an entry that was deleted stays in place and fails
//! closed instead of falling back to another source.
use super::*;

/// What a binding currently points at, without any value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Binding {
    pub consumer: String,
    pub slot: String,
    pub entry: String,
    /// `ok`, or why the reference cannot be used: `deleted` or `missing`.
    pub state: &'static str,
    /// Name and version of the entry while it exists.
    pub variable: Option<BoundVariable>,
    pub bound_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BoundVariable {
    pub name: String,
    pub version: u64,
}

impl Store {
    /// Point a consumer slot at one entry of this account, replacing an earlier binding.
    pub fn bind(&self, consumer: &str, slot: &str, entry: &str) -> Result<Binding> {
        let entry = entry_id(entry)?;
        let db = self.lock();
        let Some(row) = fetch(
            &db,
            "SELECT id,name,description,revision,dek_version,ciphertext,created_at,updated_at FROM variables WHERE account=?1 AND id=?2",
            params![self.account, entry],
        )?
        else {
            return Err(missing(&db, &self.account, &entry)?);
        };
        let at = now();
        db.execute(
            "INSERT INTO consumer_bindings(account,consumer,slot,entry,bound_at) VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(account,consumer,slot) DO UPDATE SET entry=?4, bound_at=?5",
            params![self.account, consumer, slot, entry, at],
        )?;
        Ok(Binding {
            consumer: consumer.into(),
            slot: slot.into(),
            entry,
            state: "ok",
            variable: Some(BoundVariable {
                name: row.name,
                version: row.revision,
            }),
            bound_at: at,
        })
    }

    /// Remove a binding; reports whether one existed.
    pub fn unbind(&self, consumer: &str, slot: &str) -> Result<bool> {
        let db = self.lock();
        Ok(db.execute(
            "DELETE FROM consumer_bindings WHERE account=?1 AND consumer=?2 AND slot=?3",
            params![self.account, consumer, slot],
        )? > 0)
    }

    /// Every binding of this account with the state of the entry it points at.
    pub fn bindings(&self) -> Result<Vec<Binding>> {
        let db = self.lock();
        let mut statement = db.prepare(
            "SELECT consumer,slot,entry,bound_at FROM consumer_bindings WHERE account=?1 ORDER BY consumer,slot",
        )?;
        let rows: Vec<(String, String, String, i64)> = statement
            .query_map([&self.account], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<std::result::Result<_, _>>()?;
        rows.into_iter()
            .map(|(consumer, slot, entry, bound_at)| {
                binding_state(&db, &self.account, consumer, slot, entry, bound_at)
            })
            .collect()
    }

    /// The binding of one slot, if any, with the state of its entry.
    pub fn binding(&self, consumer: &str, slot: &str) -> Result<Option<Binding>> {
        let db = self.lock();
        let row: Option<(String, i64)> = db
            .query_row(
                "SELECT entry,bound_at FROM consumer_bindings WHERE account=?1 AND consumer=?2 AND slot=?3",
                params![self.account, consumer, slot],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.map(|(entry, bound_at)| {
            binding_state(
                &db,
                &self.account,
                consumer.into(),
                slot.into(),
                entry,
                bound_at,
            )
        })
        .transpose()
    }

    /// The value of one entry for an authorized consumer. `expected` is the version the caller
    /// chose: another current version is a conflict naming the version now stored, never a
    /// silent use of a different value. Without it the current version is used and reported.
    pub fn use_entry(&self, entry: &str, expected: Option<u64>) -> Result<(Metadata, Secret)> {
        let (metadata, value) = self.reveal(entry)?;
        if let Some(expected) = expected
            && expected != metadata.version
        {
            return Err(Error::Conflict(format!(
                "The variable is now at version {}, not {expected}. Inspect it before using it.",
                metadata.version
            )));
        }
        Ok((metadata, value))
    }

    /// The value a bound slot resolves to now, or `None` when nothing is bound. A binding that
    /// cannot be used (deleted entry, unreadable key) is an error, never `None`: the consumer
    /// must not fall back to another credential source.
    pub fn bound_value(&self, consumer: &str, slot: &str) -> Result<Option<(Metadata, Secret)>> {
        match self.binding(consumer, slot)? {
            None => Ok(None),
            Some(binding) => self.use_entry(&binding.entry, None).map(Some),
        }
    }

    /// What an entry ID refers to now: `ok` with its name and version, `deleted` or `missing`.
    /// Never decrypts.
    pub fn reference(&self, entry: &str) -> Result<(&'static str, Option<BoundVariable>)> {
        let entry = entry_id(entry)?;
        let db = self.lock();
        let found = binding_state(&db, &self.account, String::new(), String::new(), entry, 0)?;
        Ok((found.state, found.variable))
    }

    /// Whether a slot has a binding (the entry may no longer exist); never decrypts.
    pub fn is_bound(&self, consumer: &str, slot: &str) -> Result<bool> {
        Ok(self.binding(consumer, slot)?.is_some())
    }
}

fn binding_state(
    db: &Connection,
    account: &str,
    consumer: String,
    slot: String,
    entry: String,
    bound_at: i64,
) -> Result<Binding> {
    let found: Option<(String, u64)> = db
        .query_row(
            "SELECT name,revision FROM variables WHERE account=?1 AND id=?2",
            params![account, entry],
            |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u64)),
        )
        .optional()?;
    let (state, variable) = match found {
        Some((name, version)) => ("ok", Some(BoundVariable { name, version })),
        None if tombstone(db, account, &entry)?.is_some() => ("deleted", None),
        None => ("missing", None),
    };
    Ok(Binding {
        consumer,
        slot,
        entry,
        state,
        variable,
        bound_at,
    })
}
