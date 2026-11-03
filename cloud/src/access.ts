import type { Env } from "./env";
import { Refusal } from "./protocol";

/**
 * Validation of the Cloudflare Access assertion (`Cf-Access-Jwt-Assertion`).
 *
 * The hostname is an Access application whose only policy is Service Auth, so traffic without a
 * valid service token never reaches the Worker. The Worker still checks the assertion Access adds
 * (signature with the team's keys, issuer, audience and lifetime) and accepts service token
 * identities only: the token's client ID (`common_name`) is the identity the registry binds to an
 * account.
 */

const KEYS_TTL_MS = 10 * 60 * 1000;
const SKEW_SECONDS = 60;
const ALGORITHM = { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" } as const;

interface KeySet {
  expires: number;
  keys: Map<string, CryptoKey>;
}

const keySets = new Map<string, KeySet>();

/** The configured team domain and audience, or null when the Worker is not configured yet. */
export function accessSettings(env: Env): { issuer: string; audience: string } | null {
  const domain = env.ACCESS_TEAM_DOMAIN?.trim().replace(/\/+$/, "");
  const audience = env.ACCESS_AUD?.trim();
  if (!domain || !audience) return null;
  const issuer = domain.startsWith("https://") ? domain : `https://${domain}`;
  if (!/^https:\/\/[a-z0-9-]+\.cloudflareaccess\.com$/.test(issuer)) return null;
  return { issuer, audience };
}

/** The service token client ID the request was admitted with. */
export async function serviceIdentity(request: Request, env: Env): Promise<string> {
  const settings = accessSettings(env);
  if (!settings) throw new Refusal("not_configured", "Access is not configured for this Worker.");
  const assertion = request.headers.get("cf-access-jwt-assertion");
  // A repeated header arrives joined with a comma, which a JWT never contains.
  if (!assertion || assertion.includes(",")) throw unauthorized();
  const parts = assertion.split(".");
  if (parts.length !== 3) throw unauthorized();
  const [encodedHeader, encodedPayload, encodedSignature] = parts as [string, string, string];
  const header = decodeJson(encodedHeader);
  const claims = decodeJson(encodedPayload);
  if (header.alg !== "RS256" || typeof header.kid !== "string") throw unauthorized();
  const key = await signingKey(settings.issuer, header.kid);
  const signature = decodeBase64Url(encodedSignature);
  const signed = new TextEncoder().encode(`${encodedHeader}.${encodedPayload}`);
  if (!key || !signature || !(await crypto.subtle.verify(ALGORITHM, key, signature, signed))) {
    throw unauthorized();
  }
  const now = Math.floor(Date.now() / 1000);
  const audiences = Array.isArray(claims.aud) ? claims.aud : [claims.aud];
  if (
    claims.iss !== settings.issuer ||
    !audiences.includes(settings.audience) ||
    typeof claims.exp !== "number" ||
    claims.exp < now - SKEW_SECONDS ||
    (typeof claims.nbf === "number" && claims.nbf > now + SKEW_SECONDS) ||
    (typeof claims.iat === "number" && claims.iat > now + SKEW_SECONDS)
  ) {
    throw unauthorized();
  }
  // Only service tokens: a person's session through Access carries an email, not a common name.
  const identity = claims.common_name;
  if (typeof identity !== "string" || !/^[A-Za-z0-9._-]{1,128}$/.test(identity)) throw unauthorized();
  return identity;
}

function unauthorized(): Refusal {
  return new Refusal("unauthorized", "A valid Access service token is required.");
}

async function signingKey(issuer: string, kid: string): Promise<CryptoKey | undefined> {
  const cached = keySets.get(issuer);
  if (cached && cached.expires > Date.now() && cached.keys.has(kid)) return cached.keys.get(kid);
  // An unknown key ID refreshes the set once: Access rotates its keys.
  const fresh = await fetchKeys(issuer);
  keySets.set(issuer, fresh);
  return fresh.keys.get(kid);
}

async function fetchKeys(issuer: string): Promise<KeySet> {
  const response = await fetch(`${issuer}/cdn-cgi/access/certs`, { redirect: "error" });
  if (!response.ok) throw new Refusal("internal", "The Access signing keys are unavailable.");
  const body = (await response.json()) as { keys?: unknown };
  const keys = new Map<string, CryptoKey>();
  for (const jwk of Array.isArray(body.keys) ? body.keys : []) {
    const entry = jwk as JsonWebKey & { kid?: unknown };
    if (entry.kty !== "RSA" || typeof entry.kid !== "string") continue;
    try {
      keys.set(entry.kid, await crypto.subtle.importKey("jwk", entry, ALGORITHM, false, ["verify"]));
    } catch {
      // A key this runtime cannot import is skipped; the others still verify.
    }
  }
  return { expires: Date.now() + KEYS_TTL_MS, keys };
}

function decodeJson(segment: string): Record<string, unknown> {
  const bytes = decodeBase64Url(segment);
  if (!bytes) throw unauthorized();
  try {
    const value: unknown = JSON.parse(new TextDecoder().decode(bytes));
    if (value && typeof value === "object" && !Array.isArray(value)) return value as Record<string, unknown>;
  } catch {
    // Falls through to the refusal.
  }
  throw unauthorized();
}

export function decodeBase64Url(text: string): Uint8Array | null {
  if (!/^[A-Za-z0-9_-]*$/.test(text)) return null;
  const padded = text.replace(/-/g, "+").replace(/_/g, "/") + "=".repeat((4 - (text.length % 4)) % 4);
  try {
    return Uint8Array.from(atob(padded), (c) => c.charCodeAt(0));
  } catch {
    return null;
  }
}

/** Forget cached keys (tests rotate them). */
export function forgetKeys(): void {
  keySets.clear();
}
