import { beforeEach, describe, expect, it, vi } from "vitest";
import { forgetKeys } from "../src/access";
import { assertion, call, EMPTY_DIGEST, enrolled, mockAccessKeys, sealed } from "./helpers";

let counter = 0;
/** A fresh service token identity per test: each one is its own account. */
function identity(): string {
  counter += 1;
  return `synthetic-token-${counter}.access`;
}

beforeEach(async () => {
  vi.restoreAllMocks();
  forgetKeys();
  await mockAccessKeys();
});

async function record(collection: string, id: string, revision: number, text: string) {
  return { collection, id, revision, schema: 1, ...(await sealed(text)) };
}

describe("health", () => {
  it("answers without credentials and reports versions", async () => {
    const response = await call("/v1/health");
    expect(response.status).toBe(200);
    const body = (await response.json()) as Record<string, unknown>;
    expect(body).toMatchObject({
      service: "personal-teams-assistant-cloud",
      environment: "production",
      protocols: [1],
      features: ["cas"],
      schema: { registry: 1, account_store: 1 },
      configured: true,
    });
    expect(response.headers.get("x-request-id")).toBeTruthy();
  });

  it("refuses unknown routes and methods", async () => {
    expect((await call("/v2/health")).status).toBe(404);
    expect((await call("/v1/health", { method: "POST" })).status).toBe(405);
  });
});

describe("Access assertion", () => {
  it("is required for enrollment and every account route", async () => {
    for (const path of ["/v1/whoami", "/v1/store", "/v1/installations", "/v1/records/notes", "/v1/backups"]) {
      expect((await call(path)).status).toBe(401);
    }
    const response = await call("/v1/enroll", { method: "POST", body: { label: "x", host_version: "1.0.0" } });
    expect(response.status).toBe(401);
  });

  it("rejects a forged signature, another audience or issuer, an expired token and a person's session", async () => {
    const other = (await crypto.subtle.generateKey(
      { name: "RSASSA-PKCS1-v1_5", modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: "SHA-256" },
      true,
      ["sign", "verify"],
    )) as CryptoKeyPair;
    const now = Math.floor(Date.now() / 1000);
    const bad = [
      await assertion("forged", {}, other.privateKey),
      await assertion("aud", { aud: ["another-audience"] }),
      await assertion("iss", { iss: "https://evil.cloudflareaccess.com" }),
      await assertion("expired", { exp: now - 3600, iat: now - 7200 }),
      await assertion(undefined, { email: "person@example.com" }),
      "not-a-jwt",
    ];
    for (const value of bad) {
      const response = await call("/v1/enroll", {
        method: "POST",
        headers: { "cf-access-jwt-assertion": value },
        body: { label: "x", host_version: "1.0.0" },
      });
      expect(response.status).toBe(401);
    }
    const repeated = await assertion("repeated");
    const response = await call("/v1/enroll", {
      method: "POST",
      headers: { "cf-access-jwt-assertion": `${repeated}, ${repeated}` },
      body: { label: "x", host_version: "1.0.0" },
    });
    expect(response.status).toBe(401);
  });
});

