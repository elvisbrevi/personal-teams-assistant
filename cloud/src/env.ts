import type { AccountStore } from "./account-store";
import type { Registry } from "./registry";

/** Bindings and settings of the Worker (`wrangler.jsonc`); secrets are set with `wrangler secret put`. */
export interface Env {
  REGISTRY: DurableObjectNamespace<Registry>;
  ACCOUNT_STORE: DurableObjectNamespace<AccountStore>;
  BACKUPS: R2Bucket;
  VERSION?: WorkerVersionMetadata;
  /** Secret: the Access team domain, `https://<team>.cloudflareaccess.com`. */
  ACCESS_TEAM_DOMAIN?: string;
  /** Secret: the audience tag of the Access application protecting this hostname. */
  ACCESS_AUD?: string;
  /** Daily share of one account: Worker requests that reach its store or backups. */
  ACCOUNT_DAILY_REQUESTS?: string;
  /** Daily share of one account: record rows written. */
  ACCOUNT_DAILY_ROWS_WRITTEN?: string;
  /** Environment label shown by the health check (`production` or `staging`). */
  ENVIRONMENT?: string;
}
