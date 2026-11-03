import { SELF } from "cloudflare:test";
import { vi } from "vitest";

export const ISSUER = "https://synthetic.cloudflareaccess.com";
export const AUDIENCE = "synthetic-audience";
const BASE = "https://cloud.example.test";

let signing: CryptoKeyPair | undefined;
const KID = "synthetic-key";

async function keys(): Promise<CryptoKeyPair> {
  if (!signing) {
    signing = (await crypto.subtle.generateKey(
      { name: "RSASSA-PKCS1-v1_5", modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: "SHA-256" },
      true,
      ["sign", "verify"],
    )) as CryptoKeyPair;
  }
  return signing;
}

/** Serve the synthetic team's signing keys to the Worker's JWKS fetch. */
export async function mockAccessKeys(): Promise<void> {
  const jwk = (await crypto.subtle.exportKey("jwk", (await keys()).publicKey)) as JsonWebKey;
  vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
    const url = input instanceof Request ? input.url : String(input);
    // The Workers runtime accepts only these redirect modes; anything else throws in production.
    const redirect = init?.redirect ?? (input instanceof Request ? input.redirect : "follow");
    if (!["follow", "manual"].includes(redirect)) throw new TypeError(`Unsupported redirect mode: ${redirect}`);
    if (url === `${ISSUER}/cdn-cgi/access/certs`) {
      return Response.json({ keys: [{ ...jwk, kid: KID, alg: "RS256" }] });
    }
    throw new Error(`Unexpected outbound fetch: ${url}`);
  });
}

function encode(value: unknown): string {
  const bytes = typeof value === "string" ? new TextEncoder().encode(value) : new Uint8Array(value as ArrayBuffer);
  return btoa(String.fromCharCode(...bytes)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** An Access assertion for a service token, signed by the synthetic team unless `key` says otherwise. */
export async function assertion(
  commonName: string | undefined,
  overrides: Record<string, unknown> = {},
  key?: CryptoKey,
): Promise<string> {
  const now = Math.floor(Date.now() / 1000);
  const header = encode(JSON.stringify({ alg: "RS256", kid: KID, typ: "JWT" }));
  const claims = { aud: [AUDIENCE], iss: ISSUER, iat: now, exp: now + 300, common_name: commonName, ...overrides };
  const payload = encode(JSON.stringify(claims));
  const signature = await crypto.subtle.sign(
    "RSASSA-PKCS1-v1_5",
    key ?? (await keys()).privateKey,
    new TextEncoder().encode(`${header}.${payload}`),
  );
  return `${header}.${payload}.${encode(signature)}`;
}

export interface Caller {
  identity: string;
  token?: string;
}

export async function call(
  path: string,
  options: { method?: string; caller?: Caller; body?: unknown; headers?: Record<string, string>; raw?: BodyInit } = {},
): Promise<Response> {
  const headers: Record<string, string> = { ...options.headers };
  if (options.caller) {
    headers["cf-access-jwt-assertion"] = await assertion(options.caller.identity);
    if (options.caller.token) headers.authorization = `Bearer ${options.caller.token}`;
  }
  let body: BodyInit | undefined = options.raw;
  if (options.body !== undefined) {
    headers["content-type"] = "application/json";
    body = JSON.stringify(options.body);
  }
  return SELF.fetch(`${BASE}${path}`, { method: options.method ?? "GET", headers, body });
}

/** Enroll an installation for a service token identity and return a caller holding its credential. */
export async function enrolled(identity: string, label = "synthetic host"): Promise<Caller & { installation: string }> {
  const response = await call("/v1/enroll", {
    method: "POST",
    caller: { identity },
    body: { label, host_version: "0.10.23" },
  });
  if (response.status !== 201) throw new Error(`enroll failed: ${response.status} ${await response.text()}`);
  const enrolled = (await response.json()) as { token: string; installation: string };
  return { identity, token: enrolled.token, installation: enrolled.installation };
}

export async function sealed(text: string): Promise<{ sealed: string; digest: string }> {
  const bytes = new TextEncoder().encode(text);
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
  return {
    sealed: btoa(String.fromCharCode(...bytes)),
    digest: Array.from(digest, (b) => b.toString(16).padStart(2, "0")).join(""),
  };
}

export const EMPTY_DIGEST = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
