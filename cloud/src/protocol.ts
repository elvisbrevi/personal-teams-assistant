/** Version of this Worker's code; reported by `/v1/health`. */
export const WORKER_VERSION = "0.4.0";
/** Protocol versions served. Changes inside a version are additive only. */
export const PROTOCOLS = [1] as const;
/**
 * Additive capabilities of protocol 1 that a host checks before relying on them. `cas`: a record
 * may name the revision it replaces (`base_revision`) and a batch may be `atomic`. `erase`:
 * `DELETE /v1/store` removes the account's records and backups. `origin`: listed and read records
 * say whether the calling installation wrote them (`own`). `rebind`: `POST /v1/rebind` moves the
 * account to another Access service token. `plan`: `GET /v1/store` reports the plan's daily quota
 * the accounts share and how many accounts share it.
 */
export const FEATURES = ["cas", "erase", "origin", "rebind", "plan"] as const;
/** Schema versions of the Durable Objects; each applies forward-only migrations on start. */
export const REGISTRY_SCHEMA = 1;
export const ACCOUNT_STORE_SCHEMA = 1;

/** Collections an account store keeps. New collections are an additive change. */
export const COLLECTIONS = [
  "settings",
  "organization",
  "notes",
  "evaluations",
  "examples",
  "audits",
  "activity_snapshots",
  "activity_summaries",
  "activity_runs",
  "implementation_runs",
  "implementation_requests",
] as const;
export type Collection = (typeof COLLECTIONS)[number];

export const LIMITS = {
  /** Largest JSON request body. */
  bodyBytes: 4 * 1024 * 1024,
  /** Records per write batch. */
  batchRecords: 100,
  /** Largest sealed record (SQLite rows hold up to 2 MB). */
  sealedBytes: 1024 * 1024,
  /** Largest record ID, in UTF-8 bytes. */
  idBytes: 512,
  /** Records per listing page, and the sealed bytes a page may carry. */
  pageRecords: 500,
  pageBytes: 8 * 1024 * 1024,
  /** Active installations per account. */
  installations: 20,
  /** Largest encrypted backup. */
  backupBytes: 64 * 1024 * 1024,
  /** Label of an installation. */
  labelChars: 80,
  /** Records, and then backup objects, one erasure request removes. */
  eraseRecords: 1000,
  eraseObjects: 1000,
} as const;

/** Defaults of the daily per-account share of the plan's quota. */
export const DEFAULT_DAILY_REQUESTS = 20_000;
export const DEFAULT_DAILY_ROWS_WRITTEN = 20_000;
/** Defaults of the plan's daily quota every account shares (Workers Free: requests and rows written). */
export const DEFAULT_PLAN_DAILY_REQUESTS = 100_000;
export const DEFAULT_PLAN_DAILY_ROWS_WRITTEN = 100_000;

export type ErrorCode =
  | "unauthorized"
  | "not_found"
  | "invalid_input"
  | "conflict"
  | "payload_too_large"
  | "method_not_allowed"
  | "quota_exhausted"
  | "not_configured"
  | "internal";

const STATUS: Record<ErrorCode, number> = {
  unauthorized: 401,
  not_found: 404,
  invalid_input: 400,
  conflict: 409,
  payload_too_large: 413,
  method_not_allowed: 405,
  quota_exhausted: 429,
  not_configured: 503,
  internal: 500,
};

/** A refusal the Worker answers with `{error: {code, message}}`. Messages never carry content. */
export class Refusal extends Error {
  constructor(
    readonly code: ErrorCode,
    message: string,
  ) {
    super(message);
  }

  get status(): number {
    return STATUS[this.code];
  }
}

export function json(body: unknown, status = 200, headers: HeadersInit = {}): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json", "cache-control": "no-store", ...headers },
  });
}

export function refusal(error: Refusal, requestId: string): Response {
  return json({ error: { code: error.code, message: error.message }, request: requestId }, error.status);
}
