import { accessSettings, decodeBase64Url, serviceIdentity } from "./access";
import type { Charged, Limits, SealedRecord, StoredRecord } from "./account-store";
import type { Env } from "./env";
import {
  ACCOUNT_STORE_SCHEMA,
  COLLECTIONS,
  type Collection,
  DEFAULT_DAILY_REQUESTS,
  DEFAULT_DAILY_ROWS_WRITTEN,
  json,
  LIMITS,
  FEATURES,
  PROTOCOLS,
  REGISTRY_SCHEMA,
  Refusal,
  refusal,
  WORKER_VERSION,
} from "./protocol";
import type { Principal } from "./registry";

export { AccountStore } from "./account-store";
export { Registry } from "./registry";

/**
 * The account store Worker, protocol v1.
 *
 * Every route except `/v1/health` needs an Access service token (checked again here through its
 * assertion) and, except `/v1/enroll`, the installation credential as a bearer token.
 * `authenticate` is the only source of identity: no route or body names an account, and the
 * account's objects are reached only through the authenticated principal.
 */
export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const started = Date.now();
    const requestId = crypto.randomUUID();
    let route = "unknown";
    let failure: string | undefined;
    let response: Response;
    try {
      const url = new URL(request.url);
      const matched = match(request.method, url.pathname);
      route = matched.route;
      response = await matched.handle(request, env, url);
    } catch (error) {
      // The error's class names an unexpected failure for the log; its message may carry data.
      if (!(error instanceof Refusal)) failure = error instanceof Error ? error.name : "unknown";
      response =
        error instanceof Refusal ? refusal(error, requestId) : refusal(new Refusal("internal", "Internal error."), requestId);
    }
    // Method, route pattern, status and duration only: never bodies, headers or identifiers.
    console.log(
      JSON.stringify({
        request: requestId,
        method: request.method,
        route,
        status: response.status,
        ms: Date.now() - started,
        ...(failure ? { failure } : {}),
      }),
    );
    response.headers.set("x-request-id", requestId);
    return response;
  },
} satisfies ExportedHandler<Env>;

type Handler = (request: Request, env: Env, url: URL) => Promise<Response>;

function match(method: string, path: string): { route: string; handle: Handler } {
  const parts = path.split("/").filter(Boolean);
  const route = (pattern: string, methods: Record<string, Handler>) => {
    const handle = methods[method];
    return {
      route: `${method} ${pattern}`,
      handle:
        handle ??
        (async () => {
          throw new Refusal("method_not_allowed", "Method not allowed.");
        }),
    };
  };
  const unknown = {
    route: `${method} *`,
    handle: async () => {
      throw new Refusal("not_found", "No such route.");
    },
  };
  if (parts[0] !== "v1") return unknown;
  const [, first, second, third] = parts;
  if (first === "health" && parts.length === 2) return route("/v1/health", { GET: health });
  if (first === "enroll" && parts.length === 2) return route("/v1/enroll", { POST: enroll });
  if (first === "whoami" && parts.length === 2) return route("/v1/whoami", { GET: whoami });
  if (first === "store" && parts.length === 2) return route("/v1/store", { GET: store });
  if (first === "installations" && parts.length === 2) return route("/v1/installations", { GET: installations });
  if (first === "installations" && parts.length === 3) {
    return route("/v1/installations/:id", { DELETE: (r, e) => revoke(r, e, decode(second)) });
  }
  if (first === "records" && parts.length === 2) return route("/v1/records", { POST: write });
  if (first === "records" && parts.length === 3) {
    return route("/v1/records/:collection", { GET: (r, e, u) => changes(r, e, u, decode(second)) });
  }
  if (first === "records" && parts.length === 4) {
    return route("/v1/records/:collection/:id", { GET: (r, e) => read(r, e, decode(second), decode(third)) });
  }
  if (first === "backups" && parts.length === 2) return route("/v1/backups", { GET: backups });
  if (first === "backups" && parts.length === 3) {
    const name = decode(second);
    return route("/v1/backups/:name", {
      PUT: (r, e) => putBackup(r, e, name),
      GET: (r, e) => getBackup(r, e, name),
      DELETE: (r, e) => deleteBackup(r, e, name),
    });
  }
  return unknown;
}

function decode(segment: string | undefined): string {
  try {
    return decodeURIComponent(segment ?? "");
  } catch {
    throw new Refusal("invalid_input", "Malformed path.");
  }
}

// ---------------------------------------------------------------------------------------------
// Identity

function now(): number {
  return Math.floor(Date.now() / 1000);
}

function registry(env: Env) {
  return env.REGISTRY.get(env.REGISTRY.idFromName("registry"));
}

function accountStore(env: Env, principal: Principal) {
  return env.ACCOUNT_STORE.get(env.ACCOUNT_STORE.idFromName(principal.account));
}