describe("installation credential", () => {
  it("is issued once, stored as a digest and identifies the installation", async () => {
    const caller = await enrolled(identity());
    expect(caller.token).toMatch(/^ptac_[A-Za-z0-9_-]{43}$/);
    const response = await call("/v1/whoami", { caller });
    expect(response.status).toBe(200);
    const body = (await response.json()) as { installation: { id: string; label: string }; account: unknown };
    expect(body.installation.id).toBe(caller.installation);
    expect(body.installation.label).toBe("synthetic host");
    expect(body.account).toEqual({ generation: 1, installations: 1 });
    expect(JSON.stringify(body)).not.toContain(caller.token!);
  });

  it("needs the Access identity it was issued under", async () => {
    const caller = await enrolled(identity());
    const elsewhere = { identity: identity(), token: caller.token };
    expect((await call("/v1/whoami", { caller: elsewhere })).status).toBe(401);
    expect((await call("/v1/whoami", { caller: { identity: caller.identity } })).status).toBe(401);
    expect((await call("/v1/whoami", { caller: { identity: caller.identity, token: "ptac_" + "A".repeat(43) } })).status).toBe(401);
  });

  it("refuses unknown fields such as an account ID", async () => {
    const response = await call("/v1/enroll", {
      method: "POST",
      caller: { identity: identity() },
      body: { label: "x", host_version: "1.0.0", account: "someone-else" },
    });
    expect(response.status).toBe(400);
  });

  it("is revocable per installation, and only within its own account", async () => {
    const id = identity();
    const first = await enrolled(id, "first");
    const second = await enrolled(id, "second");
    const stranger = await enrolled(identity());
    const listed = (await (await call("/v1/installations", { caller: first })).json()) as {
      installations: { id: string; current: boolean }[];
    };
    expect(listed.installations.map((entry) => entry.id).sort()).toEqual([first.installation, second.installation].sort());
    expect(listed.installations.find((entry) => entry.current)?.id).toBe(first.installation);

    expect((await call(`/v1/installations/${first.installation}`, { method: "DELETE", caller: stranger })).status).toBe(404);
    expect((await call("/v1/whoami", { caller: first })).status).toBe(200);

    expect((await call(`/v1/installations/${second.installation}`, { method: "DELETE", caller: first })).status).toBe(200);
    expect((await call("/v1/whoami", { caller: second })).status).toBe(401);
    expect((await call("/v1/installations/self", { method: "DELETE", caller: first })).status).toBe(200);
    expect((await call("/v1/whoami", { caller: first })).status).toBe(401);
    expect((await call("/v1/whoami", { caller: stranger })).status).toBe(200);
  });
});

