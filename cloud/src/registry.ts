import { DurableObject } from "cloudflare:workers";
import type { Env } from "./env";
import { LIMITS, REGISTRY_SCHEMA } from "./protocol";

/** Who a request acts for. Derived only from the Access identity and the installation credential. */
export interface Principal {
  account: string;
  installation: string;
  /** The Access service token identity (its client ID) the account is bound to. */
  identity: string;
}

export interface Installation {
  id: string;
  label: string;
  host_version: string;
  enrolled_at: number;
  last_seen_at: number;
  revoked_at: number | null;
}

export type Enrollment =
  | { ok: true; installation: string; generation: number }
  | { ok: false; reason: "installation_limit" };

export type Rebinding =
  | { ok: true; rebound: boolean; principal: Principal }
  | { ok: false; reason: "unauthorized" | "identity_in_use" };

/** How often an installation's `last_seen_at` is written: once an hour keeps row writes low. */
const SEEN_INTERVAL_SECONDS = 3600;

/**
 * The registry: one object for the whole Worker (`idFromName("registry")`).
 *
 * It binds each Access service token identity to one account (created on the token's first
 * enrollment, with a random ID nothing outside the Worker sees) and keeps the installation
 * credentials of each account as SHA-256 digests. An installation is bound to its account and to
 * the account's generation: raising the generation revokes every installation at once.
 */