async function sha256Hex(bytes: Uint8Array | string): Promise<string> {
  const data = typeof bytes === "string" ? new TextEncoder().encode(bytes) : bytes;
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", data));
  return Array.from(digest, (b) => b.toString(16).padStart(2, "0")).join("");
}

/** The principal of a request: its Access service token identity plus its installation credential. */
export async function authenticate(request: Request, env: Env): Promise<Principal> {
  const identity = await serviceIdentity(request, env);
  const header = request.headers.get("authorization") ?? "";
  const token = /^Bearer (ptac_[A-Za-z0-9_-]{43})$/.exec(header)?.[1];
  if (!token) throw new Refusal("unauthorized", "A valid installation credential is required.");
  const principal = await registry(env).authenticate(identity, await sha256Hex(token), now());
  if (!principal) throw new Refusal("unauthorized", "A valid installation credential is required.");
  return principal;
}

function limits(env: Env): Limits {
  const parse = (value: string | undefined, fallback: number) => {
    const number = Number(value);
    return Number.isSafeInteger(number) && number > 0 ? number : fallback;
  };
  return {
    requests: parse(env.ACCOUNT_DAILY_REQUESTS, DEFAULT_DAILY_REQUESTS),
    rowsWritten: parse(env.ACCOUNT_DAILY_ROWS_WRITTEN, DEFAULT_DAILY_ROWS_WRITTEN),
  };
}

function charged<T>(result: Charged<T>): T {
  if (!result.ok) throw new Refusal("quota_exhausted", "This account's daily share of the quota is used up.");
  return result.value;
}

// ---------------------------------------------------------------------------------------------
// Bodies

async function body(request: Request, limit: number = LIMITS.bodyBytes): Promise<Record<string, unknown>> {
  if (!(request.headers.get("content-type") ?? "").startsWith("application/json")) {
    throw new Refusal("invalid_input", "The body must be JSON.");
  }
  const declared = Number(request.headers.get("content-length") ?? "0");
  if (declared > limit) throw new Refusal("payload_too_large", "The body is too large.");
  const text = await request.text();
  if (text.length > limit) throw new Refusal("payload_too_large", "The body is too large.");
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    throw new Refusal("invalid_input", "The body is not valid JSON.");
  }
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Refusal("invalid_input", "The body must be a JSON object.");
  }
  return value as Record<string, unknown>;
}

/** Closed schemas: an unknown field (an account ID, for instance) is refused, not ignored. */
function closed(value: Record<string, unknown>, allowed: string[], where: string): void {
  const extra = Object.keys(value).find((key) => !allowed.includes(key));
  if (extra !== undefined) throw new Refusal("invalid_input", `Unknown field in ${where}.`);
}

function text(value: unknown, field: string, max: number, pattern?: RegExp): string {
  if (typeof value !== "string" || value.length === 0 || value.length > max || (pattern && !pattern.test(value))) {
    throw new Refusal("invalid_input", `Invalid ${field}.`);
  }
  return value;
}

function collection(value: string): Collection {
  if (!(COLLECTIONS as readonly string[]).includes(value)) throw new Refusal("not_found", "Unknown collection.");
  return value as Collection;
}

function recordId(value: unknown): string {
  // IDs are synchronization metadata kept in clear; the host chooses them, never content.
  if (
    typeof value !== "string" ||
    value.length === 0 ||
    new TextEncoder().encode(value).byteLength > LIMITS.idBytes ||
    /\p{Cc}/u.test(value)
  ) {
    throw new Refusal("invalid_input", "Invalid record ID.");
  }
  return value;
}

function positive(value: unknown, field: string, max = Number.MAX_SAFE_INTEGER): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 1 || value > max) {
    throw new Refusal("invalid_input", `Invalid ${field}.`);
  }
  return value;
}

function base64(bytes: Uint8Array): string {
  let binary = "";
  for (let i = 0; i < bytes.length; i += 0x8000) binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(binary);
}