describe("account store", () => {
  it("upserts sealed records idempotently by collection, ID and revision", async () => {
    const caller = await enrolled(identity());
    const write = async (records: unknown[]) =>
      (await (await call("/v1/records", { method: "POST", caller, body: { records } })).json()) as {
        results: { result: string }[];
      };
    const v1 = await record("notes", "note-1", 1, "sealed-v1");
    expect((await write([v1])).results[0]?.result).toBe("created");
    expect((await write([v1])).results[0]?.result).toBe("unchanged");
    expect((await write([await record("notes", "note-1", 1, "different")])).results[0]?.result).toBe("conflict");
    expect((await write([await record("notes", "note-1", 2, "sealed-v2")])).results[0]?.result).toBe("updated");
    expect((await write([v1])).results[0]?.result).toBe("stale");
    const tombstone = { collection: "notes", id: "note-1", revision: 3, schema: 1, digest: EMPTY_DIGEST, deleted: true };
    expect((await write([tombstone])).results[0]?.result).toBe("updated");
    expect((await write([await record("notes", "note-1", 2, "sealed-v2")])).results[0]?.result).toBe("stale");

    const read = (await (await call("/v1/records/notes/note-1", { caller })).json()) as Record<string, unknown>;
    expect(read).toMatchObject({ revision: 3, deleted: true, sealed: null });
  });

  it("writes a record that names its base revision only over exactly that revision", async () => {
    const caller = await enrolled(identity());
    const write = async (records: unknown[], atomic?: boolean) =>
      (await (
        await call("/v1/records", { method: "POST", caller, body: atomic === undefined ? { records } : { records, atomic } })
      ).json()) as { results: { result: string }[] };
    const results = async (records: unknown[], atomic?: boolean) =>
      (await write(records, atomic)).results.map((entry) => entry.result);

    const first = { ...(await record("settings", "config", 1, "c1")), base_revision: 0 };
    expect(await results([first])).toEqual(["created"]);
    // Another writer that read "no record yet" loses.
    expect(await results([{ ...(await record("settings", "config", 2, "theirs")), base_revision: 0 }])).toEqual(["conflict"]);
    // A repeated send of what is held is acknowledged, not a conflict.
    expect(await results([first])).toEqual(["unchanged"]);
    const second = { ...(await record("settings", "config", 2, "c2")), base_revision: 1 };
    expect(await results([second])).toEqual(["updated"]);
    expect(await results([second])).toEqual(["unchanged"]);
    // A writer that read revision 1 after revision 2 was written is refused, even above it.
    expect(await results([{ ...(await record("settings", "config", 5, "late")), base_revision: 1 }])).toEqual(["conflict"]);
    const held = (await (await call("/v1/records/settings/config", { caller })).json()) as { revision: number; sealed: string };
    expect(held.revision).toBe(2);
    expect(atob(held.sealed)).toBe("c2");
    // A tombstone can be compare-and-swapped too.
    const tombstone = { collection: "settings", id: "config", revision: 3, schema: 1, digest: EMPTY_DIGEST, deleted: true, base_revision: 2 };
    expect(await results([tombstone])).toEqual(["updated"]);
    // A base at or above the record's own revision is invalid.
    const invalid = await call("/v1/records", {
      method: "POST",
      caller,
      body: { records: [{ ...(await record("settings", "x", 2, "x")), base_revision: 2 }] },
    });
    expect(invalid.status).toBe(400);
    const flag = await call("/v1/records", { method: "POST", caller, body: { records: [first], atomic: "yes" } });
    expect(flag.status).toBe(400);
  });

  it("writes an atomic batch all at once or not at all", async () => {
    const caller = await enrolled(identity());
    const results = async (records: unknown[], atomic: boolean) =>
      ((await (await call("/v1/records", { method: "POST", caller, body: { records, atomic } })).json()) as {
        results: { result: string }[];
      }).results.map((entry) => entry.result);
    const map = { ...(await record("settings", "knowledge_map", 1, "m1")), base_revision: 0 };
    expect(await results([map], true)).toEqual(["created"]);
    const before = (await (await call("/v1/store", { caller })).json()) as {
      collections: Record<string, { generation: number }>;
    };

    const config = { ...(await record("settings", "config", 1, "c1")), base_revision: 0 };
    const staleMap = { ...(await record("settings", "knowledge_map", 2, "m2")), base_revision: 0 };
    expect(await results([config, staleMap], true)).toEqual(["aborted", "conflict"]);
    expect((await call("/v1/records/settings/config", { caller })).status).toBe(404);
    const after = (await (await call("/v1/store", { caller })).json()) as {
      collections: Record<string, { generation: number }>;
    };
    expect(after.collections.settings?.generation).toBe(before.collections.settings?.generation);

    // Without the flag each record is decided on its own, as before.
    expect(await results([config, staleMap], false)).toEqual(["created", "conflict"]);
    // A batch that repeats what is held and changes the rest is not refused.
    const nextMap = { ...(await record("settings", "knowledge_map", 2, "m2")), base_revision: 1 };
    expect(await results([config, nextMap], true)).toEqual(["unchanged", "updated"]);
  });

  it("never lets a base revision reach another account's record", async () => {
    const owner = await enrolled(identity());
    const stranger = await enrolled(identity());
    const mine = { ...(await record("settings", "config", 1, "mine")), base_revision: 0 };
    await call("/v1/records", { method: "POST", caller: owner, body: { records: [mine] } });
    // The stranger's store has no such record: base 1 conflicts there, and base 0 creates the
    // stranger's own record without touching the owner's.
    const theirs = async (base: number) =>
      ((await (
        await call("/v1/records", {
          method: "POST",
          caller: stranger,
          body: { records: [{ ...(await record("settings", "config", 2, "theirs")), base_revision: base }], atomic: true },
        })
      ).json()) as { results: { result: string }[] }).results[0]?.result;
    expect(await theirs(1)).toBe("conflict");
    expect(await theirs(0)).toBe("created");
    const held = (await (await call("/v1/records/settings/config", { caller: owner })).json()) as { revision: number; sealed: string };
    expect(held.revision).toBe(1);
    expect(atob(held.sealed)).toBe("mine");
  });

  it("lists changes after a generation", async () => {
    const caller = await enrolled(identity());
    const records = [await record("audits", "a", 1, "one"), await record("audits", "b", 1, "two")];
    await call("/v1/records", { method: "POST", caller, body: { records } });
    const page = (await (await call("/v1/records/audits?since=0", { caller })).json()) as {
      records: { id: string; generation: number; sealed: string }[];
      generation: number;
      more: boolean;
    };
    expect(page.records.map((entry) => entry.id)).toEqual(["a", "b"]);
    expect(atob(page.records[0]!.sealed)).toBe("one");
    expect(page.generation).toBe(2);
    expect(page.more).toBe(false);
    const after = (await (await call(`/v1/records/audits?since=1&limit=1`, { caller })).json()) as {
      records: { id: string }[];
    };
    expect(after.records.map((entry) => entry.id)).toEqual(["b"]);
  });

  it("never lets one account read or overwrite another's records", async () => {
    const owner = await enrolled(identity());
    const stranger = await enrolled(identity());
    await call("/v1/records", { method: "POST", caller: owner, body: { records: [await record("notes", "shared-id", 1, "mine")] } });

    expect((await call("/v1/records/notes/shared-id", { caller: stranger })).status).toBe(404);
    const listing = (await (await call("/v1/records/notes", { caller: stranger })).json()) as { records: unknown[] };
    expect(listing.records).toEqual([]);
    const written = (await (
      await call("/v1/records", { method: "POST", caller: stranger, body: { records: [await record("notes", "shared-id", 1, "theirs")] } })
    ).json()) as { results: { result: string }[] };
    expect(written.results[0]?.result).toBe("created");
    const mine = (await (await call("/v1/records/notes/shared-id", { caller: owner })).json()) as { sealed: string };
    expect(atob(mine.sealed)).toBe("mine");
    const summary = (await (await call("/v1/store", { caller: owner })).json()) as {
      collections: Record<string, { records: number }>;
    };
    expect(summary.collections.notes?.records).toBe(1);
  });

  it("validates batches", async () => {
    const caller = await enrolled(identity());
    const good = await record("notes", "n", 1, "x");
    const cases: unknown[] = [
      { records: [] },
      { records: [{ ...good, collection: "secrets" }] },
      { records: [{ ...good, digest: "0".repeat(64) }] },
      { records: [{ ...good, revision: 0 }] },
      { records: [{ ...good, account: "other" }] },
      { records: [good, good] },
      { records: [{ ...good, deleted: true }] },
      { records: [good], account: "other" },
    ];
    for (const body of cases) {
      const response = await call("/v1/records", { method: "POST", caller, body });
      expect([400, 404]).toContain(response.status);
    }
  });

  it("keeps a daily share per account", async () => {
    const caller = await enrolled(identity());
    const other = await enrolled(identity());
    const records = await Promise.all(Array.from({ length: 40 }, (_, i) => record("notes", `n${i}`, 1, `x${i}`)));
    expect((await call("/v1/records", { method: "POST", caller, body: { records } })).status).toBe(200);
    const exhausted = await call("/v1/records", {
      method: "POST",
      caller,
      body: { records: [await record("notes", "late", 1, "x")] },
    });
    expect(exhausted.status).toBe(429);
    const summary = (await (await call("/v1/store", { caller })).json()) as { usage: { rows_written: number } };
    expect(summary.usage.rows_written).toBe(40);
    // Another account's share is untouched.
    const theirs = await call("/v1/records", { method: "POST", caller: other, body: { records: [records[0]] } });
    expect(theirs.status).toBe(200);
  });
});

