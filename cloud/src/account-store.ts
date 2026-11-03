import { DurableObject } from "cloudflare:workers";
import type { Env } from "./env";
import { ACCOUNT_STORE_SCHEMA, COLLECTIONS, type Collection, LIMITS } from "./protocol";

/**
 * One account's database: one SQLite-backed object per account (`idFromName(account)`).
 *
 * Schema v1 keeps records sealed by the host (AES-256-GCM-SIV under the account's content key,
 * which never reaches Cloudflare) with their synchronization metadata in clear: collection, ID,
 * revision, the SHA-256 of the sealed bytes, size, record schema and the collection generation of
 * the last change. Deleting writes a tombstone so an older replica cannot resurrect the record.
 * Writes are idempotent upserts keyed by collection, ID and revision. A record that names the
 * revision it replaces (`base`) is written only over exactly that revision (compare-and-swap; 0 is
 * "no record yet"), and an atomic batch writes all of its records or none.
 */

export interface SealedRecord {
  collection: Collection;
  id: string;
  revision: number;
  /** Hex SHA-256 of `sealed`; checked by the Worker before the write. */
  digest: string;
  schema: number;
  deleted: boolean;
  sealed: Uint8Array | null;
  /** The revision this write replaces; absent for a plain upsert. */
  base?: number;
}

/** `aborted`: an atomic batch wrote nothing because another of its records was refused. */
export type WriteResult = "created" | "updated" | "unchanged" | "stale" | "conflict" | "aborted";

export interface StoredRecord {
  collection: Collection;
  id: string;
  revision: number;
  digest: string;
  schema: number;
  deleted: boolean;
  size: number;
  generation: number;
  updated_at: number;
  sealed: Uint8Array | null;
}

export interface Limits {
  requests: number;
  rowsWritten: number;
}

export interface Usage {
  day: string;
  requests: number;
  rows_written: number;
  limits: { requests: number; rows_written: number };
}

export type Charged<T> = { ok: true; value: T } | { ok: false; reason: "quota_exhausted"; usage: Usage };

interface Row extends Record<string, SqlStorageValue> {
  collection: string;
  id: string;
  revision: number;
  digest: string;
  schema: number;
  deleted: number;
  size: number;
  generation: number;
  updated_at: number;
  sealed: ArrayBuffer | null;
}