export class Registry extends DurableObject<Env> {
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
    if (version > REGISTRY_SCHEMA) throw new Error("The registry was written by a newer Worker.");
    if (version < 1) {
      this.sql.exec(`
        CREATE TABLE accounts (
          id TEXT PRIMARY KEY,
          identity TEXT NOT NULL UNIQUE,
          generation INTEGER NOT NULL,
          created_at INTEGER NOT NULL
        );
        CREATE TABLE installations (
          id TEXT PRIMARY KEY,
          account TEXT NOT NULL REFERENCES accounts(id),
          token_sha256 TEXT NOT NULL UNIQUE,
          generation INTEGER NOT NULL,
          label TEXT NOT NULL,
          host_version TEXT NOT NULL,
          enrolled_at INTEGER NOT NULL,
          last_seen_at INTEGER NOT NULL,
          revoked_at INTEGER
        );
        CREATE INDEX installations_account ON installations(account);
      `);
    }
    this.sql.exec("INSERT OR REPLACE INTO meta (key, value) VALUES ('schema', ?)", REGISTRY_SCHEMA);
  }

  /** Enroll a new installation for the identity's account, creating the account on first use. */
  enroll(identity: string, tokenSha256: string, label: string, hostVersion: string, now: number): Enrollment {
    return this.ctx.storage.transactionSync(() => {
      let account = this.sql
        .exec<{ id: string; generation: number }>("SELECT id, generation FROM accounts WHERE identity = ?", identity)
        .toArray()[0];
      if (!account) {
        account = { id: crypto.randomUUID(), generation: 1 };
        this.sql.exec(
          "INSERT INTO accounts (id, identity, generation, created_at) VALUES (?, ?, ?, ?)",
          account.id,
          identity,
          account.generation,
          now,
        );
      }
      const active = this.sql
        .exec<{ n: number }>(
          "SELECT COUNT(*) AS n FROM installations WHERE account = ? AND revoked_at IS NULL AND generation = ?",
          account.id,
          account.generation,
        )
        .one().n;
      if (active >= LIMITS.installations) return { ok: false, reason: "installation_limit" } as const;
      const installation = crypto.randomUUID();
      this.sql.exec(
        `INSERT INTO installations
           (id, account, token_sha256, generation, label, host_version, enrolled_at, last_seen_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
        installation,
        account.id,
        tokenSha256,
        account.generation,
        label,
        hostVersion,
        now,
        now,
      );
      return { ok: true, installation, generation: account.generation } as const;
    });
  }

  /**
   * The principal of an installation credential presented with an Access identity, or null. Both
   * must belong to the same account: a credential issued under another service token is refused.
   */
  authenticate(identity: string, tokenSha256: string, now: number): Principal | null {
    const row = this.sql
      .exec<{ installation: string; account: string; last_seen_at: number }>(
        `SELECT i.id AS installation, i.account AS account, i.last_seen_at AS last_seen_at
           FROM installations i JOIN accounts a ON a.id = i.account
          WHERE i.token_sha256 = ? AND a.identity = ? AND i.revoked_at IS NULL
            AND i.generation = a.generation`,
        tokenSha256,
        identity,
      )
      .toArray()[0];
    if (!row) return null;
    if (now - row.last_seen_at >= SEEN_INTERVAL_SECONDS) {
      this.sql.exec("UPDATE installations SET last_seen_at = ? WHERE id = ?", now, row.installation);
    }
    return { account: row.account, installation: row.installation, identity };
  }

  /**
   * Bind the account of an active installation credential to another Access identity (a renewed
   * or replaced service token), keeping the account, its store and its installations. The new
   * identity must not belong to an account yet; afterwards the previous identity reaches nothing
   * of this account, and every installation must present the new service token.
   */
  rebind(identity: string, tokenSha256: string, now: number): Rebinding {
    return this.ctx.storage.transactionSync(() => {
      const row = this.sql
        .exec<{ installation: string; account: string; identity: string }>(
          `SELECT i.id AS installation, i.account AS account, a.identity AS identity
             FROM installations i JOIN accounts a ON a.id = i.account
            WHERE i.token_sha256 = ? AND i.revoked_at IS NULL AND i.generation = a.generation`,
          tokenSha256,
        )
        .toArray()[0];
      if (!row) return { ok: false, reason: "unauthorized" } as const;
      const principal = { account: row.account, installation: row.installation, identity };
      if (row.identity === identity) return { ok: true, rebound: false, principal } as const;
      const taken = this.sql
        .exec<{ n: number }>("SELECT COUNT(*) AS n FROM accounts WHERE identity = ?", identity)
        .one().n;
      if (taken > 0) return { ok: false, reason: "identity_in_use" } as const;
      this.sql.exec("UPDATE accounts SET identity = ? WHERE id = ?", identity, row.account);
      this.sql.exec("UPDATE installations SET last_seen_at = ? WHERE id = ?", now, row.installation);
      return { ok: true, rebound: true, principal } as const;
    });
  }

  /** Accounts with at least one active installation: those sharing the plan's quota now. */
  sharing(): number {
    return this.sql
      .exec<{ n: number }>(
        `SELECT COUNT(DISTINCT a.id) AS n FROM accounts a JOIN installations i ON i.account = a.id
          WHERE i.revoked_at IS NULL AND i.generation = a.generation`,
      )
      .one().n;
  }

  /** The account's installations, newest first. */
  installations(account: string): Installation[] {
    return this.sql
      .exec<Installation & Record<string, SqlStorageValue>>(
        `SELECT id, label, host_version, enrolled_at, last_seen_at, revoked_at
           FROM installations WHERE account = ? ORDER BY enrolled_at DESC, id`,
        account,
      )
      .toArray()
      .map((row) => ({ ...row }));
  }

  /** The account's generation and its active installation count. */
  account(account: string): { generation: number; installations: number } {
    const generation = this.sql
      .exec<{ generation: number }>("SELECT generation FROM accounts WHERE id = ?", account)
      .one().generation;
    const installations = this.sql
      .exec<{ n: number }>(
        "SELECT COUNT(*) AS n FROM installations WHERE account = ? AND revoked_at IS NULL AND generation = ?",
        account,
        generation,
      )
      .one().n;
    return { generation, installations };
  }

  /** Revoke one installation of the account (idempotent); false when the account has no such installation. */
  revoke(account: string, installation: string, now: number): boolean {
    const known = this.sql
      .exec<{ n: number }>("SELECT COUNT(*) AS n FROM installations WHERE id = ? AND account = ?", installation, account)
      .one().n;
    if (known === 0) return false;
    this.sql.exec(
      "UPDATE installations SET revoked_at = ? WHERE id = ? AND account = ? AND revoked_at IS NULL",
      now,
      installation,
      account,
    );
    return true;
  }
}