describe("backups", () => {
  it("stores encrypted blobs under the caller's account only", async () => {
    const owner = await enrolled(identity());
    const stranger = await enrolled(identity());
    const { digest } = await sealed("backup bytes");
    const put = await call("/v1/backups/2026-11-03.bin", {
      method: "PUT",
      caller: owner,
      raw: "backup bytes",
      headers: { "x-content-sha256": digest, "content-length": "12" },
    });
    expect(put.status).toBe(201);
    const fetched = await call("/v1/backups/2026-11-03.bin", { caller: owner });
    expect(new TextDecoder().decode(await fetched.arrayBuffer())).toBe("backup bytes");
    expect((await call("/v1/backups/2026-11-03.bin", { caller: stranger })).status).toBe(404);
    const theirs = (await (await call("/v1/backups", { caller: stranger })).json()) as { backups: unknown[] };
    expect(theirs.backups).toEqual([]);
    expect((await call("/v1/backups/2026-11-03.bin", { method: "DELETE", caller: stranger })).status).toBe(404);
    expect((await call("/v1/backups/..%2Fescape", { caller: owner })).status).toBe(400);
    const listed = (await (await call("/v1/backups", { caller: owner })).json()) as { backups: { name: string }[] };
    expect(listed.backups.map((entry) => entry.name)).toEqual(["2026-11-03.bin"]);
  });

  it("checks the declared digest", async () => {
    const owner = await enrolled(identity());
    const response = await call("/v1/backups/x.bin", {
      method: "PUT",
      caller: owner,
      raw: "bytes",
      headers: { "x-content-sha256": "0".repeat(64), "content-length": "5" },
    });
    expect(response.status).toBe(400);
  });
});