export class AccountStore extends DurableObject<Env> {
  private readonly sql: SqlStorage;

  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    this.sql = ctx.storage.sql;
    ctx.blockConcurrencyWhile(async () => this.migrate());
  }

  private migrate(): void {
    this.sql.exec("CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL)");
    const row = this.sql.exec<{ value: number }>("SELECT value FROM meta WHERE key = 'schema'").toArray()[0];
    const version = row?.value ?? 0;
    if (version > ACCOUNT_STORE_SCHEMA) throw new Error("The account store was written by a newer Worker.");
    if (version < 1) {
      this.sql.exec(`
        CREATE TABLE records (
          collection TEXT NOT NULL,
          id TEXT NOT NULL,
          revision INTEGER NOT NULL,
          digest TEXT NOT NULL,
          schema INTEGER NOT NULL,
          deleted INTEGER NOT NULL,
          size INTEGER NOT NULL,
          generation INTEGER NOT NULL,
          updated_at INTEGER NOT NULL,
          installation TEXT NOT NULL,
          sealed BLOB,
          PRIMARY KEY (collection, id)
        );
        CREATE INDEX records_generation ON records(collection, generation);
        CREATE TABLE collections (name TEXT PRIMARY KEY, generation INTEGER NOT NULL);
        CREATE TABLE usage (day TEXT PRIMARY KEY, requests INTEGER NOT NULL, rows_written INTEGER NOT NULL);
      `);
    }
    this.sql.exec("INSERT OR REPLACE INTO meta (key, value) VALUES ('schema', ?)", ACCOUNT_STORE_SCHEMA);
  }

  /** Count one request (and the rows it will write) against today's share, or refuse it. */
  private charge(limits: Limits, rows: number, now: number): Usage | null {
    const day = new Date(now * 1000).toISOString().slice(0, 10);
    const current = this.usage(limits, now);
    if (current.requests + 1 > limits.requests || current.rows_written + rows > limits.rowsWritten) {
      return current;
    }
    this.sql.exec(
      `INSERT INTO usage (day, requests, rows_written) VALUES (?, 1, ?)
       ON CONFLICT(day) DO UPDATE SET requests = requests + 1, rows_written = rows_written + excluded.rows_written`,
      day,
      rows,
    );
    // Only recent days matter; older counters are dropped.
    this.sql.exec("DELETE FROM usage WHERE day < ?", new Date((now - 7 * 86400) * 1000).toISOString().slice(0, 10));
    return null;
  }

  private usage(limits: Limits, now: number): Usage {
    const day = new Date(now * 1000).toISOString().slice(0, 10);
    const row = this.sql
      .exec<{ requests: number; rows_written: number }>("SELECT requests, rows_written FROM usage WHERE day = ?", day)
      .toArray()[0];
    return {
      day,
      requests: row?.requests ?? 0,
      rows_written: row?.rows_written ?? 0,
      limits: { requests: limits.requests, rows_written: limits.rowsWritten },
    };
  }

  /** Charge a request that does not touch records (backups). */
  admit(limits: Limits, now: number): Charged<null> {
    const exhausted = this.charge(limits, 0, now);
    return exhausted ? { ok: false, reason: "quota_exhausted", usage: exhausted } : { ok: true, value: null };
  }

  /** Upsert a batch of sealed records in one transaction; an atomic batch writes all or none. */
  write(
    installation: string,
    records: SealedRecord[],
    limits: Limits,
    now: number,
    atomic = false,
  ): Charged<WriteResult[]> {
    const exhausted = this.charge(limits, records.length, now);
    if (exhausted) return { ok: false, reason: "quota_exhausted", usage: exhausted };
    const results = this.ctx.storage.transactionSync(() => {
      const decided = records.map((record) => this.decide(record));
      if (atomic && decided.some((result) => result === "stale" || result === "conflict")) {
        return decided.map((result) => (result === "created" || result === "updated" ? "aborted" : result));
      }
      return records.map((record, index) => {
        const result = decided[index]!;
        if (result === "created" || result === "updated") this.put(installation, record, now);
        return result;
      });
    });
    return { ok: true, value: results };
  }

  /** What writing `record` would do, without writing it. */
  private decide(record: SealedRecord): WriteResult {
    const existing = this.sql
      .exec<{ revision: number; digest: string; deleted: number }>(
        "SELECT revision, digest, deleted FROM records WHERE collection = ? AND id = ?",
        record.collection,
        record.id,
      )
      .toArray()[0];
    // A repeated send of what is already held is acknowledged whatever it names as its base.
    if (existing?.revision === record.revision) {
      const same = existing.digest === record.digest && Boolean(existing.deleted) === record.deleted;
      return same ? "unchanged" : "conflict";
    }
    if (record.base !== undefined && (existing?.revision ?? 0) !== record.base) return "conflict";
    if (existing && existing.revision > record.revision) return "stale";
    return existing ? "updated" : "created";
  }

  private put(installation: string, record: SealedRecord, now: number): void {
    const generation = this.nextGeneration(record.collection);
    this.sql.exec(
      `INSERT INTO records
         (collection, id, revision, digest, schema, deleted, size, generation, updated_at, installation, sealed)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
       ON CONFLICT(collection, id) DO UPDATE SET
         revision = excluded.revision, digest = excluded.digest, schema = excluded.schema,
         deleted = excluded.deleted, size = excluded.size, generation = excluded.generation,
         updated_at = excluded.updated_at, installation = excluded.installation, sealed = excluded.sealed`,
      record.collection,
      record.id,
      record.revision,
      record.digest,
      record.schema,
      record.deleted ? 1 : 0,
      record.sealed?.byteLength ?? 0,
      generation,
      now,
      installation,
      record.sealed,
    );
  }

  private nextGeneration(collection: string): number {
    return this.sql
      .exec<{ generation: number }>(
        `INSERT INTO collections (name, generation) VALUES (?, 1)
         ON CONFLICT(name) DO UPDATE SET generation = generation + 1 RETURNING generation`,
        collection,
      )
      .one().generation;
  }

  /** Changes of a collection after a generation, oldest first, bounded by count and bytes. */
  changes(
    collection: Collection,
    since: number,
    limit: number,
    limits: Limits,
    now: number,
  ): Charged<{ records: StoredRecord[]; generation: number; more: boolean }> {
    const exhausted = this.charge(limits, 0, now);
    if (exhausted) return { ok: false, reason: "quota_exhausted", usage: exhausted };
    const rows = this.sql
      .exec<Row>(
        `SELECT collection, id, revision, digest, schema, deleted, size, generation, updated_at, sealed
           FROM records WHERE collection = ? AND generation > ? ORDER BY generation LIMIT ?`,
        collection,
        since,
        limit + 1,
      )
      .toArray();
    const records: StoredRecord[] = [];
    let bytes = 0;
    for (const row of rows.slice(0, limit)) {
      bytes += row.size;
      if (records.length > 0 && bytes > LIMITS.pageBytes) break;
      records.push(stored(row));
    }
    return {
      ok: true,
      value: { records, generation: this.generation(collection), more: records.length < rows.length },
    };
  }

  /** One record (tombstones included), or null. */
  read(collection: Collection, id: string, limits: Limits, now: number): Charged<StoredRecord | null> {
    const exhausted = this.charge(limits, 0, now);
    if (exhausted) return { ok: false, reason: "quota_exhausted", usage: exhausted };
    const row = this.sql
      .exec<Row>(
        `SELECT collection, id, revision, digest, schema, deleted, size, generation, updated_at, sealed
           FROM records WHERE collection = ? AND id = ?`,
        collection,
        id,
      )
      .toArray()[0];
    return { ok: true, value: row ? stored(row) : null };
  }

  private generation(collection: string): number {
    return (
      this.sql.exec<{ generation: number }>("SELECT generation FROM collections WHERE name = ?", collection).toArray()[0]
        ?.generation ?? 0
    );
  }

  /** Schema, per-collection counts and today's usage. Not charged: status must work when exhausted. */
  summary(limits: Limits, now: number) {
    const counts = new Map(
      this.sql
        .exec<{ collection: string; records: number; tombstones: number; bytes: number }>(
          `SELECT collection, SUM(1 - deleted) AS records, SUM(deleted) AS tombstones, SUM(size) AS bytes
             FROM records GROUP BY collection`,
        )
        .toArray()
        .map((row) => [row.collection, row]),
    );
    const collections = Object.fromEntries(
      COLLECTIONS.map((name) => {
        const row = counts.get(name);
        return [
          name,
          {
            records: row?.records ?? 0,
            tombstones: row?.tombstones ?? 0,
            bytes: row?.bytes ?? 0,
            generation: this.generation(name),
          },
        ];
      }),
    );
    return {
      schema: ACCOUNT_STORE_SCHEMA,
      collections,
      usage: this.usage(limits, now),
      stored_bytes: this.ctx.storage.sql.databaseSize,
    };
  }
}

function stored(row: Row): StoredRecord {
  return {
    collection: row.collection as Collection,
    id: row.id,
    revision: row.revision,
    digest: row.digest,
    schema: row.schema,
    deleted: row.deleted === 1,
    size: row.size,
    generation: row.generation,
    updated_at: row.updated_at,
    sealed: row.sealed ? new Uint8Array(row.sealed) : null,
  };
}