function fromBase64(value: unknown): Uint8Array {
  if (typeof value !== "string" || !/^[A-Za-z0-9+/]*={0,2}$/.test(value)) {
    throw new Refusal("invalid_input", "Invalid sealed record.");
  }
  const bytes = decodeBase64Url(value.replace(/=+$/, "").replace(/\+/g, "-").replace(/\//g, "_"));
  if (!bytes) throw new Refusal("invalid_input", "Invalid sealed record.");
  return bytes;
}

// ---------------------------------------------------------------------------------------------
// Routes

async function health(_request: Request, env: Env): Promise<Response> {
  return json({
    service: "personal-teams-assistant-cloud",
    environment: env.ENVIRONMENT ?? "unknown",
    version: WORKER_VERSION,
    deployment: env.VERSION?.id ?? null,
    protocols: PROTOCOLS,
    features: FEATURES,
    schema: { registry: REGISTRY_SCHEMA, account_store: ACCOUNT_STORE_SCHEMA },
    configured: accessSettings(env) !== null,
  });
}

async function enroll(request: Request, env: Env): Promise<Response> {
  const identity = await serviceIdentity(request, env);
  const input = await body(request, 4096);
  closed(input, ["label", "host_version"], "the enrollment");
  const label = text(input.label, "label", LIMITS.labelChars, /^[^\p{Cc}]+$/u);
  const hostVersion = text(input.host_version, "host version", 40, /^[0-9A-Za-z.+-]+$/);
  // 256 random bits; the registry keeps only the SHA-256.
  const secret = crypto.getRandomValues(new Uint8Array(32));
  const token = `ptac_${base64(secret).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "")}`;
  const enrolled = await registry(env).enroll(identity, await sha256Hex(token), label, hostVersion, now());
  if (!enrolled.ok) throw new Refusal("conflict", "The account has too many active installations; revoke one first.");
  return json({ installation: enrolled.installation, generation: enrolled.generation, token }, 201);
}

async function whoami(request: Request, env: Env): Promise<Response> {
  const principal = await authenticate(request, env);
  const account = await registry(env).account(principal.account);
  const installation = (await registry(env).installations(principal.account)).find(
    (entry) => entry.id === principal.installation,
  );
  return json({
    installation,
    account: { generation: account.generation, installations: account.installations },
    protocols: PROTOCOLS,
  });
}

async function installations(request: Request, env: Env): Promise<Response> {
  const principal = await authenticate(request, env);
  const list = await registry(env).installations(principal.account);
  return json({ installations: list.map((entry) => ({ ...entry, current: entry.id === principal.installation })) });
}

async function revoke(request: Request, env: Env, id: string): Promise<Response> {
  const principal = await authenticate(request, env);
  const target = id === "self" ? principal.installation : id;
  if (!(await registry(env).revoke(principal.account, target, now()))) {
    throw new Refusal("not_found", "No such installation.");
  }
  return json({ revoked: target });
}

async function store(request: Request, env: Env): Promise<Response> {
  const principal = await authenticate(request, env);
  return json(await accountStore(env, principal).summary(limits(env), now()));
}

async function write(request: Request, env: Env): Promise<Response> {
  const principal = await authenticate(request, env);
  const input = await body(request);
  closed(input, ["records", "atomic"], "the batch");
  if (input.atomic !== undefined && typeof input.atomic !== "boolean") {
    throw new Refusal("invalid_input", "Invalid atomic flag.");
  }
  if (!Array.isArray(input.records) || input.records.length === 0 || input.records.length > LIMITS.batchRecords) {
    throw new Refusal("invalid_input", `A batch holds 1 to ${LIMITS.batchRecords} records.`);
  }
  const seen = new Set<string>();
  const records: SealedRecord[] = [];
  for (const raw of input.records) {
    if (!raw || typeof raw !== "object" || Array.isArray(raw)) throw new Refusal("invalid_input", "Invalid record.");
    const entry = raw as Record<string, unknown>;
    closed(entry, ["collection", "id", "revision", "digest", "schema", "deleted", "sealed", "base_revision"], "a record");
    const record: SealedRecord = {
      collection: collection(text(entry.collection, "collection", 64)),
      id: recordId(entry.id),
      revision: positive(entry.revision, "revision"),
      digest: text(entry.digest, "digest", 64, /^[0-9a-f]{64}$/),
      schema: positive(entry.schema, "record schema", 1000),
      deleted: entry.deleted === true,
      sealed: null as Uint8Array | null,
    };
    if (entry.base_revision !== undefined) {
      const base = entry.base_revision;
      if (typeof base !== "number" || !Number.isSafeInteger(base) || base < 0 || base >= record.revision) {
        throw new Refusal("invalid_input", "A base revision is below the record's revision.");
      }
      record.base = base;
    }
    if (entry.deleted !== undefined && typeof entry.deleted !== "boolean") {
      throw new Refusal("invalid_input", "Invalid deleted flag.");
    }
    if (record.deleted) {
      if (entry.sealed !== undefined && entry.sealed !== null) {
        throw new Refusal("invalid_input", "A tombstone carries no sealed content.");
      }
      if (record.digest !== (await sha256Hex(new Uint8Array()))) {
        throw new Refusal("invalid_input", "A tombstone's digest is the digest of no bytes.");
      }
    } else {
      record.sealed = fromBase64(entry.sealed);
      if (record.sealed.byteLength === 0 || record.sealed.byteLength > LIMITS.sealedBytes) {
        throw new Refusal("payload_too_large", "A sealed record is empty or too large.");
      }
      if ((await sha256Hex(record.sealed)) !== record.digest) {
        throw new Refusal("invalid_input", "A record's digest does not match its sealed bytes.");
      }
    }
    const key = `${record.collection}\u0000${record.id}`;
    if (seen.has(key)) throw new Refusal("invalid_input", "A batch names a record twice.");
    seen.add(key);
    records.push(record);
  }
  const results = charged(
    await accountStore(env, principal).write(principal.installation, records, limits(env), now(), input.atomic === true),
  );
  return json({
    results: records.map((record, index) => ({
      collection: record.collection,
      id: record.id,
      revision: record.revision,
      result: results[index],
    })),
  });
}

function shown(record: StoredRecord) {
  return { ...record, sealed: record.sealed ? base64(record.sealed) : null };
}

async function changes(request: Request, env: Env, url: URL, name: string): Promise<Response> {
  const principal = await authenticate(request, env);
  const target = collection(name);
  const since = Number(url.searchParams.get("since") ?? "0");
  const limit = Number(url.searchParams.get("limit") ?? String(LIMITS.pageRecords));
  if (!Number.isSafeInteger(since) || since < 0 || !Number.isSafeInteger(limit) || limit < 1 || limit > LIMITS.pageRecords) {
    throw new Refusal("invalid_input", "Invalid since or limit.");
  }
  const page = charged(await accountStore(env, principal).changes(target, since, limit, limits(env), now()));
  return json({ ...page, records: page.records.map(shown) });
}

async function read(request: Request, env: Env, name: string, id: string): Promise<Response> {
  const principal = await authenticate(request, env);
  const record = charged(await accountStore(env, principal).read(collection(name), recordId(id), limits(env), now()));
  if (!record) throw new Refusal("not_found", "No such record.");
  return json(shown(record));
}

// Backups: encrypted blobs under the account's own R2 prefix, which only the principal names.

function backupKey(principal: Principal, name: string): string {
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/.test(name)) throw new Refusal("invalid_input", "Invalid backup name.");
  return `accounts/${principal.account}/backups/${name}`;
}

async function admitted(env: Env, principal: Principal): Promise<void> {
  charged(await accountStore(env, principal).admit(limits(env), now()));
}

async function backups(request: Request, env: Env): Promise<Response> {
  const principal = await authenticate(request, env);
  await admitted(env, principal);
  const prefix = `accounts/${principal.account}/backups/`;
  const listing = await env.BACKUPS.list({ prefix, limit: 1000 });
  return json({
    backups: listing.objects.map((object) => ({
      name: object.key.slice(prefix.length),
      size: object.size,
      uploaded_at: Math.floor(object.uploaded.getTime() / 1000),
      sha256: object.customMetadata?.sha256 ?? null,
    })),
    truncated: listing.truncated,
  });
}

async function putBackup(request: Request, env: Env, name: string): Promise<Response> {
  const principal = await authenticate(request, env);
  const key = backupKey(principal, name);
  const size = Number(request.headers.get("content-length") ?? "-1");
  if (!Number.isSafeInteger(size) || size < 1) throw new Refusal("invalid_input", "Content-Length is required.");
  if (size > LIMITS.backupBytes) throw new Refusal("payload_too_large", "The backup is too large.");
  const digest = request.headers.get("x-content-sha256") ?? "";
  if (!/^[0-9a-f]{64}$/.test(digest)) throw new Refusal("invalid_input", "X-Content-SHA256 is required.");
  await admitted(env, principal);
  const bytes = new Uint8Array(await request.arrayBuffer());
  if (bytes.byteLength !== size) throw new Refusal("invalid_input", "The body does not match Content-Length.");
  if ((await sha256Hex(bytes)) !== digest) throw new Refusal("invalid_input", "The body does not match its digest.");
  await env.BACKUPS.put(key, bytes, { customMetadata: { sha256: digest, installation: principal.installation } });
  return json({ name, size, sha256: digest }, 201);
}

async function getBackup(request: Request, env: Env, name: string): Promise<Response> {
  const principal = await authenticate(request, env);
  const key = backupKey(principal, name);
  await admitted(env, principal);
  const object = await env.BACKUPS.get(key);
  if (!object) throw new Refusal("not_found", "No such backup.");
  return new Response(object.body, {
    headers: {
      "content-type": "application/octet-stream",
      "cache-control": "no-store",
      "x-content-sha256": object.customMetadata?.sha256 ?? "",
    },
  });
}

async function deleteBackup(request: Request, env: Env, name: string): Promise<Response> {
  const principal = await authenticate(request, env);
  const key = backupKey(principal, name);
  await admitted(env, principal);
  if (!(await env.BACKUPS.head(key))) throw new Refusal("not_found", "No such backup.");
  await env.BACKUPS.delete(key);
  return json({ deleted: name });
}
